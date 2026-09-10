//! Security-hardening tests: the escaper fixture through the REAL MCP
//! tool-call path — spawn the compiled oss-mcp binary over stdio (like the
//! golden session tests), fetch the escape-lab fixture, and assert on the
//! RAW response bytes: no ESC character, no BEL, no raw bidi overrides,
//! plus visible markers and a sanitization warning.

use std::io::{Read, Write};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

const FIXTURE_REPO: &str = "helix/edge-sdk";
const FIXTURE_PATH: &str = "packages/core/src/vendor/escape-lab.md";

/// Pin OSS_SEARCH_HOME to a directory that never contains an index so
/// these sessions deterministically exercise the stub engine regardless
/// of any real hot-set built under ~/.cache/oss-search.
const NO_HOTSET_HOME: &str = concat!(env!("CARGO_TARGET_TMPDIR"), "/oss-mcp-sec-no-hotset");

fn session_raw(requests: &[Value]) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_oss-mcp"))
        .env("OSS_SEARCH_HOME", NO_HOTSET_HOME)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn oss-mcp");
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        for req in requests {
            stdin
                .write_all(serde_json::to_string(req).unwrap().as_bytes())
                .unwrap();
            stdin.write_all(b"\n").unwrap();
        }
    }
    drop(child.stdin.take());
    let mut out = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut out)
        .unwrap();
    let status = child.wait().unwrap();
    assert!(status.success(), "oss-mcp exited with {status}");
    out
}

fn init_requests() -> Vec<Value> {
    vec![
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "security-test", "version": "0.0.0"}
            }
        }),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({
            "jsonrpc": "2.0",
            "id": "call:fetch",
            "method": "tools/call",
            "params": {
                "name": "oss_fetch_file",
                "arguments": {"repo": FIXTURE_REPO, "path": FIXTURE_PATH}
            }
        }),
    ]
}

#[test]
fn raw_response_bytes_carry_no_control_or_bidi_characters() {
    let raw = session_raw(&init_requests());
    // Assert on the raw wire bytes BEFORE any parsing: JSON string escapes
    // would otherwise hide the raw bytes we are policing for.
    assert!(
        !raw.contains('\u{1b}'),
        "raw ESC byte on the wire: {:?}",
        raw.match_indices('\u{1b}').count()
    );
    assert!(!raw.contains('\u{7}'), "raw BEL byte on the wire");
    assert!(!raw.contains('\u{202e}'), "raw RLO on the wire");
    assert!(!raw.contains('\u{202c}'), "raw PDF on the wire");
    assert!(!raw.contains('\u{2066}'), "raw LRI on the wire");
    assert!(!raw.contains('\u{2069}'), "raw PDI on the wire");

    // And the escaped, visible forms plus the warning are present.
    let resp = raw
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|m| m["id"] == "call:fetch")
        .expect("fetch response");
    assert_eq!(resp["result"]["isError"], false);
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let envelope: Value = serde_json::from_str(text).unwrap();
    let content = envelope["results"][0]["content"].as_str().unwrap();
    assert!(content.contains("<U+202E>"), "RLO marker missing");
    assert!(content.contains("<U+2066>"), "LRI marker missing");
    assert!(content.contains("chars truncated]"), "line-cap marker missing");
    let warnings = envelope["warnings"].as_array().unwrap();
    assert!(warnings
        .iter()
        .any(|w| w.as_str().unwrap().contains("bidi")));
    assert!(warnings
        .iter()
        .any(|w| w.as_str().unwrap().contains("capped")));
}

#[test]
fn hostile_error_echo_is_sanitized_on_the_wire() {
    let mut requests = init_requests();
    // Replace the fetch with a hostile-echo error call: unknown field makes
    // the input echo back through the error JSON path.
    requests[2] = json!({
        "jsonrpc": "2.0",
        "id": "call:err",
        "method": "tools/call",
        "params": {
            "name": "oss_search_repos",
            "arguments": {"query": "\u{202e}rdm\u{202c}\u{1b}[31m", "bogus": 1}
        }
    });
    let raw = session_raw(&requests);
    assert!(
        !raw.contains('\u{1b}'),
        "raw ESC byte in error response on the wire"
    );
    assert!(!raw.contains('\u{202e}'), "raw RLO in error response");
    let resp = raw
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|m| m["id"] == "call:err")
        .expect("error response");
    assert_eq!(resp["result"]["isError"], true);
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("<U+202E>"), "RLO not visibly escaped in error");
}
