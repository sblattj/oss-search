//! E2E gate driver: run all 7 tools through the RELEASE `oss-cli` binary,
//! assert each output is the same schema-valid envelope the MCP surface
//! produces, and save each pretty JSON output under `data/e2e/cli/`.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Instant;

use serde_json::{json, Value};

const CAP: usize = 25_000;
const REQUIRED_FIELDS: [&str; 7] = [
    "results",
    "total",
    "has_more",
    "partial",
    "backend_status",
    "warnings",
    "truncated",
];

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn fail(msg: String) -> ! {
    eprintln!("CLI E2E FAIL: {msg}");
    std::process::exit(1);
}

fn assert_cond(cond: bool, msg: String) {
    if !cond {
        fail(msg);
    }
}

fn check_envelope(tool: &str, envelope: &Value) {
    assert_cond(
        envelope["results"].is_array(),
        format!("{tool}: results not array"),
    );
    assert_cond(
        envelope["total"].is_u64(),
        format!("{tool}: total not u64"),
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
    assert_cond(!backends.is_empty(), format!("{tool}: backend_status empty"));
    for b in backends {
        assert_cond(
            b["backend"].is_string() && b["status"].is_string(),
            format!("{tool}: backend entry malformed: {b}"),
        );
    }
    assert_cond(
        envelope["warnings"].is_array(),
        format!("{tool}: warnings not array"),
    );
    for f in REQUIRED_FIELDS {
        assert_cond(
            envelope.get(f).is_some(),
            format!("{tool}: envelope missing field {f}"),
        );
    }
    for key in envelope.as_object().unwrap().keys() {
        assert_cond(
            key == "note" || key == "next_cursor" || REQUIRED_FIELDS.contains(&key.as_str()),
            format!("{tool}: unexpected envelope field {key}"),
        );
    }
}

fn main() {
    let bin = std::env::var("OSS_CLI_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|_| workspace_root().join("target/release/oss-cli"));
    assert_cond(
        bin.is_file(),
        format!("release binary missing at {}", bin.display()),
    );
    let e2e_dir = std::env::var("OSS_E2E_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| workspace_root().join("data/e2e"));
    let cli_dir = e2e_dir.join("cli");
    std::fs::create_dir_all(&cli_dir).expect("cli dir");

    let cases: Vec<(&str, Vec<&str>)> = vec![
        (
            "oss_search_repos",
            vec!["search-repos", "--query", "pool; calendar"],
        ),
        (
            "oss_search_code",
            vec![
                "search-code",
                "--probe",
                "special_closes",
                "--probe",
                "spawn_worker",
                "--response-format",
                "detailed",
            ],
        ),
        (
            "oss_repo_profile",
            vec!["repo-profile", "--repo", "gabe/hashopen", "--deep-docs"],
        ),
        (
            "oss_repo_tree",
            vec!["repo-tree", "--repo", "gabe/hashopen", "--glob", "*.md"],
        ),
        (
            "oss_fetch_file",
            vec![
                "fetch-file",
                "--repo",
                "gabe/hashopen",
                "--path",
                "src/market.py",
                "--line-start",
                "8",
                "--line-end",
                "16",
            ],
        ),
        (
            "oss_fetch_docs",
            vec!["fetch-docs", "--repo", "gabe/hashopen", "--pattern", "early"],
        ),
        ("oss_guide", vec!["guide"]),
    ];

    let mut summary = Vec::new();
    for (i, (tool, args)) in cases.iter().enumerate() {
        let t = Instant::now();
        let out = Command::new(&bin)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap_or_else(|e| fail(format!("run oss-cli {args:?}: {e}")));
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        assert_cond(
            out.status.success(),
            format!(
                "oss-cli {args:?} exited {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            ),
        );
        let envelope: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| fail(format!("{tool}: stdout not JSON: {e}")));
        let bytes = serde_json::to_vec(&envelope).unwrap().len();
        assert_cond(
            bytes <= CAP,
            format!("{tool}: payload {bytes} bytes exceeds {CAP}"),
        );
        check_envelope(tool, &envelope);
        let out_path = cli_dir.join(format!("{:02}-{}.json", i + 1, tool));
        std::fs::write(&out_path, &out.stdout).expect("write cli output");
        summary.push(json!({
            "tool": tool,
            "args": args,
            "exit": out.status.code(),
            "ms": (ms * 100.0).round() / 100.0,
            "results": envelope["results"].as_array().unwrap().len(),
            "total": envelope["total"],
            "has_more": envelope["has_more"],
            "backend_status": envelope["backend_status"],
            "output": out_path,
        }));
        println!(
            "{tool}: exit 0, envelope schema-valid ({} results, total {}), {} bytes -> {} : PASS",
            envelope["results"].as_array().unwrap().len(),
            envelope["total"],
            bytes,
            out_path.display()
        );
    }

    let summary_path = cli_dir.join("summary.json");
    let mut f = std::fs::File::create(&summary_path).expect("summary file");
    writeln!(
        f,
        "{}",
        serde_json::to_string_pretty(&json!({
            "binary": bin,
            "cases": summary,
            "schema_fields_required": REQUIRED_FIELDS,
        }))
        .unwrap()
    )
    .expect("write summary");
    println!(
        "\nCLI E2E PASS: 7/7 tools schema-valid via release oss-cli; outputs in {}",
        cli_dir.display()
    );
}
