//! Security-hardening tests: output escaping through the real tool-call
//! path, oss_fetch_file prompt-injection hygiene, error-echo sanitization,
//! and ReDoS-shaped patterns through the escaper.

use oss_core::{call_tool, sanitize_multiline, sanitize_str, SanitizeReport, StubEngine};
use serde_json::{json, Value};

const FIXTURE_REPO: &str = "helix/edge-sdk";
const FIXTURE_PATH: &str = "packages/core/src/vendor/escape-lab.md";

const BOUND: std::time::Duration = std::time::Duration::from_secs(2);

fn fetch_fixture() -> Value {
    let engine = StubEngine::new();
    call_tool(
        &engine,
        "oss_fetch_file",
        Some(json!({"repo": FIXTURE_REPO, "path": FIXTURE_PATH})),
    )
    .expect("fetch escape-lab fixture")
}

#[test]
fn escaper_fixture_no_raw_control_or_bidi_in_tool_output() {
    let out = fetch_fixture();
    let content = out["results"][0]["content"].as_str().unwrap();
    // Raw response must not carry terminal-control or bidi characters.
    assert!(!content.contains('\u{1b}'), "raw ESC in content");
    assert!(!content.contains('\u{7}'), "raw BEL in content");
    assert!(!content.contains('\u{202e}'), "raw RLO in content");
    assert!(!content.contains('\u{202c}'), "raw PDF in content");
    assert!(!content.contains('\u{2066}'), "raw LRI in content");
    assert!(!content.contains('\u{2069}'), "raw PDI in content");
    // Visible markers replace them.
    assert!(content.contains("<U+202E>"));
    assert!(content.contains("<U+2066>"));
    assert!(!content.contains("click me") || !content.contains("evil.example"));
    // The whole serialized envelope is clean, not just the content field.
    let raw = serde_json::to_string(&out).unwrap();
    assert!(!raw.contains('\u{1b}'));
    assert!(!raw.contains('\u{202e}'));
    assert!(!raw.contains('\u{2066}'));
}

#[test]
fn long_line_capped_with_marker_and_warning() {
    let out = fetch_fixture();
    let content = out["results"][0]["content"].as_str().unwrap();
    let long_line: Vec<&str> = content.split('\n').collect();
    let longest = long_line.iter().map(|l| l.chars().count()).max().unwrap();
    assert!(
        longest < 600,
        "5000-char line must be capped, longest is {longest}"
    );
    assert!(content.contains("chars truncated]"));
    let warnings = out["warnings"].as_array().unwrap();
    assert!(warnings
        .iter()
        .any(|w| w.as_str().unwrap().contains("bidi")));
    assert!(warnings
        .iter()
        .any(|w| w.as_str().unwrap().contains("line(s) capped")));
}

/// Prompt-injection hygiene (spec item 4): file content surfaced through
/// oss_fetch_file must pass through the SAME escaper that every other tool
/// output passes through. Behavioral proof: running the exported escaper
/// directly over the raw fixture text produces exactly the bytes the tool
/// returned, and the escaper's report matches the emitted warnings.
#[test]
fn fetch_file_content_flows_through_shared_escaper() {
    // The raw fixture text, as the StubEngine holds it (unsanitized).
    let raw_fixture = [
        "lab: control file exercising the output escaper",
        "csi: \u{1b}[31mred\u{1b}[0m text",
        "osc8: \u{1b}]8;;https://evil.example\u{7}click me\u{1b}]8;;\u{7}",
        "rlo: \u{202e}evil\u{202c}",
        "lri: \u{2066}iso\u{2069}",
        "bell: a\u{7}b",
    ]
    .join("\n");
    let mut report = SanitizeReport::default();
    let escaped_direct = sanitize_multiline(&raw_fixture, &mut report);
    assert!(report.bidi_escaped >= 3);
    assert!(report.controls_stripped >= 6);

    // What the tool actually returned for the same lines.
    let out = fetch_fixture();
    let content = out["results"][0]["content"].as_str().unwrap();
    // The tool caps the fixture at 100 lines by default; the escape-lab
    // control lines all fall in that range, so the escaped prefix must be
    // byte-identical to the direct escaper run.
    let tool_prefix: String = content
        .split('\n')
        .take(6)
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        tool_prefix, escaped_direct,
        "oss_fetch_file content is NOT the shared escaper's output"
    );
    // And the warnings correspond to the escaper's report counts.
    let warnings = out["warnings"].as_array().unwrap();
    assert!(warnings
        .iter()
        .any(|w| w.as_str().unwrap().contains("bidi override character")));
}

