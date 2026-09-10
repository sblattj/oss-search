//! oss-live: the production [`SearchEngine`] backing the 7-tool surface with
//! real backends.
//!
//! Trait methods on [`oss_core::SearchEngine`] are synchronous, so
//! [`LiveEngine`] owns a private tokio runtime and blocks on its async
//! internals. That means LiveEngine methods must NOT be called from inside an
//! async context — wrap the call in `tokio::task::spawn_blocking` when
//! driving the engine from async code (oss-mcp does exactly that; oss-cli and
//! other sync callers can call directly).
//!
//! Backend truthfulness rules:
//! * every backend consulted for a call gets an entry in `backend_status`
//!   for that tool (recorded per call; `backend_status()` returns the most
//!   recent state);
//! * `partial` is true whenever any consulted backend degraded or was
//!   unavailable — an empty result set from a failing backend is never
//!   presented as a negative finding;
//! * backends the query plan selected but this engine cannot dispatch
//!   (local index, Sourcegraph) are reported as Unavailable rather than
//!   silently dropped.

mod code_search;
mod content;
mod docs;
mod guide;
pub mod hotset;
mod profile;
mod repo_search;
mod status;
mod util;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use oss_core::{EngineOutput, SearchEngine};
use oss_core::{BackendState, BackendStatus};
use oss_core::ToolError;
use oss_core::{
    FetchDocsParams, FetchFileParams, GuideParams, RepoProfileParams, RepoTreeParams,
    SearchCodeParams, SearchReposParams,
};
use oss_facade::{
    DepsDevClient, EcosystemsClient, GitHubSearchClient, GithubContentClient, GrepAppClient,
    NpmsClient, DEPS_DEV_BASE, GITHUB_API_BASE, GREP_APP_BASE, NPMS_BASE, PACKAGES_BASE,
    REPOS_BASE,
};
use oss_rank::Ranker;

/// Where each backend lives. All fields default to the real production
/// endpoints; tests point them at wiremock servers.
#[derive(Debug, Clone)]
pub struct LiveEngineConfig {
    pub github_api_base: String,
    pub grep_app_base: String,
    pub npms_base: String,
    pub ecosystems_repos_base: String,
    pub ecosystems_packages_base: String,
    pub deps_dev_base: String,
    pub github_token: Option<String>,
    /// Skip client-side rate-limit pacing (tests only: uncapped pacers).
    pub uncapped_limiters: bool,
}

impl Default for LiveEngineConfig {
    fn default() -> Self {
        Self {
            github_api_base: GITHUB_API_BASE.to_string(),
            grep_app_base: GREP_APP_BASE.to_string(),
            npms_base: NPMS_BASE.to_string(),
            ecosystems_repos_base: REPOS_BASE.to_string(),
            ecosystems_packages_base: PACKAGES_BASE.to_string(),
            deps_dev_base: DEPS_DEV_BASE.to_string(),
            github_token: std::env::var("GITHUB_TOKEN").ok().filter(|t| !t.is_empty()),
            uncapped_limiters: false,
        }
    }
}

impl LiveEngineConfig {
    /// Point every backend at one server (wiremock) with uncapped pacers.
    pub fn all_at(base: impl Into<String>) -> Self {
        let base = base.into();
        Self {
            github_api_base: base.clone(),
            grep_app_base: base.clone(),
            npms_base: base.clone(),
            ecosystems_repos_base: base.clone(),
            ecosystems_packages_base: base.clone(),
            deps_dev_base: base,
            github_token: Some("test-token".to_string()),
            uncapped_limiters: true,
        }
    }
}

pub(crate) struct Inner {
    pub github: GitHubSearchClient,
    pub grep_app: GrepAppClient,
    pub github_content: GithubContentClient,
    pub npms: NpmsClient,
    pub ecosystems: EcosystemsClient,
    pub deps_dev: DepsDevClient,
    pub ranker: Ranker,
    pub github_token: Option<String>,
    pub last_status: Mutex<HashMap<String, Vec<BackendStatus>>>,
    /// The engine's private runtime. Dropping a runtime blocks (joins its
    /// worker threads), which panics inside an async context — `Drop for
    /// Inner` moves the teardown to a dedicated thread instead.
    pub runtime: Option<tokio::runtime::Runtime>,
}

impl Inner {
    pub fn rt(&self) -> &tokio::runtime::Runtime {
        self.runtime.as_ref().expect("runtime present until drop")
    }

    pub fn record(&self, tool: &str, statuses: Vec<BackendStatus>) {
        if let Ok(mut map) = self.last_status.lock() {
            map.insert(tool.to_string(), statuses);
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(rt) = self.runtime.take() {
            std::thread::spawn(move || drop(rt));
        }
    }
}

/// Production [`SearchEngine`] over live backends. Clone-safe; see the crate
/// docs for the sync/async calling contract.
#[derive(Clone)]
pub struct LiveEngine {
    inner: Arc<Inner>,
}

impl LiveEngine {
    pub fn new() -> Self {
        Self::with_config(LiveEngineConfig::default())
    }

