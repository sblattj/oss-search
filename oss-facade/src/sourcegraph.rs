//! Sourcegraph code search over the public streaming API
//! (`GET /.api/search/stream`, server-sent events).
//!
//! Anonymous access works (rate limited); `SOURCEGRAPH_TOKEN` raises the
//! limits (`Authorization: token <t>`). The whole request, body included, runs
//! under one total deadline because the stream may stay open.

use std::time::Duration;

use reqwest::Client;
use serde::Deserialize;
use tokio::time::Instant;

use crate::error::{FacadeError, Result};
use crate::http::{build_url, error_chain, header_value};
use crate::types::{BackendResponse, Capabilities, Filters, Hit, LineRange, Status};
use crate::BackendClient;

pub const SOURCEGRAPH_BASE: &str = "https://sourcegraph.com";
/// Total wall-clock budget for one search (connect + stream to the end).
pub const SOURCEGRAPH_DEADLINE: Duration = Duration::from_secs(20);
/// Upper bound for the `count:` cap sent to Sourcegraph.
const MAX_COUNT: u32 = 100;
/// Upper bound on the bytes read from one stream.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone)]
pub struct SourcegraphClient {
    client: Client,
    base: String,
    token: Option<String>,
    deadline: Duration,
}

impl SourcegraphClient {
    /// Production client: reads the optional `SOURCEGRAPH_TOKEN`.
    pub fn from_env() -> Self {
        Self::with_base(
            SOURCEGRAPH_BASE,
            std::env::var("SOURCEGRAPH_TOKEN").ok().filter(|t| !t.is_empty()),
        )
    }

    pub fn with_base(base: impl Into<String>, token: Option<String>) -> Self {
        Self::with_base_and_deadline(base, token, SOURCEGRAPH_DEADLINE)
    }

