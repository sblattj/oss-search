use serde_json::Value;

use crate::envelope::BackendStatus;
use crate::error::ToolError;
use crate::params::{
    FetchDocsParams, FetchFileParams, GuideParams, RepoProfileParams, RepoTreeParams,
    SearchCodeParams, SearchReposParams,
};

#[derive(Debug, Clone)]
pub struct EngineOutput {
    pub results: Vec<Value>,
    pub total: u64,
    pub has_more: bool,
    pub partial: bool,
    pub next_cursor: Option<String>,
}

impl EngineOutput {
    pub fn complete(results: Vec<Value>) -> Self {
        let total = results.len() as u64;
        EngineOutput {
            results,
            total,
            has_more: false,
            partial: false,
            next_cursor: None,
        }
    }

    pub fn paged(results: Vec<Value>, total: u64, next_cursor: Option<String>) -> Self {
        EngineOutput {
            results,
            total,
            has_more: next_cursor.is_some(),
            partial: false,
            next_cursor,
        }
    }
}

pub trait SearchEngine: Send + Sync {
    fn backend_status(&self, tool: &str) -> Vec<BackendStatus>;
    fn search_repos(&self, params: &SearchReposParams) -> Result<EngineOutput, ToolError>;
    fn search_code(&self, params: &SearchCodeParams) -> Result<EngineOutput, ToolError>;
    fn repo_profile(&self, params: &RepoProfileParams) -> Result<EngineOutput, ToolError>;
    fn repo_tree(&self, params: &RepoTreeParams) -> Result<EngineOutput, ToolError>;
    fn fetch_file(&self, params: &FetchFileParams) -> Result<EngineOutput, ToolError>;
    fn fetch_docs(&self, params: &FetchDocsParams) -> Result<EngineOutput, ToolError>;
    fn guide(&self, params: &GuideParams) -> Result<EngineOutput, ToolError>;
}
