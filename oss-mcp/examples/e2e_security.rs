//! E2E security gate: pipe hostile control/bidi bytes (ESC[31m + U+202E)
//! through the RELEASE `oss-mcp` binary and police the RAW response bytes.
//! Two paths are exercised: the typed-error path (hostile query echoed back
//! in input_echo) and the success path (fetch of the escape-lab fixture).
//! Raw stdout bytes are saved verbatim under `data/e2e/security/`.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::{json, Value};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn fail(msg: String) -> ! {
    eprintln!("SECURITY E2E FAIL: {msg}");
    std::process::exit(1);
}

fn assert_cond(cond: bool, msg: String) {
    if !cond {
        fail(msg);
    }
}

const HOSTILE_QUERY: &str = "\u{202e}rdm\u{202c}\u{1b}[31m";

fn count(haystack: &[u8], needle: &str) -> usize {
    let needle = needle.as_bytes();
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}

fn main() {
    let bin = std::env::var("OSS_MCP_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|_| workspace_root().join("target/release/oss-mcp"));
    assert_cond(
        bin.is_file(),
        format!("release binary missing at {}", bin.display()),
    );
    let e2e_dir = std::env::var("OSS_E2E_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| workspace_root().join("data/e2e"));
    let sec_dir = e2e_dir.join("security");
    std::fs::create_dir_all(&sec_dir).expect("security dir");

    let requests: Vec<Value> = vec![
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "e2e-security", "version": "0.0.0"}
            }
        }),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({
            "jsonrpc": "2.0",
            "id": "call:hostile-error-echo",
            "method": "tools/call",
            "params": {
                "name": "oss_search_repos",
                "arguments": {"query": HOSTILE_QUERY, "bogus": 1}
            }
        }),
        json!({
            "jsonrpc": "2.0",
            "id": "call:fixture-content",
            "method": "tools/call",
            "params": {
                "name": "oss_fetch_file",
                "arguments": {
                    "repo": "helix/edge-sdk",
                    "path": "packages/core/src/vendor/escape-lab.md"
                }
            }
        }),
    ];

    let mut child = Command::new(&bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| fail(format!("spawn {}: {e}", bin.display())));
    {
        let stdin = child.stdin.as_mut().unwrap();
        for req in &requests {
            stdin
                .write_all(serde_json::to_string(req).unwrap().as_bytes())
                .unwrap();
            stdin.write_all(b"\n").unwrap();
        }
    }
    drop(child.stdin.take());
    let mut raw = Vec::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut raw)
        .expect("read raw stdout");
    let status = child.wait().expect("wait");
    assert_cond(status.success(), format!("oss-mcp exited with {status}"));

    let raw_path = sec_dir.join("hostile-query-raw-response.txt");
    std::fs::write(&raw_path, &raw).expect("save raw response bytes");
    let repro_path = sec_dir.join("hostile-query.json");
    std::fs::write(
        &repro_path,
        serde_json::to_string_pretty(&json!({
            "request": "tools/call oss_search_repos {query: <RLO>rdm<PDF>ESC[31m, bogus: 1}",
            "hostile_query_codepoints": ["U+202E RLO", "U+202C PDF", "U+001B ESC", "[31m"],
            "second_call": "tools/call oss_fetch_file helix/edge-sdk escape-lab.md (fixture content carries ESC/BEL/RLO/LRI/PDI bytes)"
        }))
        .unwrap(),
    )
    .expect("save repro");

    let mut verdict = serde_json::Map::new();
    verdict.insert("raw_bytes".into(), json!(raw.len()));
    let mut all_zero = true;
    for (label, needle) in [
        ("ESC_u001b", "\u{1b}"),
        ("BEL_u0007", "\u{7}"),
        ("RLO_u202E", "\u{202e}"),
        ("PDF_u202C", "\u{202c}"),
        ("LRI_u2066", "\u{2066}"),
        ("PDI_u2069", "\u{2069}"),
    ] {
        let n = count(&raw, needle);
        verdict.insert(label.to_string(), json!(n));
        if n != 0 {
            all_zero = false;
        }
    }
    assert_cond(
        all_zero,
        format!("hostile bytes survived to the wire: {verdict:?}"),
    );

    let mut by_id: Vec<(String, Value)> = Vec::new();
    for line in String::from_utf8_lossy(&raw).lines() {
        if let Ok(msg) = serde_json::from_str::<Value>(line)
            && let Some(id) = msg.get("id").and_then(|v| v.as_str())
        {
            by_id.push((id.to_string(), msg));
        }
    }
    let err_resp = by_id
        .iter()
        .find(|(id, _)| id == "call:hostile-error-echo")
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| fail("no hostile-error-echo response".to_string()));
    assert_eq!(err_resp["result"]["isError"], true);
    let err_text = err_resp["result"]["content"][0]["text"].as_str().unwrap();
    assert_cond(
        err_text.contains("<U+202E>"),
        "RLO not visibly escaped in error echo".to_string(),
    );
    let err_payload: Value = serde_json::from_str(err_text).unwrap();
    verdict.insert(
        "error_path_isError".into(),
        err_resp["result"]["isError"].clone(),
    );
    verdict.insert(
        "error_path_code".into(),
        err_payload["error"]["code"].clone(),
    );
    verdict.insert(
        "error_path_rlo_visible_marker".into(),
        json!(err_text.contains("<U+202E>")),
    );

    let fix_resp = by_id
        .iter()
        .find(|(id, _)| id == "call:fixture-content")
        .map(|(_, v)| v.clone())
        .unwrap_or_else(|| fail("no fixture-content response".to_string()));
    assert_eq!(fix_resp["result"]["isError"], false);
    let fix_text = fix_resp["result"]["content"][0]["text"].as_str().unwrap();
    let envelope: Value = serde_json::from_str(fix_text).unwrap();
    let content = envelope["results"][0]["content"].as_str().unwrap();
    assert_cond(
        content.contains("<U+202E>"),
        "RLO marker missing in fixture".to_string(),
    );
    assert_cond(
        content.contains("<U+2066>"),
        "LRI marker missing in fixture".to_string(),
    );
    assert_cond(
        content.contains("chars truncated]"),
        "line-cap marker missing in fixture".to_string(),
    );
    let warnings = envelope["warnings"].as_array().unwrap();
    assert_cond(
        warnings.iter().any(|w| w.as_str().unwrap().contains("bidi")),
        "bidi warning missing".to_string(),
    );
    assert_cond(
        warnings.iter().any(|w| w.as_str().unwrap().contains("capped")),
        "line-cap warning missing".to_string(),
    );
    verdict.insert(
        "success_path_markers".into(),
        json!(["<U+202E>", "<U+2066>", "chars truncated]"]),
    );
    verdict.insert("verdict".into(), json!("PASS"));

    let verdict_path = sec_dir.join("verdict.json");
    std::fs::write(
        &verdict_path,
        serde_json::to_string_pretty(&Value::Object(verdict)).unwrap(),
    )
    .expect("write verdict");
    println!(
        "SECURITY E2E PASS: {} raw response bytes policed — 0x1b x0, U+202E x0, U+202C x0, U+2066 x0, U+2069 x0, BEL x0; visible markers + warnings present; raw bytes at {}",
        raw.len(),
        raw_path.display()
    );
}
