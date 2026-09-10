use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use oss_core::{call_tool, StubEngine};
use serde_json::{json, Value};

fn run_cli(args: &[&str]) -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_oss-cli"))
        .args(args)
        .output()
        .expect("run oss-cli");
    assert!(
        out.status.success(),
        "oss-cli {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("cli stdout is JSON")
}

fn expected(tool: &str, args: Value) -> Value {
    call_tool(&StubEngine::new(), tool, Some(args)).expect("in-process call succeeds")
}

fn mcp_binary() -> Option<PathBuf> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../target/debug/oss-mcp");
    p.exists().then_some(p)
}

fn mcp_call(name: &str, args: Value) -> Value {
    let binary = mcp_binary().expect(
        "oss-mcp binary not built; run `cargo test -p oss-mcp -p oss-cli -p oss-core` so both binaries exist",
    );
    let mut child = Command::new(binary)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn oss-mcp");
    {
        let stdin = child.stdin.as_mut().unwrap();
        let requests = vec![
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "cli-parity", "version": "0"}}}),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": name, "arguments": args}}),
        ];
        for r in requests {
            stdin
                .write_all(serde_json::to_string(&r).unwrap().as_bytes())
                .unwrap();
            stdin.write_all(b"\n").unwrap();
        }
    }
    drop(child.stdin.take());
    let mut out = String::new();
    child.stdout.take().unwrap().read_to_string(&mut out).unwrap();
    child.wait().unwrap();
    for line in out.lines() {
        if let Ok(msg) = serde_json::from_str::<Value>(line)
            && msg.get("id").is_some() && msg["id"] == json!(2) {
                let text = msg["result"]["content"][0]["text"].as_str().unwrap();
                return serde_json::from_str(text).expect("mcp text is envelope JSON");
            }
    }
    panic!("no tools/call response from oss-mcp for {name}");
}

#[test]
fn search_repos_snapshot_matches_dispatch() {
    let cli = run_cli(&[
        "search-repos",
        "--query",
        "pool; calendar",
        "--language",
        "Rust",
        "--min-stars",
        "100",
        "--sort",
        "stars",
        "--response-format",
        "detailed",
    ]);
    let exp = expected(
        "oss_search_repos",
        json!({"query": "pool; calendar", "language": "Rust", "min_stars": 100, "sort": "stars", "response_format": "detailed"}),
    );
    assert_eq!(cli, exp);
}

#[test]
fn search_code_snapshot_matches_dispatch() {
    let cli = run_cli(&[
        "search-code",
        "--probe",
        "special_closes",
        "--pattern",
        "spawn_worker",
        "--response-format",
        "concise",
    ]);
    let exp = expected(
        "oss_search_code",
        json!({"probes": ["special_closes", "spawn_worker"], "response_format": "concise"}),
    );
    assert_eq!(cli, exp);
}

#[test]
fn search_code_snippets_snapshot_matches_dispatch() {
    let cli = run_cli(&[
        "search-code",
        "--probe",
        "special_closes",
        "--mode",
        "snippets",
        "--snippets-per-repo",
        "1",
        "--response-format",
        "detailed",
    ]);
    let exp = expected(
        "oss_search_code",
        json!({"probes": ["special_closes"], "mode": "snippets", "snippets_per_repo": 1, "response_format": "detailed"}),
    );
    assert_eq!(cli, exp);
}

#[test]
fn repo_profile_snapshot_matches_dispatch() {
    let cli = run_cli(&["repo-profile", "--repo", "gabe/hashopen", "--deep-docs"]);
    let exp = expected("oss_repo_profile", json!({"repo": "gabe/hashopen", "deep_docs": true}));
    assert_eq!(cli, exp);
}

#[test]
fn repo_tree_snapshot_matches_dispatch() {
    let cli = run_cli(&[
        "repo-tree",
        "--repo",
        "gabe/hashopen",
        "--ref",
        "main",
        "--path-prefix",
        "src/",
        "--limit",
        "10",
    ]);
    let exp = expected(
        "oss_repo_tree",
        json!({"repo": "gabe/hashopen", "ref": "main", "path_prefix": "src/", "limit": 10}),
    );
    assert_eq!(cli, exp);
}

#[test]
fn fetch_file_snapshot_matches_dispatch() {
    let cli = run_cli(&[
        "fetch-file",
        "--repo",
        "gabe/hashopen",
        "--path",
        "src/market.py",
        "--line-start",
        "8",
        "--line-end",
        "16",
    ]);
    let exp = expected(
        "oss_fetch_file",
        json!({"repo": "gabe/hashopen", "path": "src/market.py", "line_range": {"start": 8, "end": 16}}),
    );
    assert_eq!(cli, exp);
}

#[test]
fn fetch_docs_snapshot_matches_dispatch() {
    let cli = run_cli(&["fetch-docs", "--repo", "gabe/hashopen", "--pattern", "early close"]);
    let exp = expected(
        "oss_fetch_docs",
        json!({"repo": "gabe/hashopen", "pattern": "early close"}),
    );
    assert_eq!(cli, exp);
}

#[test]
fn guide_snapshot_matches_dispatch() {
    let cli = run_cli(&["guide"]);
    let exp = expected("oss_guide", json!({}));
    assert_eq!(cli, exp);
}

#[test]
fn cli_error_is_typed_json_on_stderr() {
    let out = Command::new(env!("CARGO_BIN_EXE_oss-cli"))
        .args(["search-repos", "--query", "x", "--limit", "999"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err: Value = serde_json::from_slice(&out.stderr).expect("stderr error JSON");
    assert_eq!(err["error"]["code"], "invalid_value");
    assert!(err["error"]["message"].as_str().unwrap().contains("999"));
}

#[test]
fn cli_parity_with_mcp_all_seven_tools() {
    if mcp_binary().is_none() {
        eprintln!("SKIP cli_parity_with_mcp_all_seven_tools: oss-mcp binary not built yet");
        return;
    }
    let cases: Vec<(&str, Vec<&str>, Value)> = vec![
        ("oss_search_repos", vec!["search-repos", "--query", "calendar"], json!({"query": "calendar"})),
        ("oss_search_code", vec!["search-code", "--probe", "spawn_worker"], json!({"probes": ["spawn_worker"]})),
        ("oss_repo_profile", vec!["repo-profile", "--repo", "ferrum/poolite"], json!({"repo": "ferrum/poolite"})),
        ("oss_repo_tree", vec!["repo-tree", "--repo", "ferrum/poolite"], json!({"repo": "ferrum/poolite"})),
        ("oss_fetch_file", vec!["fetch-file", "--repo", "ferrum/poolite", "--path", "src/pool.rs"], json!({"repo": "ferrum/poolite", "path": "src/pool.rs"})),
        ("oss_fetch_docs", vec!["fetch-docs", "--repo", "helix/edge-sdk", "--pattern", "ratelimit"], json!({"repo": "helix/edge-sdk", "pattern": "ratelimit"})),
        ("oss_guide", vec!["guide"], json!({})),
    ];
    for (tool, cli_args, args) in cases {
        let cli = run_cli(&cli_args);
        let mcp = mcp_call(tool, args);
        assert_eq!(cli, mcp, "CLI and MCP payloads differ for {tool}");
    }
}
