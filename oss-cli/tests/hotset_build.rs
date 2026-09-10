//! Integration test for `oss-cli hotset build`: a tiny hot-set built from
//! local git fixture repos over file:// origins (no network, no model
//! downloads), then a second run proving resumability via skip output and
//! an untouched state file.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use oss_index::{Filters, TrigramIndex};
use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_oss-cli");

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.com")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.com")
        .output()
        .expect("git spawns");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

fn fixture_repo(dir: &Path, files: &[(&str, &str)]) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q", "-b", "main", "."]);
    git(dir, &["config", "uploadpack.allowFilter", "true"]);
    git(dir, &["config", "uploadpack.allowAnySHA1InWant", "true"]);
    for (path, contents) in files {
        let target = dir.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(target, contents).unwrap();
    }
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "fixture c1"]);
}

fn file_url(dir: &Path) -> String {
    format!("file://{}", dir.canonicalize().unwrap().display())
}

struct RunOutcome {
    code: Option<i32>,
    stdout: String,
    stderr: String,
    elapsed: Duration,
}

fn build(home: &Path, repos: &str, extra: &[&str]) -> RunOutcome {
    let mut cmd = Command::new(BIN);
    cmd.args([
        "hotset", "build", "--repos", repos, "--home",
        home.to_str().unwrap(), "--concurrency", "2",
    ])
    .args(extra)
    .env_remove("OSS_SEARCH_HOME")
    .env_remove("GITHUB_TOKEN");
    let t = Instant::now();
    let out = cmd.output().expect("run oss-cli hotset build");
    RunOutcome {
        code: out.status.code(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        elapsed: t.elapsed(),
    }
}

fn fixtures(tmp: &Path) -> (String, String) {
    let alpha = tmp.join("fx-alpha");
    let beta = tmp.join("fx-beta");
    fixture_repo(
        &alpha,
        &[
            ("src/lib.rs", "pub fn hotset_alpha_marker() -> u32 {\n    41\n}\n"),
            ("README.md", "# alpha\n\nFixture repo alpha.\n"),
        ],
    );
    fixture_repo(
        &beta,
        &[
            ("app/main.py", "def hotset_beta_marker():\n    return 'beta'\n"),
            ("README.md", "# beta\n\nFixture repo beta.\n"),
        ],
    );
    (file_url(&alpha), file_url(&beta))
}

#[test]
fn hotset_build_indexes_fixtures_and_resumes() {
    let tmp = tempfile::tempdir().unwrap();
    let (url_a, url_b) = fixtures(tmp.path());
    let home = tmp.path().join("home");
    let repos = format!("{url_a},{url_b}");
    let state_path = home.join("hotset-state.json");

    // ---- run 1: cold build ----
    let run1 = build(&home, &repos, &["--no-semantic"]);
    assert_eq!(run1.code, Some(0), "run 1 failed:\n{}", run1.stderr);
    assert!(
        home.join("index").join("manifest.json").is_file(),
        "index manifest missing"
    );
    assert!(run1.stdout.contains("index docs"), "summary missing:\n{}", run1.stdout);
    // semantic loud skip note (default auto, no fastembed in test builds)
    assert!(
        run1.stderr.contains("[semantic]") && run1.stderr.to_lowercase().contains("skip"),
        "semantic skip note missing:\n{}",
        run1.stderr
    );

    // state file marks stages complete
    let state: Value = serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
    for repo in ["fx-alpha", "fx-beta"] {
        assert_eq!(state["repos"][repo]["clone_done"], true, "{repo} not clone_done");
        assert!(
            state["repos"][repo]["ingest"]["files_accepted"].as_u64().unwrap() >= 2,
            "{repo} not ingested"
        );
    }
    assert!(state["index"]["docs"].as_u64().unwrap() >= 4, "index docs missing");
    assert_eq!(state["semantic"]["status"], "skipped");

    // index exists and answers a known-identifier query from the fixture
    let idx = TrigramIndex::load(&home.join("index")).expect("load index");
    let out = idx
        .search_literal("hotset_alpha_marker", &Filters::default(), 10)
        .expect("search");
    assert!(
        out.results.iter().any(|r| r.repo == "fx-alpha" && r.path == "src/lib.rs"),
        "no hit for the alpha marker: {:?}",
        out.results.iter().map(|r| (&r.repo, &r.path)).collect::<Vec<_>>()
    );

    // ---- run 2: everything cached ----
    let mtime1 = std::fs::metadata(&state_path).unwrap().modified().unwrap();
    let run2 = build(&home, &repos, &["--no-semantic"]);
    assert_eq!(run2.code, Some(0), "run 2 failed:\n{}", run2.stderr);
    for stage in ["[clone]", "[ingest]", "[index]"] {
        assert!(
            run2.stderr.contains(stage) && run2.stderr.contains("skipping"),
            "run 2 {stage} skip line missing:\n{}",
            run2.stderr
        );
    }
    let mtime2 = std::fs::metadata(&state_path).unwrap().modified().unwrap();
    assert_eq!(mtime1, mtime2, "state file rewritten on a fully-cached run");
    assert!(
        run2.elapsed < run1.elapsed,
        "second run ({:.2?}) not faster than first ({:.2?})",
        run2.elapsed,
        run1.elapsed
    );
}

#[test]
fn hotset_build_honors_oss_search_home_env() {
    let tmp = tempfile::tempdir().unwrap();
    let gamma = tmp.path().join("fx-gamma");
    fixture_repo(
        &gamma,
        &[("src/lib.rs", "pub fn hotset_gamma_marker() -> u8 {\n    7\n}\n")],
    );
    let home = tmp.path().join("env-home");
    let t = Instant::now();
    let out = Command::new(BIN)
        .args([
            "hotset", "build", "--repos", &file_url(&gamma), "--no-semantic",
        ])
        .env("OSS_SEARCH_HOME", &home)
        .env_remove("GITHUB_TOKEN")
        .output()
        .expect("run oss-cli hotset build");
    assert_eq!(
        out.status.code(),
        Some(0),
        "env build failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(home.join("hotset-state.json").is_file(), "OSS_SEARCH_HOME ignored");
    assert!(home.join("index").join("manifest.json").is_file());
    assert!(t.elapsed() < Duration::from_secs(60));
}

#[test]
fn hotset_build_forced_semantic_uses_stub_offline() {
    // --semantic without the fastembed feature must still complete offline
    // via the deterministic stub embedder (no model downloads in tests).
    let tmp = tempfile::tempdir().unwrap();
    let (url_a, _url_b) = fixtures(tmp.path());
    let home = tmp.path().join("stub-home");
    let run = build(&home, &url_a, &["--semantic"]);
    assert_eq!(run.code, Some(0), "forced semantic run failed:\n{}", run.stderr);
    assert!(
        run.stderr.contains("StubEmbedder"),
        "stub fallback warning missing:\n{}",
        run.stderr
    );
    let sem = home.join("semantic");
    for f in ["vectors.ossv", "chunks.db", "meta.json"] {
        assert!(sem.join(f).is_file(), "semantic artifact {f} missing");
    }
    let meta: Value =
        serde_json::from_str(&std::fs::read_to_string(sem.join("meta.json")).unwrap()).unwrap();
    assert_eq!(meta["embedder"], "stub");
    let state: Value =
        serde_json::from_str(&std::fs::read_to_string(home.join("hotset-state.json")).unwrap())
            .unwrap();
    assert_eq!(state["semantic"]["status"], "done");
    assert!(state["semantic"]["chunks"].as_u64().unwrap() >= 1);
}

#[test]
fn hotset_build_rejects_repoless_invocation() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(BIN)
        .args(["hotset", "build", "--home"])
        .arg(tmp.path())
        .env_remove("OSS_SEARCH_HOME")
        .output()
        .expect("run oss-cli hotset build");
    assert_eq!(out.status.code(), Some(2), "clap should reject missing --repos");
}