/// Error responses must meet the same hygiene bar: attacker-controlled
/// input is echoed in input_echo/message and previously flowed to the wire
/// raw (fixed: ToolError::to_json now runs the shared sanitizer).
#[test]
fn error_echo_path_sanitized() {
    let engine = StubEngine::new();
    let hostile = "\u{202e}rdm\u{202c}\u{1b}[31m";
    let err = call_tool(
        &engine,
        "oss_search_repos",
        Some(json!({"query": hostile, "bogus": 1})),
    )
    .unwrap_err();
    let j = err.to_json();
    let raw = serde_json::to_string(&j).unwrap();
    assert!(!raw.contains('\u{1b}'), "raw ESC in error echo: {raw}");
    assert!(!raw.contains('\u{202e}'), "raw RLO in error echo: {raw}");
    assert!(raw.contains("<U+202E>"), "RLO not visibly escaped in error");
    // Input is still echoed (useful debugging), just sanitized.
    let echo = j["error"]["input_echo"]["query"].as_str().unwrap();
    assert!(echo.contains("<U+202E>"));
    assert!(!echo.contains('\u{202e}'));
    // Error path also bounded: a 10KB hostile pattern must not blow up the
    // error payload past sane sizes (line cap applies).
    let bomb = format!("{}zzz", "aaaa|".repeat(2500));
    let err = call_tool(
        &engine,
        "oss_search_repos",
        Some(json!({"query": bomb, "bogus": 1})),
    )
    .unwrap_err();
    let raw = serde_json::to_string(&err.to_json()).unwrap();
    assert!(
        raw.len() < 60_000,
        "error echo for 10KB input is {} bytes — uncapped",
        raw.len()
    );
}

/// ReDoS-shaped patterns, decorated with control characters, run through
/// the escaper in bounded time (they can be echoed back by error paths).
#[test]
fn redos_patterns_bounded_through_escaper() {
    let decorated = [
        "\u{202e}(a|a)*$\u{202c}".to_string(),
        "\u{1b}[31m(x+x+)+y$\u{1b}[0m".to_string(),
        format!("\u{202e}{}zzzz$\u{202c}", "aaaa|".repeat(2500)),
        format!("{}\u{7}", "(x+)+".repeat(500)),
    ];
    for pat in &decorated {
        let mut report = SanitizeReport::default();
        let start = std::time::Instant::now();
        let out = sanitize_str(pat, &mut report);
        assert!(start.elapsed() < BOUND, "escaper exceeded bound");
        assert!(!out.contains('\u{1b}'));
        assert!(!out.contains('\u{202e}'));
        assert!(!out.contains('\u{7}'));
        assert!(!out.is_empty(), "decorated pattern fully consumed");
    }
}

/// The escaper itself must be linear on adversarial input shapes: a long
/// line of alternating ESC-sequences and a huge single line.
#[test]
fn escaper_bounded_on_control_sequence_flood() {
    let flood: String = "\u{1b}]8;;x\u{7}\u{1b}[31m".repeat(20_000);
    let mut report = SanitizeReport::default();
    let start = std::time::Instant::now();
    let out = sanitize_multiline(&flood, &mut report);
    assert!(start.elapsed() < BOUND, "escaper flood took too long");
    assert!(!out.contains('\u{1b}'));
    assert!(!out.contains('\u{7}'));
    let huge = "y".repeat(200_000);
    let start = std::time::Instant::now();
    let out = sanitize_multiline(&huge, &mut report);
    assert!(start.elapsed() < BOUND);
    assert!(out.chars().count() < 600);
}
