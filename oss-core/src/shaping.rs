use serde::Serialize;
use serde_json::Value;

use crate::params::CodeSearchMode;

pub const MIN_CONTEXT_LINES: usize = 3;
pub const MAX_CONTEXT_LINES: usize = 10;
pub const DEFAULT_CONTEXT_LINES: usize = 5;
pub const MAX_LINE_LEN: usize = 500;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseFormat {
    #[default]
    Concise,
    Detailed,
}

pub fn clamp_context_lines(requested: usize) -> usize {
    requested.clamp(MIN_CONTEXT_LINES, MAX_CONTEXT_LINES)
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Snippet {
    pub locator: String,
    pub path: String,
    pub start_line: u64,
    pub end_line: u64,
    pub match_line: u64,
    pub lines: Vec<String>,
}

pub fn format_snippet(
    path: &str,
    file_lines: &[String],
    match_line: u64,
    requested_context: usize,
) -> Snippet {
    let context = clamp_context_lines(requested_context);
    let total = file_lines.len() as u64;
    let clamped_match = match_line.clamp(1, total.max(1));
    let start = clamped_match.saturating_sub(context as u64).max(1);
    let end = (clamped_match + context as u64).min(total.max(1));
    let lines: Vec<String> = file_lines[(start as usize - 1)..(end as usize)]
        .iter()
        .map(|l| cap_line(l).to_string())
        .collect();
    Snippet {
        locator: format!("{path}:L{start}-L{end}"),
        path: path.to_string(),
        start_line: start,
        end_line: end,
        match_line: clamped_match,
        lines,
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SanitizeReport {
    pub bidi_escaped: usize,
    pub controls_stripped: usize,
    pub lines_truncated: usize,
}

fn is_bidi(c: char) -> bool {
    matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

pub fn sanitize_str(s: &str, report: &mut SanitizeReport) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '\n' | '\t' => out.push(c),
            '\u{1b}' => {
                report.controls_stripped += 1;
                if i + 1 < chars.len() && chars[i + 1] == '[' {
                    i += 2;
                    while i < chars.len() && !('\u{40}'..='\u{7e}').contains(&chars[i]) {
                        i += 1;
                    }
                    i += 1;
                    continue;
                }
                if i + 1 < chars.len() && (chars[i + 1] == ']' || chars[i + 1] == 'P') {
                    i += 2;
                    while i < chars.len() && chars[i] != '\u{7}' && chars[i] != '\u{1b}' {
                        i += 1;
                    }
                    continue;
                }
            }
            c if (c as u32) < 0x20 || matches!(c, '\u{7F}'..='\u{9F}') => {
                report.controls_stripped += 1;
            }
            c if is_bidi(c) => {
                report.bidi_escaped += 1;
                out.push_str(&format!("<U+{:04X}>", c as u32));
            }
            c => out.push(c),
        }
        i += 1;
    }
    out
}

pub fn cap_line(line: &str) -> String {
    let count = line.chars().count();
    if count <= MAX_LINE_LEN {
        return line.to_string();
    }
    let kept: String = line.chars().take(MAX_LINE_LEN).collect();
    format!("{kept}…[+{} chars truncated]", count - MAX_LINE_LEN)
}

pub fn sanitize_multiline(s: &str, report: &mut SanitizeReport) -> String {
    let cleaned = sanitize_str(s, report);
    let mut lines: Vec<String> = Vec::new();
    for line in cleaned.split('\n') {
        if line.chars().count() > MAX_LINE_LEN {
            report.lines_truncated += 1;
        }
        lines.push(cap_line(line));
    }
    lines.join("\n")
}

pub fn sanitize_value(v: &mut Value, report: &mut SanitizeReport) {
    match v {
        Value::String(s) => {
            let cleaned = sanitize_multiline(s, report);
            *s = cleaned;
        }
        Value::Array(items) => {
            for item in items {
                sanitize_value(item, report);
            }
        }
        Value::Object(map) => {
            for (_, val) in map.iter_mut() {
                sanitize_value(val, report);
            }
        }
        _ => {}
    }
}

pub fn sanitize_rows(rows: &mut [Value]) -> SanitizeReport {
    let mut report = SanitizeReport::default();
    for row in rows.iter_mut() {
        sanitize_value(row, &mut report);
    }
    report
}

const CONCISE_KEEP_REPOS: &[&str] = &[
    "repo",
    "stars",
    "star_tier",
    "license",
    "license_class",
    "activity",
    "what",
    "url",
];
const CONCISE_KEEP_CODE: &[&str] = &[
    "repo",
    "probes_hit",
    "probe_breadth",
    "total_probes",
    "hit_files",
    "license_class",
    "path",
    "locator",
    "start_line",
    "end_line",
    "backends",
];

pub fn apply_response_format(
    rows: &mut [Value],
    tool: &str,
    _mode: CodeSearchMode,
    fmt: ResponseFormat,
) {
    if fmt == ResponseFormat::Detailed {
        return;
    }
    let keep: &[&str] = match tool {
        "oss_search_repos" => CONCISE_KEEP_REPOS,
        "oss_search_code" => CONCISE_KEEP_CODE,
        _ => return,
    };
    for row in rows.iter_mut() {
        if let Value::Object(map) = row {
            map.retain(|k, _| keep.contains(&k.as_str()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_lines_clamped_to_range() {
        assert_eq!(clamp_context_lines(0), MIN_CONTEXT_LINES);
        assert_eq!(clamp_context_lines(2), MIN_CONTEXT_LINES);
        assert_eq!(clamp_context_lines(5), 5);
        assert_eq!(clamp_context_lines(99), MAX_CONTEXT_LINES);
    }

    #[test]
    fn snippet_locator_and_bounds() {
        let lines: Vec<String> = (1..=20).map(|i| format!("line {i}")).collect();
        let s = format_snippet("src/market.py", &lines, 10, 3);
        assert_eq!(s.locator, "src/market.py:L7-L13");
        assert_eq!(s.start_line, 7);
        assert_eq!(s.end_line, 13);
        assert_eq!(s.lines.len(), 7);
        let s = format_snippet("src/market.py", &lines, 1, 3);
        assert_eq!(s.start_line, 1);
        let s = format_snippet("src/market.py", &lines, 20, 3);
        assert_eq!(s.end_line, 20);
    }

    #[test]
    fn strips_ansi_escapes_keeps_newline_tab() {
        let mut report = SanitizeReport::default();
        let fixture = "\u{1b}[31mred\u{1b}[0m\ttab\nkeep";
        let out = sanitize_multiline(fixture, &mut report);
        assert_eq!(out, "red\ttab\nkeep");
        assert_eq!(report.controls_stripped, 2);
    }

    #[test]
    fn escapes_bidi_overrides_with_marker() {
        let mut report = SanitizeReport::default();
        let fixture = "if access_level != \u{202E}user\u{202C} {";
        let out = sanitize_multiline(fixture, &mut report);
        assert_eq!(out, "if access_level != <U+202E>user<U+202C> {");
        assert_eq!(report.bidi_escaped, 2);
    }

    #[test]
    fn escapes_isolates_and_c1() {
        let mut report = SanitizeReport::default();
        let out = sanitize_str("a\u{2066}b\u{2069}c\u{85}d\u{9b}e", &mut report);
        assert_eq!(out, "a<U+2066>b<U+2069>cde");
        assert_eq!(report.bidi_escaped, 2);
        assert_eq!(report.controls_stripped, 2);
    }

    #[test]
    fn caps_long_lines_on_char_boundaries() {
        let mut report = SanitizeReport::default();
        let long = "é".repeat(MAX_LINE_LEN + 10);
        let out = sanitize_multiline(&long, &mut report);
        assert!(report.lines_truncated == 1);
        assert!(out.chars().count() < MAX_LINE_LEN + 60);
        assert!(out.ends_with(" chars truncated]"));
    }

    #[test]
    fn sanitize_value_walks_nested_rows() {
        let mut row = serde_json::json!({
            "repo": "x/y",
            "nested": { "content": "\u{202E}evil\u{202C}" },
            "list": ["\u{1b}[31mansi", "clean"]
        });
        let mut report = SanitizeReport::default();
        sanitize_value(&mut row, &mut report);
        assert_eq!(row["nested"]["content"], "<U+202E>evil<U+202C>");
        assert_eq!(row["list"][0], "ansi");
        assert_eq!(report.bidi_escaped, 2);
        assert_eq!(report.controls_stripped, 1);
    }

    #[test]
    fn concise_drops_heavy_fields() {
        let mut rows = vec![serde_json::json!({
            "repo": "a/b", "stars": 10, "what": "pool", "catch": "churn", "days_since_activity": 3
        })];
        apply_response_format(&mut rows, "oss_search_repos", CodeSearchMode::Repos, ResponseFormat::Concise);
        let row = &rows[0];
        assert!(row.get("catch").is_none());
        assert!(row.get("days_since_activity").is_none());
        assert_eq!(row["repo"], "a/b");
        assert_eq!(row["stars"], 10);
        apply_response_format(&mut rows, "oss_search_repos", CodeSearchMode::Repos, ResponseFormat::Detailed);
    }

    #[test]
    fn detailed_keeps_everything() {
        let mut rows = vec![serde_json::json!({ "catch": "churn", "sample_evidence": "x" })];
        apply_response_format(&mut rows, "oss_search_code", CodeSearchMode::Repos, ResponseFormat::Detailed);
        assert!(rows[0].get("catch").is_some());
    }
}
