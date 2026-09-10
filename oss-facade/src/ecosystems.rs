use serde::Deserialize;

use crate::error::{FacadeError, Result};
use crate::http::{build_url, encode_segment, Http};
use crate::types::{BackendResponse, Capabilities, Filters, Hit, Status};
use crate::BackendClient;

pub const REPOS_BASE: &str = "https://repos.ecosyste.ms";
pub const PACKAGES_BASE: &str = "https://packages.ecosyste.ms";
pub const ECOSYSTEMS_RATE_LIMIT_PER_HOUR: u32 = 5000;

#[derive(Clone)]
pub struct EcosystemsClient {
    http: Http,
    repos_base: String,
    packages_base: String,
}

impl Default for EcosystemsClient {
    fn default() -> Self {
        Self::new()
    }
}

impl EcosystemsClient {
    pub fn new() -> Self {
        Self::with_bases(REPOS_BASE, PACKAGES_BASE)
    }

    pub fn with_bases(repos_base: impl Into<String>, packages_base: impl Into<String>) -> Self {
        Self {
            http: Http::new(),
            repos_base: repos_base.into(),
            packages_base: packages_base.into(),
        }
    }

    pub async fn repository(&self, owner: &str, repo: &str) -> Result<EcosystemsRepo> {
        self.repository_on("github.com", owner, repo).await
    }

    pub async fn repository_on(
        &self,
        host: &str,
        owner: &str,
        repo: &str,
    ) -> Result<EcosystemsRepo> {
        match self
            .fetch::<EcosystemsRepo>(repository_path(host, owner, repo), "repository")
            .await?
        {
            Some(meta) => Ok(meta),
            None => Err(FacadeError::Ecosystems(format!(
                "repository {host}/{owner}/{repo} not found"
            ))),
        }
    }

    pub async fn package(&self, registry: &str, name: &str) -> Result<Option<EcosystemsPackage>> {
        self.fetch::<EcosystemsPackage>(package_path(registry, name), "package")
            .await
    }

    /// Current live route for repository metadata: `lookup?url=` (the older
    /// path-style route is gone from the service). Returns `Ok(None)` on 404.
    pub async fn repository_lookup(&self, repo_url: &str) -> Result<Option<EcosystemsRepoMeta>> {
        let url = build_url(
            &self.repos_base,
            "/api/v1/repositories/lookup",
            &[("url", repo_url.to_string())],
        )?;
        let resp = match self.http.get(&url, &[]).await {
            Ok(resp) => resp,
            Err(FacadeError::RateLimited { retry_after }) => {
                return Err(FacadeError::Ecosystems(format!(
                    "ecosyste.ms rate limited (429, retry_after={retry_after:?}); the documented budget is {ECOSYSTEMS_RATE_LIMIT_PER_HOUR} requests/hour per IP per service; back off or use bulk dumps"
                )));
            }
            Err(e) => return Err(e),
        };
        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(FacadeError::Ecosystems(format!(
                "repository lookup failed with status {status}: {}",
                &body[..body.len().min(200)]
            )));
        }
        resp.json()
            .await
            .map(Some)
            .map_err(|e| FacadeError::Decode {
                backend: "ecosyste.ms",
                detail: e.to_string(),
            })
    }

    /// Current live package-metadata route (registry names are e.g.
    /// `npmjs.org`, not `npm`). Returns `Ok(None)` on 404.
    pub async fn package_metadata(
        &self,
        registry: &str,
        name: &str,
    ) -> Result<Option<EcosystemsPackageMeta>> {
        self.fetch::<EcosystemsPackageMeta>(package_path(registry, name), "package")
            .await
    }

    async fn fetch_repo_raw(&self, host: &str, owner: &str, repo: &str) -> Result<Option<EcosystemsRepo>> {
        self.fetch::<EcosystemsRepo>(repository_path(host, owner, repo), "repository")
            .await
    }

    async fn fetch_package_raw(&self, registry: &str, name: &str) -> Result<Option<EcosystemsPackage>> {
        self.fetch::<EcosystemsPackage>(package_path(registry, name), "package")
            .await
    }

    async fn fetch<T: serde::de::DeserializeOwned>(
        &self,
        path: String,
        what: &str,
    ) -> Result<Option<T>> {
        let base = if what == "repository" {
            &self.repos_base
        } else {
            &self.packages_base
        };
        let url = build_url(base, &path, &[])?;
        let resp = match self.http.get(&url, &[]).await {
            Ok(resp) => resp,
            Err(FacadeError::RateLimited { retry_after }) => {
                return Err(FacadeError::Ecosystems(format!(
                    "ecosyste.ms rate limited (429, retry_after={retry_after:?}); the documented budget is {ECOSYSTEMS_RATE_LIMIT_PER_HOUR} requests/hour per IP per service; back off or use bulk dumps"
                )));
            }
            Err(e) => return Err(e),
        };
        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(FacadeError::Ecosystems(format!(
                "{what} request failed with status {status}: {}",
                &body[..body.len().min(200)]
            )));
        }
        resp.json()
            .await
            .map(Some)
            .map_err(|e| FacadeError::Decode {
                backend: "ecosyste.ms",
                detail: e.to_string(),
            })
    }
}

