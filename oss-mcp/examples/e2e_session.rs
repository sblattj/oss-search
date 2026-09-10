//! E2E gate driver: run a scripted JSON-RPC session against the RELEASE
//! `oss-mcp` binary over stdio (initialize -> tools/list -> tools/call on all
//! 7 tools), assert the wire contract, and write the full transcript (raw
//! JSON-RPC lines, chronological) to `data/e2e/mcp-transcript.jsonl`.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Instant;

use serde_json::{json, Value};

const CAP: usize = 25_000;
const EXPECTED_TOOLS: [&str; 7] = [
    "oss_search_repos",
    "oss_search_code",
    "oss_repo_profile",
    "oss_repo_tree",
    "oss_fetch_file",
    "oss_fetch_docs",
    "oss_guide",
];

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn fail(msg: String) -> ! {
    eprintln!("E2E FAIL: {msg}");
    std::process::exit(1);
}

fn assert_cond(cond: bool, msg: String) {
    if !cond {
        fail(msg);
    }
}

struct Session {
    child: std::process::Child,
    stdout: BufReader<std::process::ChildStdout>,
    transcript: std::fs::File,
    next_id: u64,
    timings: Vec<serde_json::Value>,
}

impl Session {
    fn send(&mut self, req: &Value) {
        let line = serde_json::to_string(req).unwrap();
        if let Err(e) = self.child.stdin.as_mut().unwrap().write_all(line.as_bytes()) {
            fail(format!("write request {line}: {e}"));
        }
        if let Err(e) = self.child.stdin.as_mut().unwrap().write_all(b"\n") {
            fail(format!("write newline: {e}"));
        }
        if let Err(e) = writeln!(self.transcript, "{line}") {
            fail(format!("transcript write: {e}"));
        }
    }

    fn read_line(&mut self) -> String {
        let mut line = String::new();
        match self.stdout.read_line(&mut line) {
            Ok(0) => fail("server closed stdout early".to_string()),
            Ok(_) => {}
            Err(e) => fail(format!("read response: {e}")),
        }
        if let Err(e) = write!(self.transcript, "{}", line) {
            fail(format!("transcript write: {e}"));
        }
        line
    }

    fn roundtrip(&mut self, id: Value, req: &Value) -> Value {
        let t = Instant::now();
        self.send(req);
        let resp = loop {
            let line = self.read_line();
            let msg: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if msg.get("id").is_some() && msg["id"] == id {
                break msg;
            }
        };
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        if let Some(name) = req["params"]["name"].as_str() {
            self.timings.push(json!({"tool": name, "ms": (ms * 100.0).round() / 100.0}));
        }
        resp
    }
}

fn envelope_of(resp: &Value, tool: &str) -> Value {
    assert_cond(
        resp["jsonrpc"] == "2.0",
        format!("{tool}: not a jsonrpc 2.0 response"),
    );
    assert_cond(
        resp.get("error").is_none(),
        format!("{tool}: jsonrpc-level error: {}", resp["error"]),
    );
    assert_eq!(
        resp["result"]["isError"], false,
        "{tool} returned isError: {}",
        resp["result"]["content"][0]["text"]
    );
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| fail(format!("{tool}: content is not text")));
    assert_cond(
        text.len() <= CAP,
        format!("{tool}: payload {} bytes exceeds {CAP}", text.len()),
    );
    let envelope: Value = serde_json::from_str(text)
        .unwrap_or_else(|e| fail(format!("{tool}: text is not JSON: {e}")));
    assert_cond(
        envelope["results"].is_array(),
        format!("{tool}: results not array"),
    );
    assert_cond(
        envelope["total"].is_u64(),
        format!("{tool}: total not u64: {}", envelope["total"]),
    );
    assert_cond(
        envelope["has_more"].is_boolean(),
        format!("{tool}: has_more not bool"),
    );
    assert_cond(
        envelope["partial"].is_boolean(),
        format!("{tool}: partial not bool"),
    );
    assert_cond(
        envelope["truncated"].is_boolean(),
        format!("{tool}: truncated not bool"),
    );
    let backends = envelope["backend_status"]
        .as_array()
        .unwrap_or_else(|| fail(format!("{tool}: backend_status not array")));
    assert_cond(
        !backends.is_empty(),
        format!("{tool}: backend_status empty"),
    );
    for b in backends {
        assert_cond(
            b["backend"].is_string(),
            format!("{tool}: backend entry lacks backend name: {b}"),
        );
        assert_cond(
            b["status"].is_string(),
            format!("{tool}: backend entry lacks status: {b}"),
        );
    }
    assert_cond(
        envelope["warnings"].is_array(),
        format!("{tool}: warnings not array"),
    );
    envelope
}

