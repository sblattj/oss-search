mod deps_dev;
mod ecosystems;
mod error;
mod github;
mod github_content;
mod grep_app;
mod http;
mod npms;
mod rate;
mod types;

pub use deps_dev::{DepsAdvisory, DepsDevClient, DepsVersionDetails, DepsVersionInfo, DEPS_DEV_BASE};
pub use ecosystems::{
    EcosystemsClient, EcosystemsPackage, EcosystemsPackageMeta, EcosystemsRepo, EcosystemsRepoMeta,
    PACKAGES_BASE, REPOS_BASE,
};
pub use error::{FacadeError, Result};
pub use github::{GitHubSearchClient, GITHUB_API_BASE, GITHUB_CODE_SEARCH_RPM, GITHUB_RESULT_CAP};
pub use github_content::{
    GithubContentClient, GhFileContent, GhTree, GhTreeEntry, GITHUB_CONTENT_RPH, GITHUB_CONTENT_RPM,
};
pub use grep_app::{GrepAppClient, McpGrepAppClient, GREP_APP_BASE, MCP_GREP_APP_BASE};
pub use http::DEFAULT_TIMEOUT;
pub use npms::{NpmsClient, NpmsPackageScore, NPMS_BASE};
pub use rate::RateLimiter;
pub use types::{BackendResponse, Capabilities, Filters, Hit, LineRange, Status};

#[async_trait::async_trait]
pub trait BackendClient: Send + Sync {
    async fn search_code(&self, query: &str, filters: &Filters) -> Result<BackendResponse>;
    fn capabilities(&self) -> Capabilities;
}
