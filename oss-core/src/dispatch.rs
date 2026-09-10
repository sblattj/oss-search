use serde_json::Value;

use crate::engine::{EngineOutput, SearchEngine};
use crate::envelope::Envelope;
use crate::error::ToolError;
use crate::params::{
    CodeSearchMode, FetchDocsParams, FetchFileParams, GuideParams, RepoProfileParams,
    RepoTreeParams, ResponseFormat, SearchCodeParams, SearchReposParams, ToolParams,
};
use crate::shaping::{apply_response_format, sanitize_rows};
use crate::TOOL_NAMES;

pub const BYTE_CAP: usize = 25_000;

pub fn call_tool(
    engine: &dyn SearchEngine,
    name: &str,
    arguments: Option<Value>,
) -> Result<Value, ToolError> {
    let args = arguments.unwrap_or_else(|| serde_json::json!({}));
    if !args.is_object() && !args.is_null() {
        return Err(ToolError::InvalidValue {
            field: String::new(),
            message: "arguments must be a JSON object".to_string(),
            input: args,
            correction: "pass an object matching the tool's inputSchema".to_string(),
        });
    }
    let args = if args.is_null() { serde_json::json!({}) } else { args };
    match name {
        "oss_search_repos" => {
            let params = SearchReposParams::parse(args)?;
            let fmt = params.response_format;
            finish(engine, name, engine.search_repos(&params)?, CodeSearchMode::Repos, fmt)
        }
        "oss_search_code" => {
            let params = SearchCodeParams::parse(args)?;
            let fmt = params.response_format;
            let mode = params.mode;
            finish(engine, name, engine.search_code(&params)?, mode, fmt)
        }
        "oss_repo_profile" => {
            let params = RepoProfileParams::parse(args)?;
            finish(engine, name, engine.repo_profile(&params)?, CodeSearchMode::Repos, None)
        }
        "oss_repo_tree" => {
            let params = RepoTreeParams::parse(args)?;
            finish(engine, name, engine.repo_tree(&params)?, CodeSearchMode::Repos, None)
        }
        "oss_fetch_file" => {
            let params = FetchFileParams::parse(args)?;
            finish(engine, name, engine.fetch_file(&params)?, CodeSearchMode::Repos, None)
        }
        "oss_fetch_docs" => {
            let params = FetchDocsParams::parse(args)?;
            finish(engine, name, engine.fetch_docs(&params)?, CodeSearchMode::Repos, None)
        }
        "oss_guide" => {
            let params = GuideParams::parse(args)?;
            finish(engine, name, engine.guide(&params)?, CodeSearchMode::Repos, None)
        }
        _ => Err(ToolError::UnknownTool {
            tool: name.to_string(),
            known: TOOL_NAMES.iter().map(|s| s.to_string()).collect(),
        }),
    }
}

