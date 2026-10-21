use serde::Deserialize;
use tokio::time::Instant;

use crate::error::{FacadeError, Result};
use crate::cooldown::{skipped_reason, Cooldown};
use crate::http::{build_url, header_value, is_rate_limited, rate_limit_wait, Http, DEFAULT_TIMEOUT};
use crate::rate::RateLimiter;
use crate::types::{BackendResponse, Capabilities, Filters, Hit, Status};
use crate::BackendClient;

pub const GITHUB_API_BASE: &str = "https://api.github.com";
pub const GITHUB_CODE_SEARCH_RPM: u32 = 10;
pub const GITHUB_RESULT_CAP: u64 = 1000;

const MAX_ATTEMPTS: u32 = 3;
const INITIAL_BACKOFF: std::time::Duration = std::time::Duration::from_millis(250);

#[derive(Clone)]
pub struct GitHubSearchClient {
    http: Http,
    base: String,
    token: Option<String>,
    limiter: RateLimiter,
    /// Code search and repository search are separate GitHub quotas.
    code_cooldown: Cooldown,
    repo_cooldown: Cooldown,
}

impl GitHubSearchClient {
    pub fn from_env() -> Self {
        Self::with_base_and_limiter(
            GITHUB_API_BASE,
            std::env::var("GITHUB_TOKEN").ok().filter(|t| !t.is_empty()),
            RateLimiter::new(GITHUB_CODE_SEARCH_RPM, std::time::Duration::from_secs(60)),
        )
    }

    pub fn with_base(base: impl Into<String>, token: Option<String>) -> Self {
        Self::with_base_and_limiter(base, token, RateLimiter::uncapped())
    }

