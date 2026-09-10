use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum FacadeError {
    #[error("transport error: {0}")]
    Transport(String),
    #[error("request timed out after {0:?}")]
    Timeout(Duration),
    #[error("rate limited (retry_after={retry_after:?})")]
    RateLimited { retry_after: Option<Duration> },
    #[error("grep.app error: {0}")]
    GrepApp(String),
    #[error("mcp grep.app error: {0}")]
    McpGrepApp(String),
    #[error("github error: {0}")]
    GitHub(String),
    #[error("deps.dev error: {0}")]
    DepsDev(String),
    #[error("ecosyste.ms error: {0}")]
    Ecosystems(String),
    #[error("npms error: {0}")]
    Npms(String),
    #[error("unexpected response from {backend}: {detail}")]
    Decode { backend: &'static str, detail: String },
    #[error("invalid query: {0}")]
    InvalidQuery(String),
}

pub type Result<T> = std::result::Result<T, FacadeError>;
