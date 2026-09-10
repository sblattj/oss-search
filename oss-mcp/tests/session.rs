use std::io::{Read, Write};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

const CAP: usize = 25_000;

/// Pin OSS_SEARCH_HOME to a directory that never contains an index so the
/// golden sessions deterministically exercise the stub engine regardless
/// of any real hot-set built under ~/.cache/oss-search.
const NO_HOTSET_HOME: &str = concat!(env!("CARGO_TARGET_TMPDIR"), "/oss-mcp-golden-no-hotset");

fn session(requests: &[Value]) -> Vec<Value> {
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
    out.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|m| m.get("id").is_some())
        .collect()
}

fn initialize_params() -> Value {
    json!({
        "protocolVersion": "2025-06-18",
        "capabilities": {},
        "clientInfo": {"name": "golden-test", "version": "0.0.0"}
    })
}

fn call(name: &str, arguments: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": format!("call:{name}"),
        "method": "tools/call",
        "params": {"name": name, "arguments": arguments}
    })
}

fn by_id(responses: &[Value], id: Value) -> Value {
    responses
        .iter()
        .find(|m| m["id"] == id)
        .cloned()
        .unwrap_or_else(|| panic!("no response with id {id}"))
}

fn envelope_of(resp: &Value, tool: &str) -> Value {
    assert_eq!(
        resp["result"]["isError"], false,
        "{tool} returned isError: {}",
        resp["result"]["content"][0]["text"]
    );
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("{tool} content is not text"));
    assert!(
        text.len() <= CAP,
        "{tool} payload {} bytes exceeds {CAP}",
        text.len()
    );
    let envelope: Value = serde_json::from_str(text)
        .unwrap_or_else(|e| panic!("{tool} text is not JSON: {e}"));
    assert!(envelope["results"].is_array(), "{tool} results");
    assert!(envelope["total"].is_u64(), "{tool} total");
    assert!(envelope["has_more"].is_boolean(), "{tool} has_more");
    assert!(envelope["partial"].is_boolean(), "{tool} partial");
    assert!(envelope["truncated"].is_boolean(), "{tool} truncated");
    let backends = envelope["backend_status"]
        .as_array()
        .unwrap_or_else(|| panic!("{tool} backend_status"));
    assert!(!backends.is_empty(), "{tool} backend_status empty");
    for b in backends {
        assert!(b["backend"].is_string(), "{tool} backend name");
        assert!(b["status"].is_string(), "{tool} backend status");
    }
    assert!(envelope["warnings"].is_array(), "{tool} warnings");
    envelope
}

