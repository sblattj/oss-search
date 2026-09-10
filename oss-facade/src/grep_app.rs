use serde::Deserialize;

use crate::error::{FacadeError, Result};
use crate::http::{build_url, header_value, is_html, Http};
use crate::types::{
    BackendResponse, Capabilities, Filters, Hit, LineRange, Status,
};
use crate::BackendClient;

pub const GREP_APP_BASE: &str = "https://grep.app";
pub const MCP_GREP_APP_BASE: &str = "https://mcp.grep.app";

#[derive(Clone)]
pub struct GrepAppClient {
    http: Http,
    base: String,
}

impl Default for GrepAppClient {
    fn default() -> Self {
        Self::new()
    }
}

impl GrepAppClient {
    pub fn new() -> Self {
        Self::with_base(GREP_APP_BASE)
    }

    pub fn with_base(base: impl Into<String>) -> Self {
        Self {
            http: Http::new(),
            base: base.into(),
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct LegacySearchResponse {
    hits: LegacyHitsOuter,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct LegacyHitsOuter {
    total: Option<u64>,
    hits: Vec<LegacyHit>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct LegacyHit {
    content: LegacyContent,
    score: Option<f64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct LegacyContent {
    snippet: Option<String>,
    repo: Option<String>,
    path: Option<String>,
    license: Option<String>,
    branch: Option<String>,
}

pub fn clean_snippet(raw: &str) -> String {
    raw.replace("<mark>", "")
        .replace("</mark>", "")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

#[async_trait::async_trait]
impl BackendClient for GrepAppClient {
    async fn search_code(&self, query: &str, filters: &Filters) -> Result<BackendResponse> {
        let start = tokio::time::Instant::now();
        let from = filters.offset.unwrap_or(0).to_string();
        let params = [
            ("q", query.to_string()),
            (
                "regexp",
                if filters.regexp { "true" } else { "false" }.to_string(),
            ),
            ("from", from),
        ];
        let extras = [
            filters.language.as_ref().map(|v| ("filter[lang][0]", v.clone())),
            filters
                .repo
                .as_ref()
                .map(|v| ("filter[repo.name][0]", v.clone())),
            filters
                .path
                .as_ref()
                .map(|v| ("filter[path.case][0]", v.clone())),
        ];
        let mut all: Vec<(&str, String)> = params.to_vec();
        for (k, v) in extras.into_iter().flatten() {
            all.push((k, v));
        }
        let url = build_url(&self.base, "/api/search", &all)?;
        let resp = match self.http.get(&url, &[]).await {
            Ok(resp) => resp,
            Err(FacadeError::RateLimited { retry_after }) => {
                return Ok(BackendResponse::unavailable(
                    format!(
                        "grep.app legacy endpoint rate limited or bot-challenged (429, retry_after={retry_after:?}); the endpoint is undocumented and gated by Vercel; prefer McpGrepAppClient at {MCP_GREP_APP_BASE}"
                    ),
                    start.elapsed(),
                ));
            }
            Err(e) => return Err(e),
        };
        if is_html(&resp) {
            return Ok(BackendResponse::unavailable(
                "grep.app legacy endpoint returned an HTML bot-challenge page; prefer McpGrepAppClient",
                start.elapsed(),
            ));
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(FacadeError::GrepApp(format!(
                "status {status}: {}",
                truncate(&body, 200)
            )));
        }
        let parsed: LegacySearchResponse = resp
            .json()
            .await
            .map_err(|e| FacadeError::Decode {
                backend: "grep.app",
                detail: e.to_string(),
            })?;
        let hits: Vec<Hit> = parsed
            .hits
            .hits
            .into_iter()
            .map(|h| Hit {
                backend: "grep.app",
                repo: h.content.repo,
                path: h.content.path,
                url: None,
                line_ranges: Vec::new(),
                snippet: clean_snippet(h.content.snippet.as_deref().unwrap_or_default()),
                license: h.content.license,
                score: h.score,
            })
            .collect();
        let total = parsed.hits.total;
        let shown = u64::from(filters.offset.unwrap_or(0)) + hits.len() as u64;
        let has_more = total.is_some_and(|t| shown < t);
        Ok(BackendResponse {
            hits,
            total,
            has_more,
            partial: false,
            status: Status::Ok,
            warnings: Vec::new(),
            elapsed: start.elapsed(),
        })
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            name: "grep.app",
            regex: true,
            language_filter: true,
            repo_filter: true,
            org_filter: false,
            path_filter: true,
            max_results: None,
            rate_limit_per_minute: None,
            auth_required: false,
            line_numbers: false,
            notes: &[
                "undocumented legacy JSON endpoint; frequently gated by Vercel bot challenges (HTTP 429)",
                "corpus is ~1M popular GitHub repositories, default branch only",
                "McpGrepAppClient is the supported preferred path",
            ],
        }
    }
}

#[derive(Clone)]
pub struct McpGrepAppClient {
    inner: std::sync::Arc<McpGrepAppInner>,
}

struct McpGrepAppInner {
    http: Http,
    base: String,
    session: tokio::sync::Mutex<Option<String>>,
    next_id: std::sync::atomic::AtomicU64,
}

impl Default for McpGrepAppClient {
    fn default() -> Self {
        Self::new()
    }
}

impl McpGrepAppClient {
    pub fn new() -> Self {
        Self::with_base(MCP_GREP_APP_BASE)
    }

    pub fn with_base(base: impl Into<String>) -> Self {
        Self {
            inner: std::sync::Arc::new(McpGrepAppInner {
                http: Http::new(),
                base: base.into(),
                session: tokio::sync::Mutex::new(None),
                next_id: std::sync::atomic::AtomicU64::new(1),
            }),
        }
    }

    fn next_id(&self) -> u64 {
        self.inner
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    async fn ensure_session(&self) -> Result<()> {
        let mut guard = self.inner.session.lock().await;
        if guard.is_some() {
            return Ok(());
        }
        let endpoint = format!("{}/", self.inner.base.trim_end_matches('/'));
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": self.next_id(),
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "oss-facade", "version": env!("CARGO_PKG_VERSION") }
            }
        });
        let resp = self.inner.http.post_json(&endpoint, &body, &[]).await?;
        if !resp.status().is_success() {
            let status = resp.status();
            return Err(FacadeError::McpGrepApp(format!(
                "initialize failed with status {status}"
            )));
        }
        let session_id = header_value(&resp, "mcp-session-id");
        if session_id.is_none() {
            return Err(FacadeError::McpGrepApp(
                "initialize response missing mcp-session-id header".to_string(),
            ));
        }
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        });
        let headers = session_id
            .clone()
            .map(|sid| vec![("mcp-session-id", sid)]);
        let notify_headers: &[(&'static str, String)] = headers.as_deref().unwrap_or(&[]);
        self.inner
            .http
            .post_json(&endpoint, &notification, notify_headers)
            .await?;
        *guard = session_id;
        Ok(())
    }

    async fn tools_call_search(
        &self,
        query: &str,
        filters: &Filters,
    ) -> std::result::Result<ToolsCallOutcome, FacadeError> {
        self.ensure_session().await?;
        let sid = self
            .inner
            .session
            .lock()
            .await
            .clone()
            .expect("session ensured above");
        let mut arguments = serde_json::json!({
            "query": query,
            "useRegexp": filters.regexp,
        });
        if let Some(language) = &filters.language {
            arguments["language"] = serde_json::json!([language]);
        }
        if let Some(repo) = &filters.repo {
            arguments["repo"] = serde_json::json!(repo);
        }
        if let Some(path) = &filters.path {
            arguments["path"] = serde_json::json!(path);
        }
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": self.next_id(),
            "method": "tools/call",
            "params": { "name": "searchGitHub", "arguments": arguments }
        });
        let endpoint = format!("{}/", self.inner.base.trim_end_matches('/'));
        let headers = vec![("mcp-session-id", sid)];
        let resp = self.inner.http.post_json_once(&endpoint, &body, &headers).await?;
        let status = resp.status();
        if status.as_u16() == 404 || status.as_u16() == 400 {
            return Ok(ToolsCallOutcome::SessionExpired);
        }
        if !status.is_success() {
            return Err(FacadeError::McpGrepApp(format!(
                "tools/call failed with status {status}"
            )));
        }
        let rpc = parse_rpc_response(resp).await?;
        if let Some(error) = rpc.error {
            return Err(FacadeError::McpGrepApp(format!(
                "json-rpc error {}: {}",
                error.code.unwrap_or_default(),
                error.message.unwrap_or_default()
            )));
        }
        let result = rpc.result.unwrap_or_default();
        if result.is_error.unwrap_or(false) {
            return Ok(ToolsCallOutcome::IsError(join_text(&result.content)));
        }
        Ok(ToolsCallOutcome::Text(join_text(&result.content)))
    }
}

enum ToolsCallOutcome {
    Text(String),
    IsError(String),
    SessionExpired,
}

fn join_text(content: &[RpcContent]) -> String {
    content
        .iter()
        .filter(|c| c.kind.as_deref().unwrap_or("text") == "text")
        .filter_map(|c| c.text.as_deref())
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RpcResponse {
    result: Option<RpcResult>,
    error: Option<RpcError>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RpcResult {
    content: Vec<RpcContent>,
    #[serde(rename = "isError")]
    is_error: Option<bool>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RpcContent {
    #[serde(rename = "type")]
    kind: Option<String>,
    text: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RpcError {
    code: Option<i64>,
    message: Option<String>,
}

async fn parse_rpc_response(resp: reqwest::Response) -> Result<RpcResponse> {
    let content_type = header_value(&resp, "content-type").unwrap_or_default();
    let text = resp
        .text()
        .await
        .map_err(|e| FacadeError::Transport(e.to_string()))?;
    if content_type.to_ascii_lowercase().contains("text/event-stream") {
        for line in text.lines() {
            if let Some(data) = line.strip_prefix("data:")
                && let Ok(parsed) = serde_json::from_str::<RpcResponse>(data.trim())
                && (parsed.result.is_some() || parsed.error.is_some())
            {
                return Ok(parsed);
            }
        }
        return Err(FacadeError::Decode {
            backend: "mcp grep.app",
            detail: "no json-rpc payload found in event-stream response".to_string(),
        });
    }
    serde_json::from_str(&text).map_err(|e| FacadeError::Decode {
        backend: "mcp grep.app",
        detail: e.to_string(),
    })
}

fn parse_github_blob_url(line: &str) -> Option<(String, String, String)> {
    let rest = line.trim().strip_prefix("https://github.com/")?;
    let mut segments = rest.trim_end_matches('/').split('/');
    let owner = segments.next()?;
    let repo = segments.next()?;
    if segments.next()? != "blob" {
        return None;
    }
    let branch = segments.next()?;
    let path = segments.collect::<Vec<_>>().join("/");
    Some((
        format!("{owner}/{repo}"),
        path.clone(),
        format!("https://github.com/{owner}/{repo}/blob/{branch}/{path}"),
    ))
}

fn parse_line_no(line: &str) -> Option<u64> {
    let trimmed = line.trim_start();
    let digit_count = trimmed
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .count();
    if digit_count == 0 {
        return None;
    }
    let digits = &trimmed[..digit_count];
    let rest = &trimmed[digit_count..];
    match rest.chars().next() {
        Some('│') | Some('|') | Some(':') | Some('\t') | Some(' ') => digits.parse().ok(),
        _ => None,
    }
}

pub fn parse_hits_from_text(text: &str) -> (Vec<Hit>, usize) {
    struct Partial {
        repo: String,
        path: String,
        url: String,
        license: Option<String>,
        snippet: String,
        line_numbers: Vec<u64>,
    }
    let mut hits: Vec<Hit> = Vec::new();
    let mut current: Option<Partial> = None;
    let mut unparsed = 0usize;
    let flush = |current: &mut Option<Partial>, hits: &mut Vec<Hit>| {
        if let Some(p) = current.take() {
            hits.push(Hit {
                backend: "mcp-grep-app",
                repo: Some(p.repo),
                path: Some(p.path),
                url: Some(p.url),
                line_ranges: p
                    .line_numbers
                    .iter()
                    .copied()
                    .min()
                    .zip(p.line_numbers.iter().copied().max())
                    .map(|(start, end)| vec![LineRange { start, end }])
                    .unwrap_or_default(),
                snippet: p.snippet.trim().to_string(),
                license: p
                    .license
                    .filter(|l| !l.eq_ignore_ascii_case("unknown")),
                score: None,
            });
        }
    };
    for line in text.lines() {
        if let Some((repo, path, url)) = parse_github_blob_url(line) {
            flush(&mut current, &mut hits);
            current = Some(Partial {
                repo,
                path,
                url,
                license: None,
                snippet: String::new(),
                line_numbers: Vec::new(),
            });
            continue;
        }
        if let Some(license) = line.trim().strip_prefix("License:") {
            if let Some(p) = current.as_mut() {
                p.license = Some(license.trim().to_string());
            } else {
                unparsed += 1;
            }
            continue;
        }
        if line.trim().is_empty() {
            continue;
        }
        if let Some(p) = current.as_mut() {
            p.snippet.push_str(line);
            p.snippet.push('\n');
            if let Some(n) = parse_line_no(line) {
                p.line_numbers.push(n);
            }
        } else {
            unparsed += 1;
        }
    }
    flush(&mut current, &mut hits);
    (hits, unparsed)
}

#[async_trait::async_trait]
impl BackendClient for McpGrepAppClient {
    async fn search_code(&self, query: &str, filters: &Filters) -> Result<BackendResponse> {
        let start = tokio::time::Instant::now();
        let attempt = self.tools_call_search(query, filters).await;
        let outcome = match attempt {
            Ok(ToolsCallOutcome::SessionExpired) => {
                *self.inner.session.lock().await = None;
                match self.tools_call_search(query, filters).await {
                    Ok(ToolsCallOutcome::SessionExpired) => {
                        return Err(FacadeError::McpGrepApp(
                            "session expired twice; server rejected re-initialization".to_string(),
                        ));
                    }
                    other => other?,
                }
            }
            Ok(other) => other,
            Err(FacadeError::RateLimited { retry_after }) => {
                return Ok(BackendResponse::unavailable(
                    format!(
                        "mcp.grep.app rate limited (429, retry_after={retry_after:?})"
                    ),
                    start.elapsed(),
                ));
            }
            Err(e) => return Err(e),
        };
        match outcome {
            ToolsCallOutcome::IsError(text) => Ok(BackendResponse::degraded(
                format!(
                    "mcp.grep.app searchGitHub returned an error: {}",
                    truncate(&text, 300)
                ),
                start.elapsed(),
            )),
            ToolsCallOutcome::Text(text) => {
                let (hits, unparsed) = parse_hits_from_text(&text);
                let mut warnings = Vec::new();
                if unparsed > 0 && hits.is_empty() {
                    return Ok(BackendResponse {
                        status: Status::Degraded {
                            reason: format!(
                                "unrecognized mcp.grep.app response format: {unparsed} unparsed lines and no hits"
                            ),
                        },
                        warnings: vec![truncate(&text, 300)],
                        hits,
                        total: None,
                        has_more: false,
                        partial: false,
                        elapsed: start.elapsed(),
                    });
                }
                if unparsed > 0 {
                    warnings.push(format!(
                        "{unparsed} lines of the mcp.grep.app response could not be attributed to a hit"
                    ));
                }
                Ok(BackendResponse {
                    hits,
                    total: None,
                    has_more: false,
                    partial: false,
                    status: Status::Ok,
                    warnings,
                    elapsed: start.elapsed(),
                })
            }
            ToolsCallOutcome::SessionExpired => unreachable!("handled above"),
        }
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            name: "mcp-grep-app",
            regex: true,
            language_filter: true,
            repo_filter: true,
            org_filter: false,
            path_filter: true,
            max_results: None,
            rate_limit_per_minute: None,
            auth_required: false,
            line_numbers: true,
            notes: &[
                "official supported grep.app interface; preferred over the legacy endpoint",
                "corpus is 1M+ pre-indexed repos plus on-demand indexing of any public repo",
                "default branch only",
            ],
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn cleans_snippet_markup() {
        assert_eq!(
            clean_snippet("a &lt;b&gt; <mark>useFetch</mark>(); &amp;&amp; &#39;q&#39;"),
            "a <b> useFetch(); && 'q'"
        );
    }

    #[test]
    fn parses_line_numbers() {
        assert_eq!(parse_line_no("  12│fn main() {"), Some(12));
        assert_eq!(parse_line_no("7:let x = 1;"), Some(7));
        assert_eq!(parse_line_no("no digits here"), None);
        assert_eq!(parse_line_no("fn main() {}"), None);
    }

    #[test]
    fn parses_github_blob_urls() {
        let (repo, path, url) =
            parse_github_blob_url("https://github.com/o/r/blob/main/src/lib.rs").unwrap();
        assert_eq!(repo, "o/r");
        assert_eq!(path, "src/lib.rs");
        assert!(url.ends_with("src/lib.rs"));
        assert!(parse_github_blob_url("https://example.com/x").is_none());
        assert!(parse_github_blob_url("https://github.com/o/r/tree/main").is_none());
    }

    #[test]
    fn parses_mcp_text_into_hits() {
        let text = "unmatched preamble line\n\
                    https://github.com/owner/repo/blob/main/src/lib.rs\n\n\
                    10│fn main() {\n\
                    11│    useFetch();\n\
                    12│}\n\n\
                    License: MIT\n\n\
                    https://github.com/o2/r2/blob/main/a.rs\n\
                    3│pub fn x() {}\n\n\
                    License: Unknown\n";
        let (hits, unparsed) = parse_hits_from_text(text);
        assert_eq!(unparsed, 1);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].repo.as_deref(), Some("owner/repo"));
        assert_eq!(hits[0].path.as_deref(), Some("src/lib.rs"));
        assert_eq!(hits[0].license.as_deref(), Some("MIT"));
        assert_eq!(hits[0].line_ranges, vec![LineRange { start: 10, end: 12 }]);
        assert!(hits[0].snippet.contains("useFetch"));
        assert_eq!(hits[1].license, None);
    }

    #[tokio::test]
    async fn legacy_parses_hits_and_totals() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "hits": {
                    "total": 42,
                    "hits": [{
                        "content": {
                            "snippet": "let x = <mark>useFetch</mark>();",
                            "repo": "owner/repo",
                            "path": "src/main.rs",
                            "branch": "main",
                            "language": "Rust",
                            "license": "MIT"
                        },
                        "score": 1.23
                    }]
                }
            })))
            .mount(&server)
            .await;
        let client = GrepAppClient::with_base(server.uri());
        let resp = client
            .search_code("useFetch", &Filters::default())
            .await
            .unwrap();
        assert_eq!(resp.status, Status::Ok);
        assert_eq!(resp.total, Some(42));
        assert!(resp.has_more);
        assert_eq!(resp.hits.len(), 1);
        assert_eq!(resp.hits[0].repo.as_deref(), Some("owner/repo"));
        assert_eq!(resp.hits[0].snippet, "let x = useFetch();");
        assert_eq!(resp.hits[0].license.as_deref(), Some("MIT"));
        assert_eq!(resp.hits[0].backend, "grep.app");
    }

    #[tokio::test]
    async fn legacy_429_is_unavailable_not_empty() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/search"))
            .respond_with(ResponseTemplate::new(429).set_body_string("Vercel Security Checkpoint"))
            .mount(&server)
            .await;
        let client = GrepAppClient::with_base(server.uri());
        let resp = client
            .search_code("useFetch", &Filters::default())
            .await
            .unwrap();
        assert!(matches!(resp.status, Status::Unavailable { .. }));
        assert!(resp.hits.is_empty());
        if let Status::Unavailable { reason } = resp.status {
            assert!(reason.contains("bot-challenged") || reason.contains("rate limited"));
        }
    }

    #[tokio::test]
    async fn legacy_html_challenge_is_unavailable() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/search"))
            .respond_with(ResponseTemplate::new(403).set_body_raw(
                "<html>challenge</html>".to_owned().into_bytes(),
                "text/html",
            ))
            .mount(&server)
            .await;
        let client = GrepAppClient::with_base(server.uri());
        let resp = client
            .search_code("useFetch", &Filters::default())
            .await
            .unwrap();
        assert!(matches!(resp.status, Status::Unavailable { .. }));
    }

    #[tokio::test]
    async fn mcp_initializes_then_searches() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .and(body_partial_json(json!({"method": "tools/call"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "id": 3,
                "result": {
                    "content": [{
                        "type": "text",
                        "text": "https://github.com/owner/repo/blob/main/src/lib.rs\n\n10│fn main() {\n11│    useFetch();\n12│}\n\nLicense: Apache-2.0\n"
                    }],
                    "isError": false
                }
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/"))
            .and(body_partial_json(json!({"method": "notifications/initialized"})))
            .respond_with(ResponseTemplate::new(202))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/"))
            .and(body_partial_json(json!({"method": "initialize"})))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("mcp-session-id", "sess-1")
                    .set_body_json(json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "result": { "protocolVersion": "2025-06-18", "capabilities": {} }
                    })),
            )
            .mount(&server)
            .await;
        let client = McpGrepAppClient::with_base(server.uri());
        let resp = client
            .search_code("useFetch", &Filters::default())
            .await
            .unwrap();
        assert_eq!(resp.status, Status::Ok);
        assert_eq!(resp.hits.len(), 1);
        assert_eq!(resp.hits[0].repo.as_deref(), Some("owner/repo"));
        assert_eq!(resp.hits[0].license.as_deref(), Some("Apache-2.0"));
        assert_eq!(resp.hits[0].line_ranges, vec![LineRange { start: 10, end: 12 }]);
        assert_eq!(resp.hits[0].backend, "mcp-grep-app");
        let requests = server.received_requests().await.unwrap();
        let initialize = requests
            .iter()
            .any(|r| String::from_utf8_lossy(&r.body).contains("\"initialize\""));
        assert!(initialize);
        let tools_call = requests
            .iter()
            .any(|r| String::from_utf8_lossy(&r.body).contains("searchGitHub"));
        assert!(tools_call);
        let session_headers = requests
            .iter()
            .any(|r| r.headers.get("mcp-session-id").is_some());
        assert!(session_headers);
    }

    #[tokio::test]
    async fn mcp_tool_error_is_degraded() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .and(body_partial_json(json!({"method": "tools/call"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "id": 3,
                "result": {
                    "content": [{ "type": "text", "text": "upstream search failed" }],
                    "isError": true
                }
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/"))
            .and(body_partial_json(json!({"method": "notifications/initialized"})))
            .respond_with(ResponseTemplate::new(202))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/"))
            .and(body_partial_json(json!({"method": "initialize"})))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("mcp-session-id", "sess-2")
                    .set_body_json(json!({ "jsonrpc": "2.0", "id": 1, "result": {} })),
            )
            .mount(&server)
            .await;
        let client = McpGrepAppClient::with_base(server.uri());
        let resp = client.search_code("useFetch", &Filters::default()).await.unwrap();
        assert!(matches!(resp.status, Status::Degraded { .. }));
        assert!(resp.hits.is_empty());
    }
}