    pub fn with_base_and_limiter(
        base: impl Into<String>,
        token: Option<String>,
        limiter: RateLimiter,
    ) -> Self {
        Self {
            http: Http::with_timeout(DEFAULT_TIMEOUT),
            base: base.into(),
            token,
            limiter,
            code_cooldown: Cooldown::new(),
            repo_cooldown: Cooldown::new(),
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GhSearchResponse {
    total_count: Option<u64>,
    incomplete_results: Option<bool>,
    items: Vec<GhItem>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GhItem {
    path: Option<String>,
    html_url: Option<String>,
    repository: Option<GhRepository>,
    score: Option<f64>,
    text_matches: Vec<GhTextMatch>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GhRepository {
    full_name: Option<String>,
    license: Option<GhLicense>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GhLicense {
    spdx_id: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GhTextMatch {
    fragment: Option<String>,
}

fn build_query(query: &str, filters: &Filters, warnings: &mut Vec<String>) -> Result<String> {
    let mut q = query.trim().to_string();
    if filters.regexp {
        warnings.push(
            "GitHub REST /search/code speaks legacy syntax only: no regex, symbol:, or boolean operators; the regex flag was dropped and the pattern is sent as literal terms"
                .to_string(),
        );
    }
    if let Some(inner) = q.strip_prefix('/').and_then(|rest| rest.strip_suffix('/'))
        && !inner.is_empty()
    {
        warnings.push(
            "regex delimiters stripped for GitHub legacy search; pattern sent as literal"
                .to_string(),
        );
        q = inner.to_string();
    }
    if q.is_empty() {
        return Err(FacadeError::InvalidQuery(
            "GitHub requires at least one search term (bare qualifiers are rejected)".to_string(),
        ));
    }
    let mut parts = vec![q];
    if let Some(v) = &filters.language {
        parts.push(format!("language:{v}"));
    }
    if let Some(v) = &filters.repo {
        parts.push(format!("repo:{v}"));
    }
    if let Some(v) = &filters.org {
        parts.push(format!("org:{v}"));
    }
    if let Some(v) = &filters.user {
        parts.push(format!("user:{v}"));
    }
    if let Some(v) = &filters.path {
        parts.push(format!("path:{v}"));
    }
    if let Some(v) = &filters.filename {
        parts.push(format!("filename:{v}"));
    }
    if let Some(v) = &filters.extension {
        parts.push(format!("extension:{v}"));
    }
    let joined = parts.join(" ");
    if joined.len() > 256 {
        warnings.push(format!(
            "query is {} bytes; GitHub rejects code-search queries over 256 characters",
            joined.len()
        ));
    }
    Ok(joined)
}

/// Result ordering for repository search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RepoSort {
    /// GitHub's default best-match relevance.
    #[default]
    BestMatch,
    Stars,
    Updated,
}

/// Filters for `GET /search/repositories`, mapped to GitHub qualifiers.
#[derive(Debug, Clone, Default)]
pub struct RepoSearchFilters {
    pub language: Option<String>,
    pub min_stars: Option<u64>,
    pub topic: Option<String>,
    pub sort: RepoSort,
    pub limit: Option<u32>,
}

/// One repository from GitHub repository search.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GitHubRepoHit {
    pub full_name: String,
    pub html_url: Option<String>,
    pub description: Option<String>,
    pub language: Option<String>,
    pub stars: u64,
    pub forks: u64,
    pub pushed_at: Option<String>,
    pub archived: bool,
    /// SPDX id; `NOASSERTION` is mapped to `None`.
    pub license: Option<String>,
    pub topics: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct GitHubRepoSearchResponse {
    pub hits: Vec<GitHubRepoHit>,
    pub total: Option<u64>,
    pub has_more: bool,
    pub status: Status,
    pub warnings: Vec<String>,
    pub elapsed: std::time::Duration,
}

impl GitHubRepoSearchResponse {
    fn unavailable(reason: impl Into<String>, elapsed: std::time::Duration) -> Self {
        Self {
            hits: Vec::new(),
            total: None,
            has_more: false,
            status: Status::Unavailable {
                reason: reason.into(),
            },
            warnings: Vec::new(),
            elapsed,
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GhRepoSearchResponse {
    total_count: Option<u64>,
    incomplete_results: Option<bool>,
    items: Vec<GhRepoItem>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GhRepoItem {
    full_name: Option<String>,
    html_url: Option<String>,
    description: Option<String>,
    language: Option<String>,
    stargazers_count: Option<u64>,
    forks_count: Option<u64>,
    pushed_at: Option<String>,
    archived: Option<bool>,
    license: Option<GhLicense>,
    topics: Vec<String>,
}

/// Quote a qualifier value when it contains whitespace or quotes.
fn qualifier_value(v: &str) -> String {
    let v = v.trim();
    if v.chars().any(|c| c.is_whitespace() || c == '"') {
        format!("\"{}\"", v.replace('"', ""))
    } else {
        v.to_string()
    }
}

/// Build the `q` string for repository search: free text plus qualifiers
/// (`language:`, `stars:>=N`, `topic:`).
pub fn build_repo_query(query: &str, filters: &RepoSearchFilters) -> Result<String> {
    let q = query.trim();
    if q.is_empty() {
        return Err(FacadeError::InvalidQuery(
            "GitHub requires at least one search term (bare qualifiers are rejected)".to_string(),
        ));
    }
    let mut parts = vec![q.to_string()];
    if let Some(v) = filters.language.as_deref().filter(|v| !v.trim().is_empty()) {
        parts.push(format!("language:{}", qualifier_value(v)));
    }
    if let Some(n) = filters.min_stars.filter(|n| *n > 0) {
        parts.push(format!("stars:>={n}"));
    }
    if let Some(v) = filters.topic.as_deref().filter(|v| !v.trim().is_empty()) {
        parts.push(format!("topic:{}", qualifier_value(v)));
    }
    Ok(parts.join(" "))
}

impl GitHubSearchClient {
    /// GitHub repository search (`GET /search/repositories`). Shares the
    /// client's rate limiter with code search. Rate-limit and auth failures
    /// come back as `Status::Unavailable`, never as an empty `Ok` result.
    /// Works unauthenticated (10 req/min) but a token raises it to 30.
    pub async fn search_repos(
        &self,
        query: &str,
        filters: &RepoSearchFilters,
    ) -> Result<GitHubRepoSearchResponse> {
        let start = Instant::now();
        let mut warnings = Vec::new();
        if self.token.is_none() {
            warnings.push(
                "GITHUB_TOKEN is not set: unauthenticated repository search is limited to 10 requests/min"
                    .to_string(),
            );
        }
        let q = build_repo_query(query, filters)?;
        let per_page = filters.limit.unwrap_or(30).clamp(1, 100);
        let mut params = vec![("q", q), ("per_page", per_page.to_string())];
        match filters.sort {
            RepoSort::BestMatch => {}
            RepoSort::Stars => params.push(("sort", "stars".to_string())),
            RepoSort::Updated => params.push(("sort", "updated".to_string())),
        }
        let url = build_url(&self.base, "/search/repositories", &params)?;
        let mut headers: Vec<(&'static str, String)> = vec![
            ("Accept", "application/vnd.github+json".to_string()),
            ("X-GitHub-Api-Version", "2022-11-28".to_string()),
        ];
        if let Some(token) = &self.token {
            headers.push(("Authorization", format!("Bearer {token}")));
        }
        let mut backoff = INITIAL_BACKOFF;
        let mut attempt = 0;
        loop {
            attempt += 1;
            if let Some(rem) = self.repo_cooldown.remaining() {
                return Ok(GitHubRepoSearchResponse::unavailable(
                    skipped_reason("github repository search", rem),
                    start.elapsed(),
                ));
            }
            self.limiter.acquire().await;
            let resp = match self.http.get_once(&url, &headers).await {
                Ok(resp) => resp,
                Err(FacadeError::RateLimited { retry_after }) => {
                    self.repo_cooldown.trip(retry_after);
                    return Ok(GitHubRepoSearchResponse::unavailable(
                        format!("GitHub repository-search rate limit hit; retry_after={retry_after:?}"),
                        start.elapsed(),
                    ));
                }
                Err(e) => return Err(e),
            };
            let status = resp.status();
            if status.is_success() {
                let parsed: GhRepoSearchResponse =
                    resp.json().await.map_err(|e| FacadeError::Decode {
                        backend: "github",
                        detail: e.to_string(),
                    })?;
                let partial = parsed.incomplete_results.unwrap_or(false);
                if partial {
                    warnings.push(
                        "incomplete_results=true: GitHub timed out mid-query; the result set is partial"
                            .to_string(),
                    );
                }
                let hits: Vec<GitHubRepoHit> = parsed
                    .items
                    .into_iter()
                    .filter_map(|i| {
                        Some(GitHubRepoHit {
                            full_name: i.full_name.filter(|n| !n.is_empty())?,
                            html_url: i.html_url,
                            description: i.description,
                            language: i.language,
                            stars: i.stargazers_count.unwrap_or(0),
                            forks: i.forks_count.unwrap_or(0),
                            pushed_at: i.pushed_at,
                            archived: i.archived.unwrap_or(false),
                            license: i
                                .license
                                .and_then(|l| l.spdx_id)
                                .filter(|s| s != "NOASSERTION"),
                            topics: i.topics,
                        })
                    })
                    .collect();
                let has_more = parsed
                    .total_count
                    .is_some_and(|t| t.min(GITHUB_RESULT_CAP) > hits.len() as u64);
                return Ok(GitHubRepoSearchResponse {
                    hits,
                    total: parsed.total_count,
                    has_more,
                    status: if partial {
                        Status::Degraded {
                            reason: "incomplete_results=true".to_string(),
                        }
                    } else {
                        Status::Ok
                    },
                    warnings,
                    elapsed: start.elapsed(),
                });
            }
            let code = status.as_u16();
            if code == 401 {
                return Ok(GitHubRepoSearchResponse::unavailable(
                    "GitHub rejected the request as unauthorized (401): GITHUB_TOKEN is missing or invalid",
                    start.elapsed(),
                ));
            }
            if code == 429 {
                self.repo_cooldown.trip(rate_limit_wait(resp.headers()));
                let reset = header_value(&resp, "x-ratelimit-reset");
                return Ok(GitHubRepoSearchResponse::unavailable(
                    format!(
                        "GitHub repository-search rate limit exhausted (429); x-ratelimit-reset={reset:?}"
                    ),
                    start.elapsed(),
                ));
            }
            if code == 403 {
                let remaining = header_value(&resp, "x-ratelimit-remaining");
                let reset = header_value(&resp, "x-ratelimit-reset");
                let retry = header_value(&resp, "retry-after");
                let header_limited = is_rate_limited(status, resp.headers());
                let wait = rate_limit_wait(resp.headers());
                let body = resp.text().await.unwrap_or_default();
                let limited = header_limited
                    || remaining.as_deref() == Some("0")
                    || retry.is_some()
                    || body.to_ascii_lowercase().contains("rate limit");
                if limited {
                    self.repo_cooldown.trip(wait);
                    return Ok(GitHubRepoSearchResponse::unavailable(
                        format!(
                            "GitHub repository-search rate limit exhausted (403); x-ratelimit-reset={reset:?} retry-after={retry:?}"
                        ),
                        start.elapsed(),
                    ));
                }
                return Err(FacadeError::GitHub(format!(
                    "403: {}",
                    body.chars().take(200).collect::<String>()
                )));
            }
            if status.is_server_error() && attempt < MAX_ATTEMPTS {
                tokio::time::sleep(backoff).await;
                backoff *= 2;
                continue;
            }
            if status.is_server_error() {
                return Ok(GitHubRepoSearchResponse::unavailable(
                    format!("GitHub returned {status} after {MAX_ATTEMPTS} attempts"),
                    start.elapsed(),
                ));
            }
            let body = resp.text().await.unwrap_or_default();
            return Err(FacadeError::GitHub(format!(
                "{code}: {}",
                body.chars().take(200).collect::<String>()
            )));
        }
    }
}

#[async_trait::async_trait]
impl BackendClient for GitHubSearchClient {
    async fn search_code(&self, query: &str, filters: &Filters) -> Result<BackendResponse> {
        let start = Instant::now();
        let mut warnings = Vec::new();
        if self.token.is_none() {
            warnings.push(
                "GITHUB_TOKEN is not set: GitHub code search requires authentication and unauthenticated requests will be rejected with 401"
                    .to_string(),
            );
        }
        let q = build_query(query, filters, &mut warnings)?;
        let per_page = filters.limit.unwrap_or(30).clamp(1, 100);
        let offset = filters.offset.unwrap_or(0) as u64;
        let page = offset / u64::from(per_page) + 1;
        let url = build_url(
            &self.base,
            "/search/code",
            &[
                ("q", q),
                ("per_page", per_page.to_string()),
                ("page", page.to_string()),
            ],
        )?;
        let mut headers: Vec<(&'static str, String)> = vec![
            ("Accept", "application/vnd.github.text-match+json".to_string()),
            ("X-GitHub-Api-Version", "2022-11-28".to_string()),
        ];
        if let Some(token) = &self.token {
            headers.push(("Authorization", format!("Bearer {token}")));
        }
        let mut backoff = INITIAL_BACKOFF;
        let mut attempt = 0;
        loop {
            attempt += 1;
            if let Some(rem) = self.code_cooldown.remaining() {
                return Ok(BackendResponse::unavailable(
                    skipped_reason("github code search", rem),
                    start.elapsed(),
                ));
            }
            self.limiter.acquire().await;
            let resp = match self.http.get_once(&url, &headers).await {
                Ok(resp) => resp,
                Err(FacadeError::RateLimited { retry_after }) => {
                    self.code_cooldown.trip(retry_after);
                    return Ok(BackendResponse::unavailable(
                        format!(
                            "GitHub code-search rate limit hit (client budget {} requests/min, server limit 10/min); retry_after={retry_after:?}",
                            GITHUB_CODE_SEARCH_RPM
                        ),
                        start.elapsed(),
                    ));
                }
                Err(e) => return Err(e),
            };
            let status = resp.status();
            if status.is_success() {
                let parsed: GhSearchResponse = resp
                    .json()
                    .await
                    .map_err(|e| FacadeError::Decode {
                        backend: "github",
                        detail: e.to_string(),
                    })?;
                let total = parsed.total_count;
                if let Some(t) = total
                    && t > GITHUB_RESULT_CAP
                {
                    warnings.push(format!(
                        "GitHub caps every code search at {GITHUB_RESULT_CAP} results; total_count is {t} and results beyond the cap are unreachable"
                    ));
                }
                if parsed.incomplete_results.unwrap_or(false) {
                    warnings.push(
                        "incomplete_results=true: GitHub timed out mid-query; the result set is partial"
                            .to_string(),
                    );
                }
                let effective_cap = total.map(|t| t.min(GITHUB_RESULT_CAP));
                let shown = offset + parsed.items.len() as u64;
                let has_more = effective_cap.is_some_and(|cap| shown < cap);
                let hits: Vec<Hit> = parsed
                    .items
                    .into_iter()
                    .map(|item| {
                        let snippets: Vec<String> = item
                            .text_matches
                            .iter()
                            .filter_map(|m| m.fragment.clone())
                            .collect();
                        Hit {
                            backend: "github",
                            repo: item.repository.as_ref().and_then(|r| r.full_name.clone()),
                            path: item.path.clone(),
                            url: item.html_url.clone(),
                            line_ranges: Vec::new(),
                            snippet: snippets.join("\n...\n"),
                            license: item
                                .repository
                                .as_ref()
                                .and_then(|r| r.license.as_ref())
                                .and_then(|l| l.spdx_id.clone())
                                .filter(|s| s != "NOASSERTION"),
                            score: item.score,
                        }
                    })
                    .collect();
                return Ok(BackendResponse {
                    hits,
                    total,
                    has_more,
                    partial: parsed.incomplete_results.unwrap_or(false),
                    status: Status::Ok,
                    warnings,
                    elapsed: start.elapsed(),
                });
            }
            if status.as_u16() == 401 {
                return Ok(BackendResponse::unavailable(
                    "GitHub rejected the request as unauthorized (401): GITHUB_TOKEN is missing or invalid",
                    start.elapsed(),
                ));
            }
            if status.as_u16() == 429 {
                self.code_cooldown.trip(rate_limit_wait(resp.headers()));
                let reset = header_value(&resp, "x-ratelimit-reset");
                return Ok(BackendResponse::unavailable(
                    format!(
                        "GitHub code-search rate limit exhausted (429, secondary limit); x-ratelimit-reset={reset:?}"
                    ),
                    start.elapsed(),
                ));
            }
            if status.as_u16() == 403 {
                let remaining = header_value(&resp, "x-ratelimit-remaining");
                let reset = header_value(&resp, "x-ratelimit-reset");
                if remaining.as_deref() == Some("0") || is_rate_limited(status, resp.headers()) {
                    self.code_cooldown.trip(rate_limit_wait(resp.headers()));
                    return Ok(BackendResponse::unavailable(
                        format!(
                            "GitHub code-search rate limit exhausted (403 with x-ratelimit-remaining=0); x-ratelimit-reset={reset:?}"
                        ),
                        start.elapsed(),
                    ));
                }
                let body = resp.text().await.unwrap_or_default();
                return Err(FacadeError::GitHub(format!(
                    "403: {}",
                    &body[..body.len().min(200)]
                )));
            }
            if status.as_u16() == 422 {
                let body = resp.text().await.unwrap_or_default();
                return Err(FacadeError::GitHub(format!(
                    "422 validation failed: {}",
                    &body[..body.len().min(200)]
                )));
            }
            if status.is_server_error() && attempt < MAX_ATTEMPTS {
                tokio::time::sleep(backoff).await;
                backoff *= 2;
                continue;
            }
            if status.is_server_error() {
                return Ok(BackendResponse::unavailable(
                    format!("GitHub returned {status} after {MAX_ATTEMPTS} attempts"),
                    start.elapsed(),
                ));
            }
            let body = resp.text().await.unwrap_or_default();
            return Err(FacadeError::GitHub(format!(
                "unexpected status {status}: {}",
                &body[..body.len().min(200)]
            )));
        }
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            name: "github",
            regex: false,
            language_filter: true,
            repo_filter: true,
            org_filter: true,
            path_filter: true,
            max_results: Some(GITHUB_RESULT_CAP),
            rate_limit_per_minute: Some(GITHUB_CODE_SEARCH_RPM),
            auth_required: true,
            line_numbers: false,
            notes: &[
                "legacy query syntax only: punctuation is stripped, no regex, no symbol:, no boolean operators",
                "1,000 results max per query; total_count above that is not pageable",
                "default branch only; files <384 KB; repos <500k files; queries search at most 4,000 matching repos",
                "client-side limiter enforces 10 requests/min regardless of token",
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn ok_body() -> serde_json::Value {
        json!({
            "total_count": 1,
            "incomplete_results": false,
            "items": [{
                "name": "main.rs",
                "path": "src/main.rs",
                "sha": "abc",
                "html_url": "https://github.com/o/r/blob/main/src/main.rs",
                "repository": {
                    "full_name": "o/r",
                    "license": { "spdx_id": "MIT" }
                },
                "score": 2.5,
                "text_matches": [{
                    "fragment": "fn main() {\n    useFetch();\n}",
                    "matches": [],
                    "property": "content"
                }]
            }]
        })
    }

    #[test]
    fn builds_legacy_syntax_query() {
        let mut warnings = Vec::new();
        let q = build_query(
            "useFetch",
            &Filters {
                language: Some("Rust".into()),
                repo: Some("o/r".into()),
                path: Some("src".into()),
                ..Filters::default()
            },
            &mut warnings,
        )
        .unwrap();
        assert_eq!(q, "useFetch language:Rust repo:o/r path:src");
        assert!(warnings.is_empty());
    }

    #[test]
    fn regex_requests_warn_and_literals_survive() {
        let mut warnings = Vec::new();
        let q = build_query(
            "/useFetch\\(/",
            &Filters {
                regexp: true,
                ..Filters::default()
            },
            &mut warnings,
        )
        .unwrap();
        assert_eq!(q, r"useFetch\(");
        assert_eq!(warnings.len(), 2);
        assert!(warnings[0].contains("legacy syntax"));
        assert!(warnings[1].contains("delimiters stripped"));
    }

    #[test]
    fn empty_query_is_rejected() {
        let err = build_query("", &Filters::default(), &mut Vec::new()).unwrap_err();
        assert!(matches!(err, FacadeError::InvalidQuery(_)));
    }

    #[tokio::test]
    async fn parses_hits_with_text_matches_and_license() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search/code"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .mount(&server)
            .await;
        let client = GitHubSearchClient::with_base(server.uri(), Some("tok".into()));
        let resp = client
            .search_code("useFetch", &Filters::default())
            .await
            .unwrap();
        assert_eq!(resp.status, Status::Ok);
        assert_eq!(resp.total, Some(1));
        assert!(!resp.has_more);
        assert_eq!(resp.hits.len(), 1);
        assert_eq!(resp.hits[0].repo.as_deref(), Some("o/r"));
        assert_eq!(resp.hits[0].path.as_deref(), Some("src/main.rs"));
        assert_eq!(resp.hits[0].license.as_deref(), Some("MIT"));
        assert!(resp.hits[0].snippet.contains("useFetch"));
        assert_eq!(resp.hits[0].backend, "github");
        assert!(resp.warnings.is_empty());
        assert!(resp.elapsed < Duration::from_secs(10));
    }

    #[tokio::test]
    async fn missing_token_surfaces_warning() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search/code"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "total_count": 0, "incomplete_results": false, "items": []
            })))
            .mount(&server)
            .await;
        let client = GitHubSearchClient::with_base(server.uri(), None);
        let resp = client.search_code("useFetch", &Filters::default()).await.unwrap();
        assert_eq!(resp.status, Status::Ok);
        assert_eq!(resp.warnings.len(), 1);
        assert!(resp.warnings[0].contains("GITHUB_TOKEN"));
    }

    #[tokio::test]
    async fn caps_and_partial_results_surface_as_warnings() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search/code"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "total_count": 5000,
                "incomplete_results": true,
                "items": []
            })))
            .mount(&server)
            .await;
        let client = GitHubSearchClient::with_base(server.uri(), Some("tok".into()));
        let resp = client.search_code("useFetch", &Filters::default()).await.unwrap();
        assert!(resp.partial);
        assert!(resp
            .warnings
            .iter()
            .any(|w| w.contains("caps every code search at 1000")));
        assert!(resp
            .warnings
            .iter()
            .any(|w| w.contains("incomplete_results=true")));
        assert!(resp.has_more);
    }

    #[tokio::test]
    async fn noassertion_license_becomes_none() {
        let server = MockServer::start().await;
        let mut body = ok_body();
        body["items"][0]["repository"]["license"]["spdx_id"] = json!("NOASSERTION");
        Mock::given(method("GET"))
            .and(path("/search/code"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
        let client = GitHubSearchClient::with_base(server.uri(), Some("tok".into()));
        let resp = client.search_code("useFetch", &Filters::default()).await.unwrap();
        assert_eq!(resp.hits[0].license, None);
    }

    #[tokio::test]
    async fn rate_limit_response_is_unavailable_not_empty_results() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search/code"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("x-ratelimit-reset", "1770000000")
                    .set_body_string("rate limit exceeded"),
            )
            .mount(&server)
            .await;
        let client = GitHubSearchClient::with_base(server.uri(), Some("tok".into()));
        let resp = client.search_code("useFetch", &Filters::default()).await.unwrap();
        assert!(matches!(resp.status, Status::Unavailable { .. }));
        assert!(resp.hits.is_empty());
        if let Status::Unavailable { reason } = resp.status {
            assert!(reason.contains("rate limit"));
            assert!(reason.contains("1770000000"));
        }
    }

    #[tokio::test]
    async fn exhausted_quota_403_is_unavailable() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search/code"))
            .respond_with(
                ResponseTemplate::new(403)
                    .insert_header("x-ratelimit-remaining", "0")
                    .insert_header("x-ratelimit-reset", "1770000000"),
            )
            .mount(&server)
            .await;
        let client = GitHubSearchClient::with_base(server.uri(), Some("tok".into()));
        let resp = client.search_code("useFetch", &Filters::default()).await.unwrap();
        assert!(matches!(resp.status, Status::Unavailable { .. }));
    }