fn repository_path(host: &str, owner: &str, repo: &str) -> String {
    format!(
        "/api/v1/repositories/{}/{}/{}",
        encode_segment(host),
        encode_segment(owner),
        encode_segment(repo)
    )
}

fn package_path(registry: &str, name: &str) -> String {
    format!(
        "/api/v1/registries/{}/packages/{}",
        encode_segment(registry),
        encode_segment(name)
    )
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct EcosystemsRepo {
    pub full_name: Option<String>,
    pub description: Option<String>,
    pub license: Option<String>,
    pub stars_count: Option<u64>,
    pub forks_count: Option<u64>,
    pub open_issues_count: Option<u64>,
    pub pushed_at: Option<String>,
    pub created_at: Option<String>,
    pub homepage: Option<String>,
    pub repository_url: Option<String>,
    pub archived: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct EcosystemsPackage {
    pub name: Option<String>,
    pub description: Option<String>,
    pub licenses: Option<Vec<String>>,
    pub repository_url: Option<String>,
    pub dependent_count: Option<u64>,
    pub dependent_repos_count: Option<u64>,
    pub latest_release_version: Option<String>,
}

/// Repository metadata from the live `lookup?url=` route (field names as the
/// service returns them today).
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct EcosystemsRepoMeta {
    pub full_name: Option<String>,
    pub description: Option<String>,
    pub license: Option<String>,
    pub stargazers_count: Option<u64>,
    pub forks_count: Option<u64>,
    pub open_issues_count: Option<u64>,
    pub pushed_at: Option<String>,
    pub created_at: Option<String>,
    pub homepage: Option<String>,
    pub repository_url: Option<String>,
    pub html_url: Option<String>,
    pub archived: Option<bool>,
    pub language: Option<String>,
    #[serde(default)]
    pub topics: Vec<String>,
    #[serde(default)]
    pub metadata: RepoMetadata,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct RepoMetadata {
    #[serde(default)]
    pub files: RepoFiles,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct RepoFiles {
    pub readme: Option<String>,
}

/// Package metadata from the live registries route (e.g. registry
/// `npmjs.org`).
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct EcosystemsPackageMeta {
    pub name: Option<String>,
    pub description: Option<String>,
    pub licenses: Option<String>,
    #[serde(default)]
    pub normalized_licenses: Vec<String>,
    pub repository_url: Option<String>,
    pub homepage: Option<String>,
    pub dependent_packages_count: Option<u64>,
    pub dependent_repos_count: Option<u64>,
    pub latest_release_number: Option<String>,
    pub latest_release_published_at: Option<String>,
    pub versions_count: Option<u64>,
    pub first_release_published_at: Option<String>,
}

#[async_trait::async_trait]
impl BackendClient for EcosystemsClient {
    async fn search_code(&self, query: &str, filters: &Filters) -> Result<BackendResponse> {
        let start = tokio::time::Instant::now();
        let repo_target = filters
            .repo
            .clone()
            .or_else(|| query.contains('/').then(|| query.to_string()));
        let mut warnings = Vec::new();
        let rate_limited = |detail: String, elapsed| BackendResponse {
            warnings: vec![format!(
                "ecosyste.ms budget is {ECOSYSTEMS_RATE_LIMIT_PER_HOUR} requests/hour per IP per service; back off or use bulk dumps at releases.ecosyste.ms"
            )],
            ..BackendResponse::unavailable(detail, elapsed)
        };
        let hit = if let Some(target) = repo_target {
            let (owner, repo) = target
                .split_once('/')
                .ok_or_else(|| FacadeError::InvalidQuery(format!(
                    "repository query must look like owner/name, got {target:?}"
                )))?;
            match self.fetch_repo_raw("github.com", owner, repo).await {
                Ok(Some(meta)) => meta.full_name.map(|full_name| Hit {
                    backend: "ecosyste.ms",
                    repo: Some(full_name),
                    path: None,
                    url: meta.repository_url.clone(),
                    line_ranges: Vec::new(),
                    snippet: meta.description.clone().unwrap_or_default(),
                    license: meta.license,
                    score: meta.stars_count.map(|s| s as f64),
                }),
                Ok(None) => None,
                Err(FacadeError::Ecosystems(e)) if e.contains("rate limited") => {
                    return Ok(rate_limited(e, start.elapsed()));
                }
                Err(e) => return Err(e),
            }
        } else {
            match self.fetch_package_raw("npm", query).await {
                Ok(Some(p)) => Some(Hit {
                    backend: "ecosyste.ms",
                    repo: p.name.clone(),
                    path: None,
                    url: p.repository_url.clone(),
                    line_ranges: Vec::new(),
                    snippet: p.description.clone().unwrap_or_default(),
                    license: p.licenses.clone().and_then(|mut l| l.pop()),
                    score: p.dependent_count.map(|c| c as f64),
                }),
                Ok(None) => None,
                Err(FacadeError::Ecosystems(e)) if e.contains("rate limited") => {
                    return Ok(rate_limited(e, start.elapsed()));
                }
                Err(e) => return Err(e),
            }
        };
        warnings.push(
            "ecosyste.ms is an exact registry/repo lookup, not a code search; budget is 5000 requests/hour per IP per service"
                .to_string(),
        );
        let found = hit.is_some();
        Ok(BackendResponse {
            hits: hit.into_iter().collect(),
            total: Some(found as u64),
            has_more: false,
            partial: false,
            status: Status::Ok,
            warnings,
            elapsed: start.elapsed(),
        })
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            name: "ecosyste.ms",
            regex: false,
            language_filter: false,
            repo_filter: true,
            org_filter: false,
            path_filter: false,
            max_results: Some(1),
            rate_limit_per_minute: None,
            auth_required: false,
            line_numbers: false,
            notes: &[
                "exact repository or registry-package lookup across ~70 registries",
                "5000 requests/hour per IP per service; bulk dumps at releases.ecosyste.ms for scale",
                "data license is CC BY-SA 4.0 (share-alike)",
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn repository_lookup_parses() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repositories/github.com/vercel/next.js"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "full_name": "vercel/next.js",
                "description": "The React Framework",
                "license": "MIT",
                "stars_count": 128000,
                "forks_count": 27000,
                "repository_url": "https://github.com/vercel/next.js"
            })))
            .mount(&server)
            .await;
        let client = EcosystemsClient::with_bases(server.uri(), server.uri());
        let repo = client.repository("vercel", "next.js").await.unwrap();
        assert_eq!(repo.full_name.as_deref(), Some("vercel/next.js"));
        assert_eq!(repo.stars_count, Some(128000));
    }

    #[tokio::test]
    async fn search_code_routes_slash_queries_to_repos() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repositories/github.com/o/r"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "full_name": "o/r",
                "description": "a repo",
                "license": "Apache-2.0",
                "stars_count": 42
            })))
            .mount(&server)
            .await;
        let client = EcosystemsClient::with_bases(server.uri(), server.uri());
        let resp = client.search_code("o/r", &Filters::default()).await.unwrap();
        assert_eq!(resp.status, Status::Ok);
        assert_eq!(resp.hits.len(), 1);
        assert_eq!(resp.hits[0].repo.as_deref(), Some("o/r"));
        assert_eq!(resp.hits[0].license.as_deref(), Some("Apache-2.0"));
        assert_eq!(resp.hits[0].score, Some(42.0));
    }

    #[tokio::test]
    async fn search_code_routes_bare_names_to_npm_packages() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/registries/npm/packages/react"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "name": "react",
                "description": "React is a JavaScript library",
                "licenses": ["MIT"],
                "repository_url": "https://github.com/facebook/react",
                "dependent_count": 215607
            })))
            .mount(&server)
            .await;
        let client = EcosystemsClient::with_bases(server.uri(), server.uri());
        let resp = client.search_code("react", &Filters::default()).await.unwrap();
        assert_eq!(resp.hits.len(), 1);
        assert_eq!(resp.hits[0].repo.as_deref(), Some("react"));
        assert_eq!(resp.hits[0].license.as_deref(), Some("MIT"));
    }

    #[tokio::test]
    async fn rate_limit_is_unavailable_with_warning() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/registries/npm/packages/react"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;
        let client = EcosystemsClient::with_bases(server.uri(), server.uri());
        let resp = client.search_code("react", &Filters::default()).await.unwrap();
        assert!(matches!(resp.status, Status::Unavailable { .. }));
        assert!(resp.hits.is_empty());
        assert!(resp
            .warnings
            .iter()
            .any(|w| w.contains("5000 requests/hour")));
    }

    #[tokio::test]
    async fn unknown_repo_is_ok_zero_hits() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repositories/github.com/o/missing"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let client = EcosystemsClient::with_bases(server.uri(), server.uri());
        let resp = client.search_code("o/missing", &Filters::default()).await.unwrap();
        assert_eq!(resp.status, Status::Ok);
        assert!(resp.hits.is_empty());
    }

    #[tokio::test]
    async fn lookup_parses_live_repo_metadata_shape() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repositories/lookup"))
            .and(wiremock::matchers::query_param("url", "https://github.com/tokio-rs/tokio"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "full_name": "tokio-rs/tokio",
                "description": "A runtime for writing reliable asynchronous applications with Rust.",
                "license": "mit",
                "stargazers_count": 33088,
                "forks_count": 3238,
                "open_issues_count": 180,
                "pushed_at": "2026-09-07T12:24:53.000Z",
                "archived": false,
                "language": "Rust",
                "topics": ["async", "runtime"],
                "repository_url": "https://github.com/tokio-rs/tokio",
                "html_url": "https://github.com/tokio-rs/tokio",
                "metadata": { "files": { "readme": "README.md" } }
            })))
            .mount(&server)
            .await;
        let client = EcosystemsClient::with_bases(server.uri(), server.uri());
        let meta = client
            .repository_lookup("https://github.com/tokio-rs/tokio")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(meta.full_name.as_deref(), Some("tokio-rs/tokio"));
        assert_eq!(meta.stargazers_count, Some(33088));
        assert_eq!(meta.language.as_deref(), Some("Rust"));
        assert_eq!(meta.metadata.files.readme.as_deref(), Some("README.md"));
    }

    #[tokio::test]
    async fn lookup_404_is_none() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/repositories/lookup"))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({"error": "not found"})))
            .mount(&server)
            .await;
        let client = EcosystemsClient::with_bases(server.uri(), server.uri());
        assert!(client
            .repository_lookup("https://github.com/o/missing")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn package_metadata_parses_live_shape() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/registries/npmjs.org/packages/express"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "name": "express",
                "description": "Fast, unopinionated, minimalist web framework",
                "licenses": "MIT",
                "normalized_licenses": ["MIT"],
                "repository_url": "https://github.com/expressjs/express",
                "dependent_packages_count": 93237,
                "dependent_repos_count": 1853938,
                "latest_release_number": "5.2.1",
                "latest_release_published_at": "2025-12-01T20:49:43.268Z",
                "versions_count": 288
            })))
            .mount(&server)
            .await;
        let client = EcosystemsClient::with_bases(server.uri(), server.uri());
        let meta = client
            .package_metadata("npmjs.org", "express")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(meta.dependent_packages_count, Some(93237));
        assert_eq!(meta.latest_release_number.as_deref(), Some("5.2.1"));
        assert_eq!(meta.normalized_licenses, vec!["MIT"]);
    }
}
