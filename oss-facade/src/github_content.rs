//! GitHub REST contents/trees client for `oss_repo_tree` / `oss_fetch_file`.
//!
//! The contents family is not code search: it reads exact paths from an exact
//! repo/ref. GitHub budgets it generously (5000 requests/hour authenticated,
//! 60/hour anonymous), so the client reuses the same token-bucket pacer
//! pattern as code search but paced to the contents budget. A 404 is a typed
//! `Ok(None)` (repo/ref/path genuinely absent); transport and auth failures
//! are errors the caller can surface as a degraded backend status.

use base64::Engine as _;
use serde::Deserialize;

use crate::error::{FacadeError, Result};
use crate::http::{build_url, Http, DEFAULT_TIMEOUT};
use crate::rate::RateLimiter;

pub const GITHUB_CONTENT_RPH: u32 = 5000;
/// Pacer equivalent of [`GITHUB_CONTENT_RPH`]: 5000/hour ≈ 83/minute.
pub const GITHUB_CONTENT_RPM: u32 = 83;

#[derive(Clone)]
pub struct GithubContentClient {
    http: Http,
    base: String,
    token: Option<String>,
    limiter: RateLimiter,
}

impl GithubContentClient {
    pub fn from_env() -> Self {
        Self::with_base_and_limiter(
            crate::github::GITHUB_API_BASE,
            std::env::var("GITHUB_TOKEN").ok().filter(|t| !t.is_empty()),
            RateLimiter::new(GITHUB_CONTENT_RPM, std::time::Duration::from_secs(60)),
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
        }
    }

    fn headers(&self) -> Vec<(&'static str, String)> {
        let mut headers: Vec<(&'static str, String)> = vec![
            ("Accept", "application/vnd.github+json".to_string()),
            ("X-GitHub-Api-Version", "2022-11-28".to_string()),
        ];
        if let Some(token) = &self.token {
            headers.push(("Authorization", format!("Bearer {token}")));
        }
        headers
    }

    /// Recursively list a repo tree. `ref_or_sha` may be a branch, tag, or
    /// commit SHA ("HEAD" works). `Ok(None)` = repo or ref does not exist.
    pub async fn tree(
        &self,
        owner: &str,
        repo: &str,
        ref_or_sha: &str,
        recursive: bool,
    ) -> Result<Option<GhTree>> {
        let path = format!(
            "/repos/{owner}/{repo}/git/trees/{}",
            crate::http::encode_segment(ref_or_sha)
        );
        let params: Vec<(&str, String)> = if recursive {
            vec![("recursive", "1".to_string())]
        } else {
            Vec::new()
        };
        let url = build_url(&self.base, &path, &params)?;
        self.limiter.acquire().await;
        let resp = self.http.get_once(&url, &self.headers()).await?;
        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        if !resp.status().is_success() {
            return Err(self.error_status(resp, "tree").await);
        }
        let parsed: RawTree = resp
            .json()
            .await
            .map_err(|e| FacadeError::Decode {
                backend: "github",
                detail: e.to_string(),
            })?;
        Ok(Some(GhTree {
            sha: parsed.sha.unwrap_or_default(),
            truncated: parsed.truncated.unwrap_or(false),
            entries: parsed
                .tree
                .into_iter()
                .filter_map(|e| {
                    let path = e.path?;
                    Some(GhTreeEntry {
                        path,
                        kind: e.kind.unwrap_or_else(|| "blob".into()),
                        size: e.size,
                    })
                })
                .collect(),
        }))
    }

    /// Read one file's contents (base64-decoded). `Ok(None)` = path absent
    /// at that ref. Files over 1 MB are rejected by this API; the error
    /// message says so and points at `download_url` (the raw endpoint).
    pub async fn file_contents(
        &self,
        owner: &str,
        repo: &str,
        path: &str,
        ref_: Option<&str>,
    ) -> Result<Option<GhFileContent>> {
        let api_path = format!("/repos/{owner}/{repo}/contents/{}", encode_path(path));
        let params: Vec<(&str, String)> = ref_
            .map(|r| vec![("ref", r.to_string())])
            .unwrap_or_default();
        let url = build_url(&self.base, &api_path, &params)?;
        self.limiter.acquire().await;
        let resp = self.http.get_once(&url, &self.headers()).await?;
        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        if !resp.status().is_success() {
            return Err(self.error_status(resp, "contents").await);
        }
        let parsed: RawContents = resp
            .json()
            .await
            .map_err(|e| FacadeError::Decode {
                backend: "github",
                detail: e.to_string(),
            })?;
        let content = match (&parsed.encoding, &parsed.content) {
            (Some(enc), Some(raw)) if enc.eq_ignore_ascii_case("base64") => {
                let cleaned: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
                base64::engine::general_purpose::STANDARD
                    .decode(cleaned.as_bytes())
                    .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                    .map_err(|e| FacadeError::Decode {
                        backend: "github",
                        detail: format!("contents base64 decode failed: {e}"),
                    })?
            }
            (_, Some(raw)) => raw.clone(),
            _ => String::new(),
        };
        Ok(Some(GhFileContent {
            path: parsed.path.unwrap_or_else(|| path.to_string()),
            html_url: parsed.html_url,
            size: parsed.size.unwrap_or(content.len() as u64),
            text: content,
            encoding: parsed.encoding,
        }))
    }