fn finish(
    engine: &dyn SearchEngine,
    tool: &str,
    out: EngineOutput,
    mode: CodeSearchMode,
    fmt: Option<ResponseFormat>,
) -> Result<Value, ToolError> {
    let EngineOutput {
        mut results,
        total,
        has_more,
        partial,
        next_cursor,
    } = out;
    if let Some(fmt) = fmt {
        apply_response_format(&mut results, tool, mode, fmt);
    }
    let report = sanitize_rows(&mut results);
    let mut warnings = Vec::new();
    if report.bidi_escaped > 0 {
        warnings.push(format!(
            "output sanitized: {} bidi override character(s) replaced with visible <U+XXXX> markers",
            report.bidi_escaped
        ));
    }
    if report.lines_truncated > 0 {
        warnings.push(format!(
            "output sanitized: {} line(s) capped at the per-line length limit",
            report.lines_truncated
        ));
    }
    let shown = results.len() as u64;
    let note = if has_more {
        Some(format!(
            "showing {shown} of {total} — pass the next_cursor value back as cursor to page"
        ))
    } else {
        None
    };
    let envelope = Envelope {
        results,
        total: Some(total),
        has_more,
        partial,
        backend_status: engine.backend_status(tool),
        warnings,
        truncated: false,
        note,
        next_cursor,
    };
    let envelope = envelope.enforce_byte_budget(BYTE_CAP);
    Ok(serde_json::to_value(&envelope).unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stub::StubEngine;

    fn env_of(v: &Value) -> &Value {
        v
    }

    #[test]
    fn guide_returns_playbook_envelope() {
        let engine = StubEngine::new();
        let out = call_tool(&engine, "oss_guide", Some(serde_json::json!({}))).unwrap();
        assert_eq!(out["results"][0]["probe_writing"]["good"][0], "spawn_worker");
        assert_eq!(out["total"], 1);
        assert_eq!(out["has_more"], false);
        assert!(out["backend_status"].is_array());
    }

    #[test]
    fn unknown_tool_lists_known_tools() {
        let engine = StubEngine::new();
        let err = call_tool(&engine, "oss_nope", None).unwrap_err();
        assert_eq!(err.code(), "unknown_tool");
        let j = err.to_json();
        assert!(j["error"]["message"].as_str().unwrap().contains("oss_guide"));
    }

    #[test]
    fn unknown_param_echoes_input_and_known_fields() {
        let engine = StubEngine::new();
        let err = call_tool(
            &engine,
            "oss_search_repos",
            Some(serde_json::json!({"query": "pool", "bogus": 1})),
        )
        .unwrap_err();
        assert_eq!(err.code(), "unknown_field");
        let j = err.to_json();
        assert_eq!(j["error"]["input_echo"]["bogus"], 1);
        assert_eq!(j["error"]["input_echo"]["query"], "pool");
        let known: Vec<&str> = j["error"]["known_fields"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(known.contains(&"query"));
        assert!(!known.contains(&"bogus"));
    }

    #[test]
    fn missing_required_field_names_correction() {
        let engine = StubEngine::new();
        let err = call_tool(&engine, "oss_search_repos", Some(serde_json::json!({}))).unwrap_err();
        assert_eq!(err.code(), "missing_field");
        let j = err.to_json();
        assert!(j["error"]["message"].as_str().unwrap().contains("query"));
        assert!(j["error"]["correction"]
            .as_str()
            .unwrap()
            .contains("\"query\""));
    }

    #[test]
    fn guide_rejects_any_arguments() {
        let engine = StubEngine::new();
        let err = call_tool(&engine, "oss_guide", Some(serde_json::json!({"x": 1}))).unwrap_err();
        assert_eq!(err.code(), "unknown_field");
    }

    #[test]
    fn search_repos_default_call_under_budget() {
        let engine = StubEngine::new();
        let out = call_tool(&engine, "oss_search_repos", Some(serde_json::json!({"query": "pool"})))
            .unwrap();
        let bytes = serde_json::to_vec(&out).unwrap().len();
        assert!(bytes <= BYTE_CAP, "{bytes} > {BYTE_CAP}");
        assert!(!out["results"].as_array().unwrap().is_empty());
    }

    #[test]
    fn sanitized_fixture_flows_through_with_warning() {
        let engine = StubEngine::new();
        let out = call_tool(
            &engine,
            "oss_fetch_file",
            Some(serde_json::json!({
                "repo": "helix/edge-sdk",
                "path": "packages/core/src/vendor/note.md"
            })),
        )
        .unwrap();
        let content = out["results"][0]["content"].as_str().unwrap();
        assert!(!content.contains('\u{1b}'));
        assert!(content.contains("<U+202E>"));
        let warnings = out["warnings"].as_array().unwrap();
        assert!(warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("bidi")));
    }

    #[test]
    fn not_found_gives_actionable_correction() {
        let engine = StubEngine::new();
        let err = call_tool(
            &engine,
            "oss_fetch_file",
            Some(serde_json::json!({"repo": "ferrum/poolite", "path": "src/guessed.rs"})),
        )
        .unwrap_err();
        assert_eq!(err.code(), "not_found");
        let j = err.to_json();
        assert!(j["error"]["correction"]
            .as_str()
            .unwrap()
            .contains("oss_repo_tree"));
    }

    #[test]
    fn envelope_shape_on_every_tool() {
        let engine = StubEngine::new();
        let calls: Vec<(&str, Value)> = vec![
            ("oss_search_repos", serde_json::json!({"query": "calendar"})),
            ("oss_search_code", serde_json::json!({"probes": ["special_closes"]})),
            ("oss_repo_profile", serde_json::json!({"repo": "gabe/hashopen"})),
            ("oss_repo_tree", serde_json::json!({"repo": "gabe/hashopen"})),
            ("oss_fetch_file", serde_json::json!({"repo": "gabe/hashopen", "path": "src/market.py"})),
            ("oss_fetch_docs", serde_json::json!({"repo": "gabe/hashopen", "pattern": "early"})),
            ("oss_guide", serde_json::json!({})),
        ];
        for (tool, args) in calls {
            let out = call_tool(&engine, tool, Some(args)).unwrap();
            assert!(out["results"].is_array(), "{tool} results");
            assert!(out["total"].is_u64(), "{tool} total");
            assert!(out["has_more"].is_boolean(), "{tool} has_more");
            assert!(out["partial"].is_boolean(), "{tool} partial");
            assert!(out["backend_status"].is_array(), "{tool} backend_status");
            assert!(out["warnings"].is_array(), "{tool} warnings");
            assert!(out["truncated"].is_boolean(), "{tool} truncated");
            let _ = env_of(&out);
        }
    }
}
