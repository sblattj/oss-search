use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::ToolError;
pub use crate::shaping::ResponseFormat;

pub const REPO_FIELDS: &[&str] = &["what", "catch", "license", "recency", "stars", "urls"];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortMode {
    #[default]
    Fitness,
    Stars,
    Updated,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeSearchMode {
    #[default]
    Repos,
    Snippets,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineRange {
    pub start: u64,
    pub end: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchReposParams {
    pub query: String,
    pub topic: Option<String>,
    pub language: Option<String>,
    pub min_stars: Option<u64>,
    #[serde(default)]
    pub sort: SortMode,
    #[serde(default = "search_repos_default_limit")]
    pub limit: u64,
    pub cursor: Option<String>,
    pub fields: Option<Vec<String>>,
    pub response_format: Option<ResponseFormat>,
}

fn search_repos_default_limit() -> u64 {
    10
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchCodeParams {
    pub probes: Vec<String>,
    pub language: Option<String>,
    pub path: Option<String>,
    pub repo_filter: Option<String>,
    #[serde(default)]
    pub mode: CodeSearchMode,
    #[serde(default = "snippets_default")]
    pub snippets_per_repo: u64,
    #[serde(default = "search_code_default_limit")]
    pub limit: u64,
    pub exclude_known: Option<String>,
    pub cursor: Option<String>,
    pub response_format: Option<ResponseFormat>,
}

fn snippets_default() -> u64 {
    2
}

fn search_code_default_limit() -> u64 {
    15
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoProfileParams {
    pub repo: String,
    #[serde(default)]
    pub deep_docs: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepoTreeParams {
    pub repo: String,
    #[serde(rename = "ref")]
    pub ref_: Option<String>,
    pub path_prefix: Option<String>,
    pub glob: Option<String>,
    #[serde(default = "tree_default_limit")]
    pub limit: u64,
}

fn tree_default_limit() -> u64 {
    200
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FetchFileParams {
    pub repo: String,
    pub path: String,
    #[serde(rename = "ref")]
    pub ref_: Option<String>,
    pub line_range: Option<LineRange>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FetchDocsParams {
    pub repo: String,
    pub pattern: String,
    pub docs_host: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuideParams {}

pub trait ToolParams: Sized + serde::de::DeserializeOwned {
    const TOOL: &'static str;
    fn known_fields() -> &'static [&'static str];
    fn validate(self, input: Value) -> Result<Self, ToolError> {
        let _ = input;
        Ok(self)
    }

    fn parse(input: Value) -> Result<Self, ToolError> {
        let echo = input.clone();
        match serde_json::from_value::<Self>(input) {
            Ok(params) => params.validate(echo),
            Err(e) => Err(map_de_error(e, Self::known_fields(), echo)),
        }
    }
}

fn map_de_error(e: serde_json::Error, known: &[&str], echo: Value) -> ToolError {
    let msg = e.to_string();
    let known: Vec<String> = known.iter().map(|s| s.to_string()).collect();
    if let Some(rest) = msg.strip_prefix("unknown field `") {
        let field = rest.split('`').next().unwrap_or_default().to_string();
        return ToolError::UnknownField {
            field,
            known,
            input: echo,
        };
    }
    if let Some(rest) = msg.strip_prefix("missing field `") {
        let field = rest.split('`').next().unwrap_or_default().to_string();
        return ToolError::MissingField {
            field,
            known,
            input: echo,
        };
    }
    ToolError::InvalidValue {
        field: String::new(),
        message: msg,
        input: echo,
        correction: "fix the field type to match the inputSchema; see oss_guide".to_string(),
    }
}

fn invalid(field: &str, message: String, correction: &str, input: Value) -> ToolError {
    ToolError::InvalidValue {
        field: field.to_string(),
        message,
        input,
        correction: correction.to_string(),
    }
}

impl ToolParams for SearchReposParams {
    const TOOL: &'static str = "oss_search_repos";
    fn known_fields() -> &'static [&'static str] {
        &[
            "query",
            "topic",
            "language",
            "min_stars",
            "sort",
            "limit",
            "cursor",
            "fields",
            "response_format",
        ]
    }

    fn validate(self, input: Value) -> Result<Self, ToolError> {
        if self.limit > 30 {
            return Err(invalid(
                "limit",
                format!("limit {} exceeds maximum 30", self.limit),
                "use limit <= 30, or page with cursor",
                input,
            ));
        }
        if let Some(fields) = &self.fields {
            for f in fields {
                if !REPO_FIELDS.contains(&f.as_str()) {
                    let correction = format!("use only: {}", REPO_FIELDS.join(", "));
                    return Err(invalid(
                        "fields",
                        format!(
                            "unknown field selector `{f}`; allowed: {}",
                            REPO_FIELDS.join(", ")
                        ),
                        &correction,
                        input,
                    ));
                }
            }
        }
        Ok(self)
    }
}

impl ToolParams for SearchCodeParams {
    const TOOL: &'static str = "oss_search_code";
    fn known_fields() -> &'static [&'static str] {
        &[
            "probes",
            "language",
            "path",
            "repo_filter",
            "mode",
            "snippets_per_repo",
            "limit",
            "exclude_known",
            "cursor",
            "response_format",
        ]
    }

    fn validate(self, input: Value) -> Result<Self, ToolError> {
        if self.probes.is_empty() || self.probes.len() > 5 {
            return Err(invalid(
                "probes",
                format!(
                    "probes must contain 1 to 5 items, got {}",
                    self.probes.len()
                ),
                "pass 1-5 distinctive code fingerprints, e.g. [\"spawn_worker\", \"/early[Cc]loses/\"]",
                input,
            ));
        }
        if self.snippets_per_repo > 5 {
            return Err(invalid(
                "snippets_per_repo",
                format!("snippets_per_repo {} exceeds maximum 5", self.snippets_per_repo),
                "use snippets_per_repo <= 5",
                input,
            ));
        }
        if self.limit > 40 {
            return Err(invalid(
                "limit",
                format!("limit {} exceeds maximum 40", self.limit),
                "use limit <= 40, or page with cursor",
                input,
            ));
        }
        Ok(self)
    }
}

impl ToolParams for RepoProfileParams {
    const TOOL: &'static str = "oss_repo_profile";
    fn known_fields() -> &'static [&'static str] {
        &["repo", "deep_docs"]
    }

    fn validate(self, input: Value) -> Result<Self, ToolError> {
        if !self.repo.contains('/') {
            return Err(invalid(
                "repo",
                format!("repo `{}` is not in owner/name form", self.repo),
                "use owner/name form, e.g. \"ferrum/poolite\"; call oss_search_repos if unsure",
                input,
            ));
        }
        Ok(self)
    }
}

impl ToolParams for RepoTreeParams {
    const TOOL: &'static str = "oss_repo_tree";
    fn known_fields() -> &'static [&'static str] {
        &["repo", "ref", "path_prefix", "glob", "limit"]
    }

    fn validate(self, input: Value) -> Result<Self, ToolError> {
        if self.limit > 500 {
            return Err(invalid(
                "limit",
                format!("limit {} exceeds maximum 500", self.limit),
                "use limit <= 500 and page with path_prefix",
                input,
            ));
        }
        Ok(self)
    }
}

impl ToolParams for FetchFileParams {
    const TOOL: &'static str = "oss_fetch_file";
    fn known_fields() -> &'static [&'static str] {
        &["repo", "path", "ref", "line_range"]
    }

    fn validate(self, input: Value) -> Result<Self, ToolError> {
        if let Some(range) = &self.line_range {
            if range.start < 1 {
                return Err(invalid(
                    "line_range",
                    format!("line_range.start {} is below 1", range.start),
                    "line_range is 1-based inclusive, e.g. {\"start\": 1, \"end\": 100}",
                    input,
                ));
            }
            if range.end < range.start {
                return Err(invalid(
                    "line_range",
                    format!(
                        "line_range.end {} is before line_range.start {}",
                        range.end, range.start
                    ),
                    "use end >= start, e.g. {\"start\": 10, \"end\": 40}",
                    input,
                ));
            }
        }
        Ok(self)
    }
}

impl ToolParams for FetchDocsParams {
    const TOOL: &'static str = "oss_fetch_docs";
    fn known_fields() -> &'static [&'static str] {
        &["repo", "pattern", "docs_host"]
    }

    fn validate(self, input: Value) -> Result<Self, ToolError> {
        if self.pattern.trim().is_empty() {
            return Err(invalid(
                "pattern",
                "pattern must not be empty".to_string(),
                "pass a text substring or /regex/, e.g. \"early close\"",
                input,
            ));
        }
        Ok(self)
    }
}

impl ToolParams for GuideParams {
    const TOOL: &'static str = "oss_guide";
    fn known_fields() -> &'static [&'static str] {
        &[]
    }
}