    pub fn with_base_and_deadline(
        base: impl Into<String>,
        token: Option<String>,
        deadline: Duration,
    ) -> Self {
        let client = Client::builder()
            // No client-level timeout: the total deadline is enforced by
            // `tokio::time::timeout` around the whole request + stream read.
            .connect_timeout(deadline)
            .user_agent(concat!("oss-facade/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("reqwest client construction cannot fail for valid config");
        Self {
            client,
            base: base.into().trim_end_matches('/').to_string(),
            token,
            deadline,
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SgMatch {
    #[serde(rename = "type")]
    kind: String,
    path: Option<String>,
    repository: Option<String>,
    commit: Option<String>,
    #[serde(rename = "lineMatches")]
    line_matches: Vec<SgLine>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SgLine {
    line: String,
    #[serde(rename = "lineNumber")]
    line_number: u64,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SgProgress {
    done: bool,
    skipped: Vec<SgSkipped>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct SgSkipped {
    reason: String,
}

/// Build the Sourcegraph query string. `count` is the match cap.
pub(crate) fn build_query(query: &str, filters: &Filters, count: u32) -> Result<String> {
    let pattern = query.trim();
    if pattern.is_empty() {
        return Err(FacadeError::InvalidQuery(
            "Sourcegraph requires a non-empty search pattern".to_string(),
        ));
    }
    let mut parts: Vec<String> = Vec::new();
    // Literal mode keeps the whole text (spaces included) as one literal
    // string; regexp mode splits on spaces, so spaces become `\s`.
    if filters.regexp {
        parts.push(pattern.split_whitespace().collect::<Vec<_>>().join("\\s"));
        parts.push("patternType:regexp".to_string());
    } else {
        parts.push(pattern.to_string());
        parts.push("patternType:literal".to_string());
    }
    parts.push("type:file".to_string());
    if let Some(v) = &filters.language {
        parts.push(format!("lang:{v}"));
    }
    if let Some(v) = &filters.repo {
        parts.push(format!("repo:{v}"));
    }
    for owner in [&filters.org, &filters.user].into_iter().flatten() {
        parts.push(format!("repo:^github\\.com/{}/", regex_escape(owner)));
    }
    if let Some(v) = &filters.path {
        parts.push(format!("file:{v}"));
    }
    if let Some(v) = &filters.filename {
        parts.push(format!("file:(^|/){}$", regex_escape(v)));
    }
    if let Some(v) = &filters.extension {
        parts.push(format!("file:\\.{}$", regex_escape(v.trim_start_matches('.'))));
    }
    parts.push(format!("count:{count}"));
    Ok(parts.join(" "))
}

fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "\\.+*?()|[]{}^$#&-~".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// One parsed SSE event.
struct SseEvent {
    name: String,
    data: String,
}

/// Parse complete SSE events out of `buf`, leaving any trailing partial
/// event in place.
fn drain_events(buf: &mut String) -> Vec<SseEvent> {
    let normalized = buf.replace("\r\n", "\n");
    let mut events = Vec::new();
    let mut rest = normalized.as_str();
    while let Some(end) = rest.find("\n\n") {
        let block = &rest[..end];
        rest = &rest[end + 2..];
        let mut name = String::new();
        let mut data = Vec::new();
        for line in block.lines() {
            if let Some(v) = line.strip_prefix("event:") {
                name = v.trim().to_string();
            } else if let Some(v) = line.strip_prefix("data:") {
                data.push(v.strip_prefix(' ').unwrap_or(v).to_string());
            }
        }
        if !name.is_empty() {
            events.push(SseEvent {
                name,
                data: data.join("\n"),
            });
        }
    }
    *buf = rest.to_string();
    events
}

fn error_message(data: &str) -> String {
    let msg = serde_json::from_str::<serde_json::Value>(data)
        .ok()
        .and_then(|v| {
            ["message", "description", "title"]
                .iter()
                .find_map(|k| v.get(*k).and_then(|m| m.as_str()).map(str::to_owned))
        })
        .unwrap_or_else(|| data.to_string());
    msg.chars().take(300).collect()
}

fn hit_from_match(m: SgMatch, base: &str) -> Option<Hit> {
    if m.kind != "content" && m.kind != "path" {
        return None;
    }
    let full_repo = m.repository?;
    let path = m.path?;
    let (repo, url) = match full_repo.strip_prefix("github.com/") {
        Some(gh) => {
            let rev = m.commit.as_deref().filter(|c| !c.is_empty()).unwrap_or("HEAD");
            (
                gh.to_string(),
                format!("https://github.com/{gh}/blob/{rev}/{path}"),
            )
        }
        None => {
            let url = format!("{base}/{full_repo}/-/blob/{path}");
            (full_repo, url)
        }
    };
    let line_ranges = m
        .line_matches
        .iter()
        .map(|l| LineRange {
            start: l.line_number,
            end: l.line_number,
        })
        .collect();
    let snippet = m
        .line_matches
        .iter()
        .map(|l| l.line.trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    Some(Hit {
        backend: "sourcegraph",
        repo: Some(repo),
        path: Some(path),
        url: Some(url),
        line_ranges,
        snippet,
        license: None,
        score: None,
    })
}

#[derive(Default)]
struct StreamState {
    hits: Vec<Hit>,
    done: bool,
    error: Option<String>,
    warnings: Vec<String>,
    match_limit: bool,
}

impl StreamState {
    fn absorb(&mut self, ev: SseEvent, base: &str) {
        match ev.name.as_str() {
            "matches" => {
                if let Ok(items) = serde_json::from_str::<Vec<serde_json::Value>>(&ev.data) {
                    for item in items {
                        if let Ok(m) = serde_json::from_value::<SgMatch>(item)
                            && let Some(h) = hit_from_match(m, base)
                        {
                            self.hits.push(h);
                        }
                    }
                }
            }
            "progress" => {
                if let Ok(p) = serde_json::from_str::<SgProgress>(&ev.data) {
                    if p.done {
                        self.done = true;
                    }
                    if p.skipped.iter().any(|s| s.reason.ends_with("limit")) {
                        self.match_limit = true;
                    }
                }
            }
            "alert" => self
                .warnings
                .push(format!("sourcegraph alert: {}", error_message(&ev.data))),
            "error" => {
                self.error
                    .get_or_insert_with(|| error_message(&ev.data));
            }
            "done" => self.done = true,
            _ => {}
        }
    }
}

#[async_trait::async_trait]
impl BackendClient for SourcegraphClient {
    async fn search_code(&self, query: &str, filters: &Filters) -> Result<BackendResponse> {
        let start = Instant::now();
        let limit = filters.limit.unwrap_or(30).clamp(1, MAX_COUNT);
        let offset = filters.offset.unwrap_or(0);
        // The stream API has no offset: over-fetch and skip.
        let count = limit.saturating_add(offset).min(MAX_COUNT);
        let q = build_query(query, filters, count)?;
        let url = build_url(
            &self.base,
            "/.api/search/stream",
            &[("q", q), ("v", "V3".to_string())],
        )?;
        let mut req = self
            .client
            .get(&url)
            .header("Accept", "text/event-stream");
        if let Some(token) = &self.token {
            req = req.header("Authorization", format!("token {token}"));
        }

        let mut state = StreamState::default();
        let base = self.base.clone();
        let run = async {
            let mut resp = req
                .send()
                .await
                .map_err(|e| FacadeError::Transport(error_chain(&e)))?;
            let status = resp.status();
            if status.as_u16() == 429 {
                let retry_after = header_value(&resp, "retry-after")
                    .and_then(|s| s.parse::<u64>().ok())
                    .map(Duration::from_secs);
                return Err(FacadeError::RateLimited { retry_after });
            }
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                return Err(FacadeError::Sourcegraph(format!(
                    "status {status}: {}",
                    body.chars().take(200).collect::<String>()
                )));
            }
            let mut pending = String::new();
            let mut raw: Vec<u8> = Vec::new();
            let mut total = 0usize;
            while let Some(chunk) = resp
                .chunk()
                .await
                .map_err(|e| FacadeError::Transport(error_chain(&e)))?
            {
                total += chunk.len();
                if total > MAX_BODY_BYTES {
                    state
                        .warnings
                        .push("sourcegraph stream exceeded the size cap; truncated".to_string());
                    break;
                }
                raw.extend_from_slice(&chunk);
                // Decode only up to the last valid UTF-8 boundary.
                let valid = match std::str::from_utf8(&raw) {
                    Ok(s) => s.len(),
                    Err(e) => e.valid_up_to(),
                };
                pending.push_str(&String::from_utf8_lossy(&raw[..valid]));
                raw.drain(..valid);
                for ev in drain_events(&mut pending) {
                    state.absorb(ev, &base);
                }
                if state.done {
                    break;
                }
            }
            Ok(())
        };
        let outcome = tokio::time::timeout(self.deadline, run).await;

        match outcome {
            Ok(Ok(())) => {}
            Ok(Err(FacadeError::RateLimited { retry_after })) => {
                return Ok(BackendResponse::unavailable(
                    format!("Sourcegraph rate limit hit (429); retry_after={retry_after:?}; set SOURCEGRAPH_TOKEN for higher limits"),
                    start.elapsed(),
                ));
            }
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                let reason = format!(
                    "Sourcegraph stream exceeded the {:?} deadline",
                    self.deadline
                );
                return Ok(finish(state, offset, limit, count, Some(reason), start));
            }
        }
        let error = state.error.take().map(|e| format!("Sourcegraph reported an error: {e}"));
        Ok(finish(state, offset, limit, count, error, start))
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            name: "sourcegraph",
            regex: true,
            language_filter: true,
            repo_filter: true,
            org_filter: true,
            path_filter: true,
            max_results: Some(u64::from(MAX_COUNT)),
            rate_limit_per_minute: None,
            auth_required: false,
            line_numbers: true,
            notes: &[
                "streaming search API (SSE); anonymous access is rate limited, SOURCEGRAPH_TOKEN raises the limits",
                "no server-side offset: pages are produced by over-fetching count=offset+limit (capped at 100)",
                "forks and archived repos are excluded by default",
            ],
        }
    }
}

/// Turn the accumulated stream state into a response. `problem` marks the
/// backend degraded when hits exist and unavailable when none do.
fn finish(
    mut state: StreamState,
    offset: u32,
    limit: u32,
    count: u32,
    problem: Option<String>,
    start: Instant,
) -> BackendResponse {
    let fetched = state.hits.len();
    let skip = (offset as usize).min(fetched);
    let mut hits: Vec<Hit> = state.hits.drain(skip..).collect();
    hits.truncate(limit as usize);
    let has_more = state.match_limit || fetched >= count as usize;
    let (status, partial) = match problem {
        None => (Status::Ok, false),
        Some(reason) if hits.is_empty() => (Status::Unavailable { reason }, false),
        Some(reason) => (Status::Degraded { reason }, true),
    };
    BackendResponse {
        total: None,
        hits,
        has_more,
        partial,
        status,
        warnings: state.warnings,
        elapsed: start.elapsed(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path, query_param_contains};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SAMPLE: &str = include_str!("../testdata/sourcegraph-stream.txt");

    fn sse(body: &str) -> ResponseTemplate {
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(body.to_string())
    }

    #[test]
    fn query_literal_with_filters() {
        let f = Filters {
            language: Some("rust".into()),
            repo: Some("tokio-rs/tokio".into()),
            path: Some("src/".into()),
            ..Filters::default()
        };
        let q = build_query("spawn worker", &f, 10).unwrap();
        assert_eq!(
            q,
            "spawn worker patternType:literal type:file lang:rust repo:tokio-rs/tokio file:src/ count:10"
        );
    }

    #[test]
    fn query_regexp_escapes_spaces_and_filters() {
        let f = Filters {
            regexp: true,
            org: Some("a.b".into()),
            extension: Some(".rs".into()),
            filename: Some("lib.rs".into()),
            ..Filters::default()
        };
        let q = build_query("use(Effect|Memo)\\( x", &f, 5).unwrap();
        assert!(q.starts_with("use(Effect|Memo)\\(\\sx patternType:regexp"), "{q}");
        assert!(q.contains("repo:^github\\.com/a\\.b/"), "{q}");
        assert!(q.contains("file:\\.rs$"), "{q}");
        assert!(q.contains("file:(^|/)lib\\.rs$"), "{q}");
        assert!(q.ends_with("count:5"), "{q}");
    }

    #[test]
    fn query_rejects_empty() {
        assert!(build_query("  ", &Filters::default(), 5).is_err());
    }

    #[tokio::test]
    async fn parses_real_sample() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/.api/search/stream"))
            .and(query_param_contains("q", "patternType:literal"))
            .and(query_param_contains("q", "count:3"))
            .respond_with(sse(SAMPLE))
            .mount(&server)
            .await;
        let client = SourcegraphClient::with_base(server.uri(), None);
        let resp = client
            .search_code(
                "useState",
                &Filters {
                    limit: Some(3),
                    ..Filters::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(resp.status, Status::Ok);
        assert_eq!(resp.hits.len(), 2);
        let first = &resp.hits[0];
        assert_eq!(first.backend, "sourcegraph");
        assert_eq!(first.repo.as_deref(), Some("sixstars/starctf2022"));
        assert!(first.path.as_deref().unwrap().ends_with("storybook/UseState.tsx"));
        assert_eq!(
            first.url.as_deref(),
            Some("https://github.com/sixstars/starctf2022/blob/64f067089fd97638cf5bcc5e679c0593d98aeffd/web-oh-my-grafana/docker/grafana-8.2.6/grafana-8.2.6/packages/grafana-ui/src/utils/storybook/UseState.tsx")
        );
        assert_eq!(first.line_ranges[0], LineRange { start: 9, end: 9 });
        assert!(first.snippet.starts_with("export class UseState<T>"));
        assert_eq!(resp.hits[1].repo.as_deref(), Some("ChrisFeldmeier/OpenCodeRust"));
    }

    #[tokio::test]
    async fn offset_skips_and_token_header_sent() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/.api/search/stream"))
            .and(query_param_contains("q", "count:4"))
            .and(header("authorization", "token sg-secret"))
            .respond_with(sse(SAMPLE))
            .mount(&server)
            .await;
        let client = SourcegraphClient::with_base(server.uri(), Some("sg-secret".into()));
        let resp = client
            .search_code(
                "useState",
                &Filters {
                    limit: Some(3),
                    offset: Some(1),
                    ..Filters::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(resp.status, Status::Ok);
        assert_eq!(resp.hits.len(), 1);
        assert_eq!(resp.hits[0].repo.as_deref(), Some("ChrisFeldmeier/OpenCodeRust"));
    }

    #[tokio::test]
    async fn rate_limit_429_is_unavailable() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/.api/search/stream"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "7"))
            .mount(&server)
            .await;
        let client = SourcegraphClient::with_base(server.uri(), None);
        let resp = client.search_code("x_y_z", &Filters::default()).await.unwrap();
        match resp.status {
            Status::Unavailable { reason } => {
                assert!(reason.contains("rate limit"), "{reason}");
                assert!(reason.contains("7s"), "{reason}");
            }
            other => panic!("expected unavailable, got {other:?}"),
        }
        assert!(resp.hits.is_empty());
    }

    #[tokio::test]
    async fn error_event_is_unavailable() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/.api/search/stream"))
            .respond_with(sse(
                "event: error\ndata: {\"title\":\"Bad query\",\"message\":\"invalid regexp\"}\n\nevent: done\ndata: {}\n\n",
            ))
            .mount(&server)
            .await;
        let client = SourcegraphClient::with_base(server.uri(), None);
        let resp = client.search_code("(", &Filters::default()).await.unwrap();
        match resp.status {
            Status::Unavailable { reason } => assert!(reason.contains("invalid regexp"), "{reason}"),
            other => panic!("expected unavailable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn http_500_is_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/.api/search/stream"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .mount(&server)
            .await;
        let client = SourcegraphClient::with_base(server.uri(), None);
        let err = client.search_code("x_y_z", &Filters::default()).await.unwrap_err();
        assert!(err.to_string().contains("500"), "{err}");
    }

    #[tokio::test]
    async fn deadline_bounds_a_stalled_stream() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/.api/search/stream"))
            .respond_with(sse(SAMPLE).set_delay(Duration::from_secs(5)))
            .mount(&server)
            .await;
        let client = SourcegraphClient::with_base_and_deadline(
            server.uri(),
            None,
            Duration::from_millis(300),
        );
        let t = std::time::Instant::now();
        let resp = client.search_code("useState", &Filters::default()).await.unwrap();
        assert!(t.elapsed() < Duration::from_secs(3), "took {:?}", t.elapsed());
        match resp.status {
            Status::Unavailable { reason } => assert!(reason.contains("deadline"), "{reason}"),
            other => panic!("expected unavailable, got {other:?}"),
        }
    }
}