    async fn error_status(&self, resp: reqwest::Response, what: &str) -> FacadeError {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let detail = &body[..body.len().min(200)];
        if status.as_u16() == 401 {
            return FacadeError::GitHub(format!(
                "{what} request unauthorized (401): GITHUB_TOKEN is missing or invalid"
            ));
        }
        if status.as_u16() == 403 && body.contains("too_large") {
            return FacadeError::GitHub(format!(
                "{what} request rejected: file too large for the contents API (>1 MB); fetch via the raw download_url instead"
            ));
        }
        if status.as_u16() == 403 || status.as_u16() == 429 {
            return FacadeError::GitHub(format!(
                "{what} request rate limited ({status}): contents budget is {GITHUB_CONTENT_RPH}/hour authenticated, 60/hour anonymous; back off"
            ));
        }
        FacadeError::GitHub(format!("unexpected status {status}: {detail}"))
    }
}

fn encode_path(path: &str) -> String {
    path.split('/')
        .map(crate::http::encode_segment)
        .collect::<Vec<_>>()
        .join("/")
}

#[derive(Debug, Clone, PartialEq)]
pub struct GhTree {
    pub sha: String,
    pub truncated: bool,
    pub entries: Vec<GhTreeEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhTreeEntry {
    pub path: String,
    pub kind: String,
    pub size: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhFileContent {
    pub path: String,
    pub html_url: Option<String>,
    pub size: u64,
    pub encoding: Option<String>,
    pub text: String,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawTree {
    sha: Option<String>,
    truncated: Option<bool>,
    tree: Vec<RawTreeEntry>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawTreeEntry {
    path: Option<String>,
    kind: Option<String>,
    size: Option<u64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawContents {
    path: Option<String>,
    html_url: Option<String>,
    size: Option<u64>,
    encoding: Option<String>,
    content: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn b64(s: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(s)
    }

    #[tokio::test]
    async fn tree_parses_entries_and_truncation() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/git/trees/HEAD"))
            .and(query_param("recursive", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "sha": "abc123",
                "truncated": true,
                "tree": [
                    { "path": "src", "mode": "040000", "type": "tree", "sha": "t1" },
                    { "path": "src/main.rs", "mode": "100644", "type": "blob", "size": 120, "sha": "b1" },
                    { "path": "README.md", "mode": "100644", "type": "blob", "size": 10, "sha": "b2" }
                ]
            })))
            .mount(&server)
            .await;
        let client = GithubContentClient::with_base(server.uri(), Some("tok".into()));
        let tree = client.tree("o", "r", "HEAD", true).await.unwrap().unwrap();
        assert!(tree.truncated);
        assert_eq!(tree.entries.len(), 3);
        assert_eq!(tree.entries[1].path, "src/main.rs");
        assert_eq!(tree.entries[1].kind, "blob");
        assert_eq!(tree.entries[1].size, Some(120));
    }

    #[tokio::test]
    async fn tree_404_is_none() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/missing/git/trees/HEAD"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let client = GithubContentClient::with_base(server.uri(), None);
        assert!(client.tree("o", "missing", "HEAD", true).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn contents_decodes_base64_with_embedded_newlines() {
        let server = MockServer::start().await;
        let encoded = b64("fn main() {\n    println!(\"hi\");\n}\n");
        let pretty = encoded
            .as_bytes()
            .chunks(16)
            .map(|chunk| std::str::from_utf8(chunk).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        Mock::given(method("GET"))
            .and(path("/repos/o/r/contents/src/main.rs"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "name": "main.rs",
                "path": "src/main.rs",
                "size": encoded.len(),
                "encoding": "base64",
                "content": pretty,
                "html_url": "https://github.com/o/r/blob/main/src/main.rs"
            })))
            .mount(&server)
            .await;
        let client = GithubContentClient::with_base(server.uri(), None);
        let file = client
            .file_contents("o", "r", "src/main.rs", None)
            .await
            .unwrap()
            .unwrap();
        assert!(file.text.starts_with("fn main()"));
        assert_eq!(file.size, encoded.len() as u64);
        assert_eq!(
            file.html_url.as_deref(),
            Some("https://github.com/o/r/blob/main/src/main.rs")
        );
    }

    #[tokio::test]
    async fn contents_404_is_none_and_401_is_typed_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/contents/nope.rs"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/contents/secret.rs"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let client = GithubContentClient::with_base(server.uri(), None);
        assert!(client
            .file_contents("o", "r", "nope.rs", None)
            .await
            .unwrap()
            .is_none());
        let err = client
            .file_contents("o", "r", "secret.rs", None)
            .await
            .unwrap_err();
        assert!(matches!(err, FacadeError::GitHub(_)));
        assert!(err.to_string().contains("GITHUB_TOKEN"));
    }

    #[tokio::test]
    async fn too_large_403_names_the_raw_fallback() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/contents/big.bin"))
            .respond_with(
                ResponseTemplate::new(403)
                    .set_body_string(r#"{"message":"This file is too_large to display"}"#),
            )
            .mount(&server)
            .await;
        let client = GithubContentClient::with_base(server.uri(), None);
        let err = client
            .file_contents("o", "r", "big.bin", None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("too large"));
        assert!(err.to_string().contains("download_url"));
    }

    #[tokio::test]
    async fn ref_is_passed_as_query_param() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/o/r/contents/lib.rs"))
            .and(query_param("ref", "v1.2.3"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "path": "lib.rs", "size": 3, "encoding": "base64", "content": b64("abc")
            })))
            .mount(&server)
            .await;
        let client = GithubContentClient::with_base(server.uri(), None);
        let file = client
            .file_contents("o", "r", "lib.rs", Some("v1.2.3"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(file.text, "abc");
    }
}