#[test]
fn golden_session_all_seven_tools() {
    let mut requests = vec![
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": initialize_params()}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    ];
    let calls: Vec<(&str, Value)> = vec![
        ("oss_search_repos", json!({"query": "pool; calendar"})),
        ("oss_search_code", json!({"probes": ["special_closes", "spawn_worker"], "response_format": "detailed"})),
        ("oss_repo_profile", json!({"repo": "gabe/hashopen", "deep_docs": true})),
        ("oss_repo_tree", json!({"repo": "gabe/hashopen", "glob": "*.md"})),
        ("oss_fetch_file", json!({"repo": "gabe/hashopen", "path": "src/market.py", "line_range": {"start": 8, "end": 16}})),
        ("oss_fetch_docs", json!({"repo": "gabe/hashopen", "pattern": "early"})),
        ("oss_guide", json!({})),
    ];
    for (name, args) in &calls {
        requests.push(call(name, args.clone()));
    }
    let responses = session(&requests);

    let init = by_id(&responses, json!(1));
    assert_eq!(init["jsonrpc"], "2.0");
    assert!(init["result"]["protocolVersion"].is_string());
    assert!(init["result"]["capabilities"]["tools"].is_object());
    assert_eq!(init["result"]["serverInfo"]["name"], "oss-mcp");

    let list = by_id(&responses, json!(2));
    let tools = list["result"]["tools"]
        .as_array()
        .expect("tools array");
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert_eq!(
        names,
        vec![
            "oss_search_repos",
            "oss_search_code",
            "oss_repo_profile",
            "oss_repo_tree",
            "oss_fetch_file",
            "oss_fetch_docs",
            "oss_guide",
        ]
    );
    for t in tools {
        assert!(t["description"].as_str().unwrap().len() > 40);
        assert_eq!(t["inputSchema"]["type"], "object");
        assert_eq!(t["inputSchema"]["additionalProperties"], false);
    }
    let guide_schema = tools
        .iter()
        .find(|t| t["name"] == "oss_guide")
        .unwrap()["inputSchema"]
        .clone();
    assert_eq!(guide_schema["properties"], json!({}));

    for (name, _) in &calls {
        let id = format!("call:{name}");
        let resp = by_id(&responses, Value::String(id));
        envelope_of(&resp, name);
    }
}

#[test]
fn unknown_param_is_typed_error_echoing_input() {
    let requests = vec![
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": initialize_params()}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        call("oss_search_repos", json!({"query": "pool", "bogus": 1})),
    ];
    let responses = session(&requests);
    let resp = by_id(&responses, json!("call:oss_search_repos"));
    assert_eq!(resp["result"]["isError"], true);
    let text = resp["result"]["content"][0]["text"].as_str().unwrap();
    let err: Value = serde_json::from_str(text).expect("error json");
    assert_eq!(err["error"]["code"], "unknown_field");
    assert_eq!(err["error"]["input_echo"]["query"], "pool");
    assert_eq!(err["error"]["input_echo"]["bogus"], 1);
    let known: Vec<&str> = err["error"]["known_fields"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(known.contains(&"query"));
    assert!(!known.contains(&"bogus"));
    assert!(err["error"]["message"]
        .as_str()
        .unwrap()
        .contains("unknown field `bogus`"));
    assert!(err["error"]["correction"]
        .as_str()
        .unwrap()
        .contains("oss_guide"));
}

#[test]
fn unknown_tool_is_typed_error() {
    let requests = vec![
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": initialize_params()}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        call("oss_nope", json!({})),
    ];
    let responses = session(&requests);
    let resp = by_id(&responses, json!("call:oss_nope"));
    assert_eq!(resp["result"]["isError"], true);
    let err: Value =
        serde_json::from_str(resp["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(err["error"]["code"], "unknown_tool");
    assert!(err["error"]["message"]
        .as_str()
        .unwrap()
        .contains("oss_search_repos"));
    assert!(err["error"]["correction"]
        .as_str()
        .unwrap()
        .contains("oss_guide"));
}

#[test]
fn sanitized_fixture_visible_through_mcp() {
    let requests = vec![
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": initialize_params()}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        call(
            "oss_fetch_file",
            json!({"repo": "helix/edge-sdk", "path": "packages/core/src/vendor/note.md"}),
        ),
    ];
    let responses = session(&requests);
    let resp = by_id(&responses, json!("call:oss_fetch_file"));
    let envelope = envelope_of(&resp, "oss_fetch_file");
    let content = envelope["results"][0]["content"].as_str().unwrap();
    assert!(!content.contains('\u{1b}'));
    assert!(!content.contains('\u{202e}'));
    assert!(content.contains("<U+202E>"));
    assert!(envelope["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|w| w.as_str().unwrap().contains("bidi")));
}

#[test]
fn snippets_mode_uses_line_locators() {
    let requests = vec![
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": initialize_params()}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        call(
            "oss_search_code",
            json!({"probes": ["special_closes"], "mode": "snippets", "response_format": "detailed"}),
        ),
    ];
    let responses = session(&requests);
    let resp = by_id(&responses, json!("call:oss_search_code"));
    let envelope = envelope_of(&resp, "oss_search_code");
    let row = &envelope["results"][0];
    let locator = row["locator"].as_str().unwrap();
    assert!(locator.contains(":L"), "locator {locator}");
    assert!(locator.starts_with("src/") || locator.contains(".py"), "locator {locator}");
    assert!(row["lines"].as_array().unwrap().len() >= 3);
    assert!(row["lines"].as_array().unwrap().len() <= 11);
}
