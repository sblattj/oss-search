//! Hot-set engine sessions over the REAL stdio MCP wire: build a tiny
//! `TrigramIndex` in a temp home, point `OSS_SEARCH_HOME` at it, spawn the
//! compiled `oss-mcp`, and assert that auto-detection serves local results
//! attributed `backend=local_index` — plus the no-hot-set path serving the
//! stub engine with a guide that names the build command. No network.

use std::io::{Read, Write};
use std::process::{Command, Stdio};

use oss_index::TrigramIndex;
use serde_json::{json, Value};

const NEEDLE: &str = "hotset_fixture_needle";

/// A temp home containing a 3-doc hot-set at <home>/index.
fn hotset_home() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    let index = TrigramIndex::build(vec![
        (
            "fixture/hotset-demo".into(),
            "src/lib.rs".into(),
            format!("pub fn {NEEDLE}() -> u32 {{ 7 }}\nfn other() {{}}\n").into_bytes(),
        ),
        (
            "fixture/hotset-demo".into(),
            "README.md".into(),
            format!("# hotset demo\nDocs mention {NEEDLE} usage.\n").into_bytes(),
        ),
        (
            "fixture/plain".into(),
            "src/other.rs".into(),
            b"fn unrelated_thing() {}\n".to_vec(),
        ),
    ])
    .unwrap();
    index.save(&home.path().join("index")).unwrap();
    home
}

/// An existing temp home with NO index child (the no-hot-set path).
fn empty_home() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

/// Spawn the compiled oss-mcp with OSS_SEARCH_HOME=home (plus optional
/// extra args), drive one JSON-RPC session, return id-keyed responses.
fn session(home: &std::path::Path, args: &[&str], requests: &[Value]) -> Vec<Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_oss-mcp"))
        .args(args)
        .env("OSS_SEARCH_HOME", home)
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

fn init_requests(extra_calls: &[(&str, Value)]) -> Vec<Value> {
    let mut requests = vec![
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "hotset-test", "version": "0.0.0"}
            }
        }),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    ];
    for (name, arguments) in extra_calls {
        requests.push(json!({
            "jsonrpc": "2.0",
            "id": format!("call:{name}"),
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        }));
    }
    requests
}

fn by_id(responses: &[Value], id: &str) -> Value {
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
    serde_json::from_str(text)
        .unwrap_or_else(|e| panic!("{tool} text is not JSON: {e}"))
}

#[test]
fn auto_detected_hotset_serves_local_results_over_stdio() {
    let home = hotset_home();
    let requests = vec![
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "hotset-test", "version": "0.0.0"}
            }
        }),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({
            "jsonrpc": "2.0", "id": "call:search",
            "method": "tools/call",
            "params": {"name": "oss_search_code", "arguments": {
                "probes": [NEEDLE], "response_format": "detailed"
            }}
        }),
        json!({
            "jsonrpc": "2.0", "id": "call:tree",
            "method": "tools/call",
            "params": {"name": "oss_repo_tree", "arguments": {"repo": "fixture/hotset-demo"}}
        }),
        json!({
            "jsonrpc": "2.0", "id": "call:fetch",
            "method": "tools/call",
            "params": {"name": "oss_fetch_file", "arguments": {
                "repo": "fixture/hotset-demo", "path": "src/lib.rs"
            }}
        }),
        json!({"jsonrpc": "2.0", "id": "call:guide", "method": "tools/call",
               "params": {"name": "oss_guide", "arguments": {}}}),
    ];
    let responses = session(home.path(), &[], &requests);

    // 1. Local search hit with backend=local_index attribution (repos mode).
    let env = envelope_of(&by_id(&responses, "call:search"), "oss_search_code");
    let row = &env["results"][0];
    assert_eq!(row["repo"], "fixture/hotset-demo");
    let backends: Vec<&str> = row["backends"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|b| b.as_str())
        .collect();
    assert_eq!(backends, vec!["local_index"], "row attribution: {row}");
    let names: Vec<&str> = env["backend_status"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["backend"].as_str())
        .collect();
    assert!(names.contains(&"local_index"), "statuses: {names:?}");

    // 2. Repo tree serves the snapshot.
    let tree = envelope_of(&by_id(&responses, "call:tree"), "oss_repo_tree");
    let paths: Vec<&str> = tree["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|p| p["path"].as_str())
        .collect();
    assert_eq!(paths, vec!["README.md", "src/lib.rs"]);

    // 3. fetch_file serves snapshot bytes.
    let file = envelope_of(&by_id(&responses, "call:fetch"), "oss_fetch_file");
    assert!(file["results"][0]["content"]
        .as_str()
        .unwrap()
        .contains(NEEDLE));

    // 4. Guide reports the built index stats.
    let guide = envelope_of(&by_id(&responses, "call:guide"), "oss_guide");
    assert_eq!(guide["results"][0]["engine"], "hotset");
    assert_eq!(guide["results"][0]["hotset"]["status"], "served");
    assert_eq!(guide["results"][0]["hotset"]["docs"], 3);
}

