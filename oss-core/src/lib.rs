mod dispatch;
mod engine;
mod envelope;
mod error;
mod params;
mod schema;
mod shaping;
mod stub;

pub use dispatch::{call_tool, BYTE_CAP};
pub use engine::{EngineOutput, SearchEngine};
pub use envelope::{BackendState, BackendStatus, Envelope};
pub use error::ToolError;
pub use params::{
    CodeSearchMode, FetchDocsParams, FetchFileParams, GuideParams, LineRange, RepoProfileParams,
    RepoTreeParams, ResponseFormat, SearchCodeParams, SearchReposParams, SortMode, ToolParams,
    REPO_FIELDS,
};
pub use schema::{schema_for, tool_specs, ToolSpec, TOOL_NAMES};
pub use shaping::{
    apply_response_format, cap_line, clamp_context_lines, format_snippet, sanitize_multiline,
    sanitize_rows, sanitize_str, sanitize_value, SanitizeReport, Snippet, DEFAULT_CONTEXT_LINES,
    MAX_CONTEXT_LINES, MAX_LINE_LEN, MIN_CONTEXT_LINES,
};
pub use stub::StubEngine;
