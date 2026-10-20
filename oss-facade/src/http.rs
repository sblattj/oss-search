use std::time::Duration;

use reqwest::{Client, Method, Response, StatusCode};

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
            if status == StatusCode::TOO_MANY_REQUESTS && map_429 {
                let retry_after = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    .map(Duration::from_secs);
                return Err(FacadeError::RateLimited { retry_after });
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
}