#[test]
fn hotset_snippets_and_stub_probe_negative_over_stdio() {
    let home = hotset_home();
    let requests = vec![
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "hotset-test", "version": "0.0.0"}
            }
        }),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({
            "jsonrpc": "2.0",
            "id": "call:snippets",
            "method": "tools/call",
            "params": {"name": "oss_search_code", "arguments": {
                "probes": [NEEDLE], "mode": "snippets", "response_format": "detailed"
            }}
        }),
        json!({
            "jsonrpc": "2.0",
            "id": "call:stubprobe",
            "method": "tools/call",
            "params": {"name": "oss_search_code", "arguments": {
                "probes": ["special_closes"], "response_format": "detailed"
            }}
        }),
    ];
    let responses = session(home.path(), &[], &requests);

    let snip = envelope_of(&by_id(&responses, "call:snippets"), "oss_search_code");
    let rows = snip["results"].as_array().unwrap();
    let row = rows
        .iter()
        .find(|r| r["path"] == "src/lib.rs")
        .unwrap_or_else(|| panic!("no src/lib.rs snippet row: {rows:?}"));
    assert!(
        row["locator"].as_str().unwrap().starts_with("src/lib.rs:L"),
        "locator: {}",
        row["locator"]
    );
    assert_eq!(row["backends"][0], "local_index");

    // The stub fixture idiom is a true negative against the hot-set.
    let neg = envelope_of(&by_id(&responses, "call:stubprobe"), "oss_search_code");
    assert_eq!(neg["total"], 0);
    assert!(neg["results"].as_array().unwrap().is_empty());
    assert_eq!(neg["partial"], false);
}

#[test]
fn no_hotset_serves_stub_and_guide_names_build_command() {
    let home = empty_home();
    let calls: Vec<(&str, Value)> = vec![
        (
            "oss_search_code",
            json!({"probes": ["special_closes"], "response_format": "detailed"}),
        ),
        ("oss_guide", json!({})),
    ];
    let responses = session(home.path(), &[], &init_requests(&calls));

    // Stub corpus serves.
    let search = envelope_of(&by_id(&responses, "call:oss_search_code"), "oss_search_code");
    assert_eq!(search["results"][0]["repo"], "gabe/hashopen");

    // Guide includes the exact hot-set build command.
    let guide = envelope_of(&by_id(&responses, "call:oss_guide"), "oss_guide");
    let row = &guide["results"][0];
    assert_eq!(row["hotset"]["status"], "not_served");
    assert_eq!(row["hotset"]["build_command"], "oss-cli hotset build");
}

#[test]
fn explicit_engine_stub_wins_over_built_hotset() {
    let home = hotset_home();
    let calls: Vec<(&str, Value)> = vec![(
        "oss_search_code",
        json!({"probes": ["special_closes"], "response_format": "detailed"}),
    )];
    let responses = session(home.path(), &["--engine", "stub"], &init_requests(&calls));
    let search = envelope_of(&by_id(&responses, "call:oss_search_code"), "oss_search_code");
    assert_eq!(
        search["results"][0]["repo"], "gabe/hashopen",
        "--engine stub must override hot-set auto-detection"
    );
}

#[test]
fn explicit_engine_hotset_without_index_exits_with_usage_error() {
    let home = empty_home();
    let child = Command::new(env!("CARGO_BIN_EXE_oss-mcp"))
        .args(["--engine", "hotset"])
        .env("OSS_SEARCH_HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn oss-mcp");
    let out = child.wait_with_output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "--engine hotset without an index must exit 2\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("oss-cli hotset build"),
        "stderr must name the build command:\n{stderr}"
    );
}