fn main() {
    let bin = std::env::var("OSS_MCP_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|_| workspace_root().join("target/release/oss-mcp"));
    assert_cond(
        bin.is_file(),
        format!(
            "release binary missing at {}: run `cargo build --release` first",
            bin.display()
        ),
    );
    let e2e_dir = std::env::var("OSS_E2E_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| workspace_root().join("data/e2e"));
    std::fs::create_dir_all(&e2e_dir).expect("create e2e dir");

    let transcript_path = e2e_dir.join("mcp-transcript.jsonl");
    let stderr_path = e2e_dir.join("logs/mcp-stderr.log");
    std::fs::create_dir_all(stderr_path.parent().unwrap()).expect("logs dir");
    let stderr_file = std::fs::File::create(&stderr_path).expect("stderr log");

    let meta = std::fs::metadata(&bin).expect("binary metadata");
    println!(
        "spawning RELEASE binary {} ({} bytes, mtime {:?})",
        bin.display(),
        meta.len(),
        meta.modified().expect("mtime")
    );

    let mut child = Command::new(&bin)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(stderr_file))
        .spawn()
        .unwrap_or_else(|e| fail(format!("spawn {}: {e}", bin.display())));
    let stdout = BufReader::new(child.stdout.take().expect("child stdout"));

    let mut session = Session {
        transcript: std::fs::File::create(&transcript_path).expect("transcript file"),
        child,
        stdout,
        next_id: 1,
        timings: Vec::new(),
    };
    let t_session = Instant::now();

    let init_id = json!(session.next_id);
    session.next_id += 1;
    let init = session.roundtrip(
        init_id.clone(),
        &json!({
            "jsonrpc": "2.0",
            "id": init_id,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "e2e-gate", "version": "0.0.0"}
            }
        }),
    );
    assert_cond(
        init["result"]["protocolVersion"].is_string(),
        "initialize: protocolVersion".to_string(),
    );
    assert_cond(
        init["result"]["capabilities"]["tools"].is_object(),
        "initialize: capabilities.tools".to_string(),
    );
    assert_eq!(init["result"]["serverInfo"]["name"], "oss-mcp");

    session.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    let list_id = json!(session.next_id);
    session.next_id += 1;
    let list = session.roundtrip(
        list_id.clone(),
        &json!({"jsonrpc": "2.0", "id": list_id, "method": "tools/list"}),
    );
    let tools = list["result"]["tools"]
        .as_array()
        .unwrap_or_else(|| fail("tools/list: tools not array".to_string()));
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert_cond(
        names == EXPECTED_TOOLS,
        format!("tools/list names {names:?} != {EXPECTED_TOOLS:?}"),
    );
    for t in tools {
        assert_cond(
            t["description"].as_str().map(|d| d.len() > 40).unwrap_or(false),
            "tool description too short".to_string(),
        );
        assert_eq!(t["inputSchema"]["type"], "object");
        assert_eq!(t["inputSchema"]["additionalProperties"], false);
    }
    println!("tools/list: exactly 7 tools, correct names + schemas: PASS");

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
        let id = json!(format!("call:{name}"));
        let resp = session.roundtrip(
            id.clone(),
            &json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": {"name": name, "arguments": args}
            }),
        );
        let envelope = envelope_of(&resp, name);
        println!(
            "{name}: isError=false, envelope schema-valid ({} results, total {}, has_more {}), {} bytes: PASS",
            envelope["results"].as_array().unwrap().len(),
            envelope["total"],
            envelope["has_more"],
            resp["result"]["content"][0]["text"].as_str().unwrap().len()
        );
    }

    drop(session.child.stdin.take());
    let mut leftover = String::new();
    let _ = session.stdout.read_to_string(&mut leftover);
    for line in leftover.lines() {
        let _ = writeln!(session.transcript, "{line}");
    }
    let status = session.child.wait().expect("wait for child");
    assert_cond(
        status.success(),
        format!("oss-mcp exited with {status}"),
    );
    let session_ms = t_session.elapsed().as_secs_f64() * 1000.0;

    let meta_json = json!({
        "binary": bin,
        "session_ms": (session_ms * 100.0).round() / 100.0,
        "tool_calls": session.timings,
        "tools_listed": EXPECTED_TOOLS,
        "transcript": transcript_path,
    });
    let meta_path = e2e_dir.join("mcp-session.json");
    std::fs::write(&meta_path, serde_json::to_string_pretty(&meta_json).unwrap())
        .expect("write session meta");
    println!(
        "\nMCP E2E PASS: 7/7 tools schema-valid; transcript {}; meta {}",
        transcript_path.display(),
        meta_path.display()
    );
}