    pub fn with_config(config: LiveEngineConfig) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("live engine runtime");
        // RateLimiter::new spawns its pacer task on the ambient runtime, so
        // the rate-limited clients MUST be constructed inside our own
        // runtime's context (block_on here runs the closure on it). The
        // runtime itself is attached afterwards (Runtime is not Clone).
        let mut inner = rt.block_on(async {
            let github = if config.uncapped_limiters {
                GitHubSearchClient::with_base(&config.github_api_base, config.github_token.clone())
            } else {
                GitHubSearchClient::with_base_and_limiter(
                    &config.github_api_base,
                    config.github_token.clone(),
                    oss_facade::RateLimiter::new(
                        oss_facade::GITHUB_CODE_SEARCH_RPM,
                        std::time::Duration::from_secs(60),
                    ),
                )
            };
            let github_content = if config.uncapped_limiters {
                GithubContentClient::with_base(&config.github_api_base, config.github_token.clone())
            } else {
                GithubContentClient::with_base_and_limiter(
                    &config.github_api_base,
                    config.github_token.clone(),
                    oss_facade::RateLimiter::new(
                        oss_facade::GITHUB_CONTENT_RPM,
                        std::time::Duration::from_secs(60),
                    ),
                )
            };
            Inner {
                github,
                grep_app: GrepAppClient::with_base(&config.grep_app_base),
                github_content,
                npms: NpmsClient::with_base(&config.npms_base),
                ecosystems: EcosystemsClient::with_bases(
                    &config.ecosystems_repos_base,
                    &config.ecosystems_packages_base,
                ),
                deps_dev: DepsDevClient::with_base(&config.deps_dev_base),
                ranker: Ranker::with_default_config(),
                github_token: config.github_token,
                last_status: Mutex::new(HashMap::new()),
                runtime: None,
            }
        });
        inner.runtime = Some(rt);
        Self {
            inner: Arc::new(inner),
        }
    }

    /// True when no GITHUB_TOKEN is configured; callers use this to emit a
    /// startup warning (GitHub code search requires authentication).
    pub fn github_token_absent(&self) -> bool {
        self.inner.github_token.is_none()
    }
}

impl Default for LiveEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Statuses reported for a tool that has not been called yet in this
/// process: honest placeholders naming the backends that WOULD be consulted.
fn untouched_status(tool: &str) -> Vec<BackendStatus> {
    let backends: &[&str] = match tool {
        "oss_search_repos" => &["npms.io", "ecosyste.ms"],
        "oss_search_code" => &["github", "grep.app"],
        "oss_repo_profile" => &["ecosyste.ms", "deps.dev"],
        "oss_repo_tree" | "oss_fetch_file" => &["github-contents"],
        "oss_fetch_docs" => &["ecosyste.ms", "github-contents"],
        _ => &[],
    };
    backends
        .iter()
        .map(|b| BackendStatus {
            backend: (*b).to_string(),
            status: BackendState::Ok,
            detail: Some("no call yet; statuses are recorded per call".to_string()),
        })
        .collect()
}

impl SearchEngine for LiveEngine {
    fn backend_status(&self, tool: &str) -> Vec<BackendStatus> {
        if let Ok(map) = self.inner.last_status.lock()
            && let Some(v) = map.get(tool)
        {
            return v.clone();
        }
        untouched_status(tool)
    }

    fn search_repos(&self, params: &SearchReposParams) -> Result<EngineOutput, ToolError> {
        self.inner
            .rt()
            .block_on(repo_search::run(&self.inner, params))
    }

    fn search_code(&self, params: &SearchCodeParams) -> Result<EngineOutput, ToolError> {
        self.inner
            .rt()
            .block_on(code_search::run(&self.inner, params))
    }

    fn repo_profile(&self, params: &RepoProfileParams) -> Result<EngineOutput, ToolError> {
        self.inner
            .rt()
            .block_on(profile::run(&self.inner, params))
    }

    fn repo_tree(&self, params: &RepoTreeParams) -> Result<EngineOutput, ToolError> {
        self.inner
            .rt()
            .block_on(content::run_tree(&self.inner, params))
    }

    fn fetch_file(&self, params: &FetchFileParams) -> Result<EngineOutput, ToolError> {
        self.inner
            .rt()
            .block_on(content::run_file(&self.inner, params))
    }

    fn fetch_docs(&self, params: &FetchDocsParams) -> Result<EngineOutput, ToolError> {
        self.inner
            .rt()
            .block_on(docs::run(&self.inner, params))
    }

    fn guide(&self, _params: &GuideParams) -> Result<EngineOutput, ToolError> {
        Ok(EngineOutput::complete(vec![guide::guide_document()]))
    }
}
