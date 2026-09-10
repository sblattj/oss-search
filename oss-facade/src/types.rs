use std::time::Duration;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filters {
    pub language: Option<String>,
    pub repo: Option<String>,
    pub org: Option<String>,
    pub user: Option<String>,
    pub path: Option<String>,
    pub filename: Option<String>,
    pub extension: Option<String>,
    pub regexp: bool,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineRange {
    pub start: u64,
    pub end: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub backend: &'static str,
    pub repo: Option<String>,
    pub path: Option<String>,
    pub url: Option<String>,
    pub line_ranges: Vec<LineRange>,
    pub snippet: String,
    pub license: Option<String>,
    pub score: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Ok,
    Degraded { reason: String },
    Unavailable { reason: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct BackendResponse {
    pub hits: Vec<Hit>,
    pub total: Option<u64>,
    pub has_more: bool,
    pub partial: bool,
    pub status: Status,
    pub warnings: Vec<String>,
    pub elapsed: Duration,
}

impl BackendResponse {
    pub fn unavailable(reason: impl Into<String>, elapsed: Duration) -> Self {
        Self {
            hits: Vec::new(),
            total: None,
            has_more: false,
            partial: false,
            status: Status::Unavailable {
                reason: reason.into(),
            },
            warnings: Vec::new(),
            elapsed,
        }
    }

    pub fn degraded(reason: impl Into<String>, elapsed: Duration) -> Self {
        Self {
            hits: Vec::new(),
            total: None,
            has_more: false,
            partial: false,
            status: Status::Degraded {
                reason: reason.into(),
            },
            warnings: Vec::new(),
            elapsed,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    pub name: &'static str,
    pub regex: bool,
    pub language_filter: bool,
    pub repo_filter: bool,
    pub org_filter: bool,
    pub path_filter: bool,
    pub max_results: Option<u64>,
    pub rate_limit_per_minute: Option<u32>,
    pub auth_required: bool,
    pub line_numbers: bool,
    pub notes: &'static [&'static str],
}
