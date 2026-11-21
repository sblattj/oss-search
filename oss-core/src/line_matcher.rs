//! Per-line matcher shared by every `oss_fetch_docs` backend: plain text is a
//! case-insensitive literal substring, `/regex/` is a case-insensitive regex.

use serde_json::Value;

use crate::ToolError;

pub enum LineMatcher {
    Literal(String),
    Regex(regex::Regex),
}

impl LineMatcher {
    /// `input` is the echoed request, used only for the typed error.
    pub fn new(pattern: &str, input: &Value) -> Result<Self, ToolError> {
        match pattern.strip_prefix('/').and_then(|i| i.strip_suffix('/')) {
            Some(inner) if !inner.is_empty() => regex::RegexBuilder::new(inner)
                .case_insensitive(true)
                .build()
                .map(LineMatcher::Regex)
                .map_err(|e| ToolError::InvalidValue {
                    field: "pattern".into(),
                    message: format!("invalid regex `{inner}`: {e}"),
                    input: input.clone(),
                    correction: "use RE2-style /regex/ syntax, or pass plain text for a literal match; see oss_guide".into(),
                }),
            _ => Ok(LineMatcher::Literal(pattern.to_lowercase())),
        }
    }

    pub fn is_match(&self, line: &str) -> bool {
        match self {
            LineMatcher::Literal(needle) => line.to_lowercase().contains(needle),
            LineMatcher::Regex(re) => re.is_match(line),
        }
    }
}
