use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::header::HeaderMap;
use reqwest::{Client, Method, Response, StatusCode};

use crate::cooldown::{MAX_COOLDOWN, SHORT_RETRY_MAX};
use crate::error::{FacadeError, Result};

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_ATTEMPTS: u32 = 3;
const INITIAL_BACKOFF: Duration = Duration::from_millis(250);

#[derive(Clone)]
pub struct Http {
    client: Client,
    timeout: Duration,
}

impl Default for Http {
    fn default() -> Self {
        Self::new()
    }
}

impl Http {
    pub fn new() -> Self {
        Self::with_timeout(DEFAULT_TIMEOUT)
    }

    pub fn with_timeout(timeout: Duration) -> Self {
        let client = Client::builder()
            .timeout(timeout)
            .user_agent(concat!("oss-facade/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("reqwest client construction cannot fail for valid config");
        Self { client, timeout }
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    pub async fn get(&self, url: &str, headers: &[(&'static str, String)]) -> Result<Response> {
        self.execute(Method::GET, url, None, headers, MAX_ATTEMPTS, true)
            .await
    }

    pub async fn get_once(
        &self,
        url: &str,
        headers: &[(&'static str, String)],
    ) -> Result<Response> {
        self.execute(Method::GET, url, None, headers, 1, false).await
    }

    pub async fn post_json(
        &self,
        url: &str,
        body: &serde_json::Value,
        headers: &[(&'static str, String)],
    ) -> Result<Response> {
        self.execute(Method::POST, url, Some(body), headers, MAX_ATTEMPTS, true)
            .await
    }

    pub async fn post_json_once(
        &self,
        url: &str,
        body: &serde_json::Value,
        headers: &[(&'static str, String)],
    ) -> Result<Response> {
        self.execute(Method::POST, url, Some(body), headers, 1, false)
            .await
    }

    async fn execute(
        &self,
        method: Method,
        url: &str,
        body: Option<&serde_json::Value>,
        headers: &[(&'static str, String)],
        attempts: u32,
        map_429: bool,
    ) -> Result<Response> {
        let mut backoff = INITIAL_BACKOFF;
        let mut attempt = 0;
        let mut short_retry_used = false;
        loop {
            attempt += 1;
            let mut req = self.client.request(method.clone(), url);
            for (key, value) in headers {
                req = req.header(*key, value.as_str());
            }
            if let Some(body) = body {
                req = req.json(body);
            }
            let resp = req.send().await.map_err(|e| transport_error(self, e))?;
            let status = resp.status();
            if is_rate_limited(status, resp.headers()) {
                let wait = rate_limit_wait(resp.headers());
                if !short_retry_used && wait.is_some_and(|w| w <= SHORT_RETRY_MAX) {
                    short_retry_used = true;
                    // The short retry does not consume a 5xx attempt.
                    attempt -= 1;
                    tokio::time::sleep(wait.unwrap_or_default()).await;
                    continue;
                }
                if status == StatusCode::TOO_MANY_REQUESTS && map_429 {
                    return Err(FacadeError::RateLimited { retry_after: wait });
                }
            }
            if status.is_server_error() && attempt < attempts {
                tokio::time::sleep(backoff).await;
                backoff *= 2;
                continue;
            }
            return Ok(resp);
        }
    }
}

/// True for 429, or 403 carrying `x-ratelimit-remaining: 0` or a `retry-after` header
/// (GitHub's primary and secondary limits).
pub fn is_rate_limited(status: StatusCode, headers: &HeaderMap) -> bool {
    if status == StatusCode::TOO_MANY_REQUESTS {
        return true;
    }
    status == StatusCode::FORBIDDEN
        && (header_str(headers, "x-ratelimit-remaining").is_some_and(|v| v.trim() == "0")
            || headers.contains_key("retry-after"))
}

/// The wait the server asked for, clamped to `MAX_COOLDOWN`.
pub fn rate_limit_wait(headers: &HeaderMap) -> Option<Duration> {
    rate_limit_wait_at(headers, SystemTime::now())
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

fn rate_limit_wait_at(headers: &HeaderMap, now: SystemTime) -> Option<Duration> {
    if let Some(v) = header_str(headers, "retry-after") {
        let v = v.trim();
        if let Ok(secs) = v.parse::<u64>() {
            return Some(Duration::from_secs(secs).min(MAX_COOLDOWN));
        }
        if let Ok(when) = httpdate::parse_http_date(v) {
            let wait = when.duration_since(now).unwrap_or(Duration::ZERO);
            return Some(wait.min(MAX_COOLDOWN));
        }
    }
    let remaining_ok = header_str(headers, "x-ratelimit-remaining")
        .map(|v| v.trim() == "0")
        .unwrap_or(true);
    if remaining_ok
        && let Some(reset) = header_str(headers, "x-ratelimit-reset")
            .and_then(|v| v.trim().parse::<u64>().ok())
    {
        let now_secs = now.duration_since(UNIX_EPOCH).ok()?.as_secs();
        return match reset.checked_sub(now_secs) {
            Some(d) if d > 0 => Some(Duration::from_secs(d).min(MAX_COOLDOWN)),
            _ => None,
        };
    }
    None
}

fn transport_error(http: &Http, e: reqwest::Error) -> FacadeError {
    if e.is_timeout() {
        FacadeError::Timeout(http.timeout())
    } else if e.is_decode() {
        FacadeError::Decode {
            backend: "http",
            detail: error_chain(&e),
        }
    } else {
        FacadeError::Transport(error_chain(&e))
    }
}

/// `reqwest::Error`'s Display stops at "error sending request for url (...)";
/// the actual cause (refused connection, untrusted certificate) is in `source()`.
pub fn error_chain(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut source = e.source();
    while let Some(cause) = source {
        let text = cause.to_string();
        if !out.contains(&text) {
            out.push_str(": ");
            out.push_str(&text);
        }
        source = cause.source();
    }
    out
}

pub fn header_value(resp: &Response, name: &str) -> Option<String> {
    resp.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

pub fn is_html(resp: &Response) -> bool {
    header_value(resp, "content-type")
        .is_some_and(|ct| ct.to_ascii_lowercase().contains("text/html"))
}

pub fn encode_segment(s: &str) -> String {
    s.replace('/', "%2F")
}

pub fn build_url(base: &str, path: &str, params: &[(&str, String)]) -> Result<String> {
    let mut url = reqwest::Url::parse(&format!("{base}{path}"))
        .map_err(|e| FacadeError::Transport(format!("invalid url: {e}")))?;
    {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in params {
            pairs.append_pair(key, value);
        }
    }
    Ok(url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn transport_error_keeps_the_source_chain() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        drop(listener);

        let err = Http::new().get_once(&url, &[]).await.unwrap_err();
        let FacadeError::Transport(msg) = err else {
            panic!("expected a transport error, got {err:?}");
        };
        assert!(msg.starts_with("error sending request"), "{msg}");
        assert!(
            msg.to_ascii_lowercase().contains("connect"),
            "underlying cause missing from: {msg}"
        );
    }

    #[test]
    fn error_chain_joins_every_source() {
        let inner = std::io::Error::other("invalid peer certificate: UnknownIssuer");
        assert_eq!(
            error_chain(&Wrap(inner)),
            "wrapped: invalid peer certificate: UnknownIssuer"
        );
    }

    #[derive(Debug)]
    struct Wrap(std::io::Error);
    impl std::fmt::Display for Wrap {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("wrapped")
        }
    }
    impl std::error::Error for Wrap {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.0)
        }
    }

    use reqwest::header::{HeaderName, HeaderValue};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn hm(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(
                HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    fn epoch(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn real_now_secs() -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
    }

    #[tokio::test]
    async fn get_short_retry_then_ok() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "1"))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/x"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        let resp = Http::new().get(&format!("{}/x", server.uri()), &[]).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn get_long_wait_is_rate_limited_without_retry() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "60"))
            .expect(1)
            .mount(&server)
            .await;
        let err = Http::new().get(&format!("{}/x", server.uri()), &[]).await.unwrap_err();
        match err {
            FacadeError::RateLimited { retry_after } => {
                assert_eq!(retry_after, Some(Duration::from_secs(60)))
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn only_one_short_retry() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
            .expect(2)
            .mount(&server)
            .await;
        let err = Http::new().get(&format!("{}/x", server.uri()), &[]).await.unwrap_err();
        assert!(matches!(err, FacadeError::RateLimited { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn get_once_403_with_long_reset_is_returned_unretried() {
        let server = MockServer::start().await;
        let reset = (real_now_secs() + 900).to_string();
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(403)
                    .insert_header("x-ratelimit-remaining", "0")
                    .insert_header("x-ratelimit-reset", reset.as_str()),
            )
            .expect(1)
            .mount(&server)
            .await;
        let resp = Http::new()
            .get_once(&format!("{}/x", server.uri()), &[])
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn wait_delta_seconds() {
        let now = epoch(1_000_000);
        assert_eq!(
            rate_limit_wait_at(&hm(&[("retry-after", "42")]), now),
            Some(Duration::from_secs(42))
        );
    }

    #[test]
    fn wait_http_date() {
        let now = epoch(1_700_000_000);
        let ahead = httpdate::fmt_http_date(now + Duration::from_secs(30));
        let past = httpdate::fmt_http_date(now - Duration::from_secs(30));
        assert_eq!(
            rate_limit_wait_at(&hm(&[("retry-after", &ahead)]), now),
            Some(Duration::from_secs(30))
        );
        assert_eq!(
            rate_limit_wait_at(&hm(&[("retry-after", &past)]), now),
            Some(Duration::ZERO)
        );
    }

    #[test]
    fn wait_ratelimit_reset() {
        let now = epoch(1_700_000_000);
        let reset = "1700000090";
        assert_eq!(
            rate_limit_wait_at(
                &hm(&[("x-ratelimit-remaining", "0"), ("x-ratelimit-reset", reset)]),
                now
            ),
            Some(Duration::from_secs(90))
        );
        assert_eq!(
            rate_limit_wait_at(&hm(&[("x-ratelimit-reset", reset)]), now),
            Some(Duration::from_secs(90))
        );
        assert_eq!(
            rate_limit_wait_at(
                &hm(&[("x-ratelimit-remaining", "5"), ("x-ratelimit-reset", reset)]),
                now
            ),
            None
        );
        assert_eq!(
            rate_limit_wait_at(
                &hm(&[("x-ratelimit-remaining", "0"), ("x-ratelimit-reset", "1699999990")]),
                now
            ),
            None
        );
    }

    #[test]
    fn wait_clamped_and_empty() {
        let now = epoch(1_700_000_000);
        assert_eq!(
            rate_limit_wait_at(&hm(&[("retry-after", "99999999")]), now),
            Some(MAX_COOLDOWN)
        );
        assert_eq!(
            rate_limit_wait_at(
                &hm(&[("x-ratelimit-remaining", "0"), ("x-ratelimit-reset", "18446744073709551615")]),
                now
            ),
            Some(MAX_COOLDOWN)
        );
        assert_eq!(rate_limit_wait_at(&HeaderMap::new(), now), None);
    }

    #[test]
    fn rate_limited_classification() {
        let none = HeaderMap::new();
        assert!(is_rate_limited(StatusCode::TOO_MANY_REQUESTS, &none));
        assert!(is_rate_limited(
            StatusCode::FORBIDDEN,
            &hm(&[("x-ratelimit-remaining", "0")])
        ));
        assert!(is_rate_limited(StatusCode::FORBIDDEN, &hm(&[("retry-after", "5")])));
        assert!(!is_rate_limited(StatusCode::FORBIDDEN, &none));
        assert!(!is_rate_limited(
            StatusCode::FORBIDDEN,
            &hm(&[("x-ratelimit-remaining", "3")])
        ));
        assert!(!is_rate_limited(StatusCode::INTERNAL_SERVER_ERROR, &none));
    }
}