    #[tokio::test]
    async fn validation_failure_is_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search/code"))
            .respond_with(ResponseTemplate::new(422).set_body_json(json!({
                "message": "Validation Failed"
            })))
            .mount(&server)
            .await;
        let client = GitHubSearchClient::with_base(server.uri(), Some("tok".into()));
        let err = client
            .search_code("useFetch", &Filters::default())
            .await
            .unwrap_err();
        assert!(matches!(err, FacadeError::GitHub(_)));
    }

    #[tokio::test]
    async fn retries_5xx_then_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search/code"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/search/code"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        let client = GitHubSearchClient::with_base(server.uri(), Some("tok".into()));
        let resp = client.search_code("useFetch", &Filters::default()).await.unwrap();
        assert_eq!(resp.status, Status::Ok);
        assert_eq!(resp.hits.len(), 1);
    }

    #[tokio::test]
    async fn persistent_5xx_is_unavailable() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search/code"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let client = GitHubSearchClient::with_base(server.uri(), Some("tok".into()));
        let resp = client.search_code("useFetch", &Filters::default()).await.unwrap();
        assert!(matches!(resp.status, Status::Unavailable { .. }));
        let received = server.received_requests().await.unwrap().len();
        assert_eq!(received, 3);
    }

    #[tokio::test(start_paused = true)]
    async fn hard_limiter_allows_at_most_10_requests_per_minute() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search/code"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "total_count": 0, "incomplete_results": false, "items": []
            })))
            .mount(&server)
            .await;
        let client = GitHubSearchClient::with_base_and_limiter(
            server.uri(),
            Some("tok".into()),
            RateLimiter::new(GITHUB_CODE_SEARCH_RPM, Duration::from_secs(60)),
        );
        let start = Instant::now();
        let mut handles = Vec::new();
        for i in 0..15 {
            let c = client.clone();
            handles.push(tokio::spawn(async move {
                c.search_code(&format!("term{i}"), &Filters::default())
                    .await
                    .unwrap();
            }));
        }
        loop {
            let received = server.received_requests().await.unwrap().len();
            if received >= 10 {
                break;
            }
            assert!(
                start.elapsed() < Duration::from_secs(59),
                "only {received} requests served at {:?}",
                start.elapsed()
            );
            tokio::time::advance(Duration::from_millis(25)).await;
            tokio::task::yield_now().await;
        }
        assert_eq!(server.received_requests().await.unwrap().len(), 10);
        assert!(start.elapsed() < Duration::from_secs(60));
        while server.received_requests().await.unwrap().len() < 15 {
            tokio::time::advance(Duration::from_millis(25)).await;
            tokio::task::yield_now().await;
        }
        assert!(start.elapsed() >= Duration::from_secs(60));
        for handle in handles {
            handle.await.unwrap();
        }
        assert_eq!(server.received_requests().await.unwrap().len(), 15);
    }

    fn repo_body() -> serde_json::Value {
        json!({
            "total_count": 2,
            "incomplete_results": false,
            "items": [
                {
                    "full_name": "johnbalvin/pyairbnb",
                    "html_url": "https://github.com/johnbalvin/pyairbnb",
                    "description": "Airbnb scraper",
                    "language": "Python",
                    "stargazers_count": 150,
                    "forks_count": 30,
                    "pushed_at": "2026-08-01T00:00:00Z",
                    "archived": false,
                    "license": { "spdx_id": "MIT" },
                    "topics": ["airbnb", "scraper"]
                },
                {
                    "full_name": "o/nolicense",
                    "license": { "spdx_id": "NOASSERTION" },
                    "archived": true
                }
            ]
        })
    }

    #[test]
    fn repo_query_maps_qualifiers() {
        let q = build_repo_query(
            "airbnb scraper",
            &RepoSearchFilters {
                language: Some("Python".into()),
                min_stars: Some(50),
                topic: Some("web scraping".into()),
                ..RepoSearchFilters::default()
            },
        )
        .unwrap();
        assert_eq!(
            q,
            "airbnb scraper language:Python stars:>=50 topic:\"web scraping\""
        );
        let plain = build_repo_query("x", &RepoSearchFilters::default()).unwrap();
        assert_eq!(plain, "x");
        assert!(build_repo_query("  ", &RepoSearchFilters::default()).is_err());
    }

    #[tokio::test]
    async fn repo_search_parses_hits_and_sends_qualifiers_and_auth() {
        use wiremock::matchers::{header, query_param};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search/repositories"))
            .and(query_param("q", "airbnb scraper language:Python stars:>=10"))
            .and(query_param("sort", "stars"))
            .and(query_param("per_page", "5"))
            .and(header("authorization", "Bearer tok"))
            .respond_with(ResponseTemplate::new(200).set_body_json(repo_body()))
            .expect(1)
            .mount(&server)
            .await;
        let client = GitHubSearchClient::with_base(server.uri(), Some("tok".into()));
        let resp = client
            .search_repos(
                "airbnb scraper",
                &RepoSearchFilters {
                    language: Some("Python".into()),
                    min_stars: Some(10),
                    sort: RepoSort::Stars,
                    limit: Some(5),
                    ..RepoSearchFilters::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(resp.status, Status::Ok);
        assert_eq!(resp.hits.len(), 2);
        let h = &resp.hits[0];
        assert_eq!(h.full_name, "johnbalvin/pyairbnb");
        assert_eq!(h.stars, 150);
        assert_eq!(h.forks, 30);
        assert_eq!(h.language.as_deref(), Some("Python"));
        assert_eq!(h.license.as_deref(), Some("MIT"));
        assert_eq!(h.pushed_at.as_deref(), Some("2026-08-01T00:00:00Z"));
        assert_eq!(h.topics, vec!["airbnb", "scraper"]);
        assert!(!h.archived);
        assert_eq!(resp.hits[1].license, None);
        assert!(resp.hits[1].archived);
        assert!(!resp.has_more);
    }

    #[tokio::test]
    async fn repo_search_without_token_warns() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search/repositories"))
            .respond_with(ResponseTemplate::new(200).set_body_json(repo_body()))
            .mount(&server)
            .await;
        let client = GitHubSearchClient::with_base(server.uri(), None);
        let resp = client
            .search_repos("x", &RepoSearchFilters::default())
            .await
            .unwrap();
        assert_eq!(resp.status, Status::Ok);
        assert!(resp.warnings.iter().any(|w| w.contains("10 requests/min")));
    }

    #[tokio::test]
    async fn repo_search_rate_limits_are_unavailable_not_empty() {
        for (code, remaining) in [(429u16, None), (403, Some("0"))] {
            let server = MockServer::start().await;
            let mut t = ResponseTemplate::new(code).insert_header("x-ratelimit-reset", "1770000000");
            if let Some(r) = remaining {
                t = t.insert_header("x-ratelimit-remaining", r);
            }
            Mock::given(method("GET"))
                .and(path("/search/repositories"))
                .respond_with(t)
                .mount(&server)
                .await;
            let client = GitHubSearchClient::with_base(server.uri(), Some("tok".into()));
            let resp = client
                .search_repos("x", &RepoSearchFilters::default())
                .await
                .unwrap();
            assert!(resp.hits.is_empty());
            match resp.status {
                Status::Unavailable { reason } => {
                    assert!(reason.contains("rate limit"), "{code}: {reason}");
                    assert!(reason.contains("1770000000"));
                }
                other => panic!("{code}: expected Unavailable, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn repo_search_secondary_403_by_body_is_unavailable() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search/repositories"))
            .respond_with(
                ResponseTemplate::new(403)
                    .set_body_string("You have exceeded a secondary rate limit."),
            )
            .mount(&server)
            .await;
        let client = GitHubSearchClient::with_base(server.uri(), Some("tok".into()));
        let resp = client
            .search_repos("x", &RepoSearchFilters::default())
            .await
            .unwrap();
        assert!(matches!(resp.status, Status::Unavailable { .. }));
    }

    #[tokio::test]
    async fn repo_search_unauthorized_and_incomplete() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search/repositories"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let client = GitHubSearchClient::with_base(server.uri(), Some("bad".into()));
        let resp = client
            .search_repos("x", &RepoSearchFilters::default())
            .await
            .unwrap();
        assert!(matches!(resp.status, Status::Unavailable { .. }));

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search/repositories"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "total_count": 5000, "incomplete_results": true,
                "items": [{"full_name": "a/b"}]
            })))
            .mount(&server)
            .await;
        let client = GitHubSearchClient::with_base(server.uri(), Some("tok".into()));
        let resp = client
            .search_repos("x", &RepoSearchFilters::default())
            .await
            .unwrap();
        assert!(matches!(resp.status, Status::Degraded { .. }));
        assert!(resp.has_more);
        assert_eq!(resp.hits.len(), 1);
    }

    fn reason_of(status: &Status) -> String {
        match status {
            Status::Unavailable { reason } => reason.clone(),
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn code_search_429_cools_down_and_skips_next_call() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search/code"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "60"))
            .expect(1)
            .mount(&server)
            .await;
        let client = GitHubSearchClient::with_base(server.uri(), Some("tok".into()));
        let first = client.search_code("q", &Filters::default()).await.unwrap();
        assert!(reason_of(&first.status).contains("rate limit"));
        let second = client.search_code("q", &Filters::default()).await.unwrap();
        assert!(reason_of(&second.status).contains("not called"));
    }

    #[tokio::test]
    async fn repo_search_429_cools_down_and_skips_next_call() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search/repositories"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "60"))
            .expect(1)
            .mount(&server)
            .await;
        let client = GitHubSearchClient::with_base(server.uri(), Some("tok".into()));
        let f = RepoSearchFilters::default();
        let first = client.search_repos("q", &f).await.unwrap();
        assert!(reason_of(&first.status).contains("rate limit"));
        let second = client.search_repos("q", &f).await.unwrap();
        assert!(reason_of(&second.status).contains("not called"));
    }

    #[tokio::test]
    async fn code_and_repo_search_cooldowns_are_independent() {
        let reset = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 900;
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search/code"))
            .respond_with(
                ResponseTemplate::new(403)
                    .insert_header("x-ratelimit-remaining", "0")
                    .insert_header("x-ratelimit-reset", reset.to_string().as_str()),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/search/repositories"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "total_count": 0, "incomplete_results": false, "items": []
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client = GitHubSearchClient::with_base(server.uri(), Some("tok".into()));
        let first = client.search_code("q", &Filters::default()).await.unwrap();
        assert!(reason_of(&first.status).contains("rate limit"));
        let repos = client
            .search_repos("q", &RepoSearchFilters::default())
            .await
            .unwrap();
        assert!(matches!(repos.status, Status::Ok), "{:?}", repos.status);
        let second = client.search_code("q", &Filters::default()).await.unwrap();
        let reason = reason_of(&second.status);
        assert!(reason.contains("not called"), "{reason}");
        assert!(reason.contains("resets in 1"), "{reason}");
    }

    #[tokio::test]
    async fn plain_403_does_not_trip_code_cooldown() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search/code"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
            .expect(2)
            .mount(&server)
            .await;
        let client = GitHubSearchClient::with_base(server.uri(), Some("tok".into()));
        assert!(client.search_code("q", &Filters::default()).await.is_err());
        assert!(client.search_code("q", &Filters::default()).await.is_err());
    }
}
