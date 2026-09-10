//! Wiremock-backed tests for LiveEngine. NO live network: every backend is
//! pointed at a single wiremock server via `LiveEngineConfig::all_at`.
//!
//! The SearchEngine trait is synchronous and LiveEngine blocks on its own
//! runtime, so every engine call goes through `spawn_blocking` (the same
//! contract oss-mcp follows).

use std::sync::Arc;

use oss_core::{call_tool, SearchEngine};
use oss_live::{LiveEngine, LiveEngineConfig};
use serde_json::{json, Value};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn live_engine(server: &MockServer) -> LiveEngine {
    let base = server.uri();
    tokio::task::spawn_blocking(move || LiveEngine::with_config(LiveEngineConfig::all_at(base)))
        .await
        .unwrap()
}

async fn call(engine: &LiveEngine, tool: &str, args: Value) -> Result<Value, oss_core::ToolError> {
    let engine = engine.clone();
    let tool = tool.to_string();
    tokio::task::spawn_blocking(move || call_tool(&engine, &tool, Some(args)))
        .await
        .unwrap()
}

fn backend_status(out: &Value, name: &str) -> Value {
    out["backend_status"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["backend"] == json!(name))
        .cloned()
        .expect("backend present")
}

// ---------------------------------------------------------------- search_code

fn github_body(total: u64, items: Vec<Value>) -> Value {
    json!({
        "total_count": total,
        "incomplete_results": false,
        "items": items
    })
}

fn gh_item(repo: &str, file: &str) -> Value {
    json!({
        "name": file.rsplit('/').next().unwrap_or(file),
        "path": file,
        "html_url": format!("https://github.com/{repo}/blob/main/{file}"),
        "repository": {
            "full_name": repo,
            "license": { "spdx_id": "MIT" }
        },
        "score": 1.0,
        "text_matches": [{ "fragment": format!("fn {file}() {{ uses spawn_worker }}"), "matches": [], "property": "content" }]
    })
}

fn grep_body(total: u64, hits: Vec<(String, String)>) -> Value {
    json!({
        "hits": {
            "total": total,
            "hits": hits
                .into_iter()
                .map(|(repo, file)| {
                    json!({
                        "content": {
                            "snippet": "let w = <mark>spawn_worker</mark>(pool);",
                            "repo": repo,
                            "path": file,
                            "license": "MIT",
                            "branch": "main"
                        },
                        "score": 10.0
                    })
                })
                .collect::<Vec<_>>()
        }
    })
}

#[tokio::test]
async fn search_code_merges_two_backends_one_unavailable() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search/code"))
        .respond_with(ResponseTemplate::new(200).set_body_json(github_body(
            5,
            vec![gh_item("tokio-rs/tokio", "src/worker.rs"), gh_item("a/b", "src/x.rs")],
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/search"))
        .respond_with(ResponseTemplate::new(429))
        .mount(&server)
        .await;

    let engine = live_engine(&server).await;
    let out = call(
        &engine,
        "oss_search_code",
        json!({"probes": ["spawn_worker"], "limit": 3}),
    )
    .await
    .unwrap();

    // hits present despite one backend down
    assert_eq!(out["results"].as_array().unwrap().len(), 2);
    assert_eq!(out["partial"], json!(true));
    // truthful has_more: github reports total 5 with only 2 delivered
    assert_eq!(out["has_more"], json!(true));
    assert!(out["next_cursor"].is_string());

    // both backends carry their status
    assert_eq!(backend_status(&out, "github")["status"], json!("ok"));
    let grep = backend_status(&out, "grep.app");
    assert_eq!(grep["status"], json!("error"), "grep: {grep}");
    assert!(grep["detail"].as_str().unwrap().contains("rate limited"));

    // per-backend attribution on rows: every row credits github only
    for row in out["results"].as_array().unwrap() {
        assert!(row["backends"].as_array().unwrap().iter().all(|b| b == &json!("github")));
        assert!(row["sample_evidence"].as_str().unwrap().contains("spawn_worker"));
    }

    // plan-selected but undispatchable backends are reported, not hidden
    assert_eq!(backend_status(&out, "local_index")["status"], json!("error"));
    assert_eq!(backend_status(&out, "sourcegraph")["status"], json!("error"));
}

#[tokio::test]
async fn search_code_dedupes_and_attributes_both_backends() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search/code"))
        .respond_with(ResponseTemplate::new(200).set_body_json(github_body(
            2,
            vec![gh_item("tokio-rs/tokio", "src/worker.rs")],
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(grep_body(
            2,
            vec![
                ("tokio-rs/tokio".into(), "src/worker.rs".into()),
                ("smol-rs/smol".into(), "src/task.rs".into()),
            ],
        )))
        .mount(&server)
        .await;

    let engine = live_engine(&server).await;
    let out = call(
        &engine,
        "oss_search_code",
        json!({"probes": ["spawn_worker"], "limit": 10}),
    )
    .await
    .unwrap();

    assert_eq!(out["partial"], json!(false));
    let repos = out["results"].as_array().unwrap();
    let tokio = repos
        .iter()
        .find(|r| r["repo"] == json!("tokio-rs/tokio"))
        .expect("tokio row");
    // same repo+path seen by both backends merges into one row credited to both
    assert!(tokio["backends"].as_array().unwrap().contains(&json!("github")));
    assert!(tokio["backends"].as_array().unwrap().contains(&json!("grep.app")));
    assert_eq!(tokio["hit_files"], json!(1));
    assert_eq!(tokio["probe_breadth"], json!(1));
    assert_eq!(backend_status(&out, "github")["status"], json!("ok"));
    assert_eq!(backend_status(&out, "grep.app")["status"], json!("ok"));
}

#[tokio::test]
async fn search_code_two_probes_widen_probe_breadth() {
    let server = MockServer::start().await;
    // probe 1 on github returns tokio; probe 2 on github returns tokio + smol
    Mock::given(method("GET"))
        .and(path("/search/code"))
        .and(query_param("q", "spawn_worker"))
        .respond_with(ResponseTemplate::new(200).set_body_json(github_body(
            1,
            vec![gh_item("tokio-rs/tokio", "src/worker.rs")],
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/search/code"))
        .and(query_param("q", "acquire_checked"))
        .respond_with(ResponseTemplate::new(200).set_body_json(github_body(
            2,
            vec![gh_item("tokio-rs/tokio", "src/lease.rs"), gh_item("smol-rs/smol", "src/lease.rs")],
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/search"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;

    let engine = live_engine(&server).await;
    let out = call(
        &engine,
        "oss_search_code",
        json!({"probes": ["spawn_worker", "acquire_checked"], "limit": 10}),
    )
    .await
    .unwrap();

    let repos = out["results"].as_array().unwrap();
    let tokio = repos
        .iter()
        .find(|r| r["repo"] == json!("tokio-rs/tokio"))
        .unwrap();
    assert_eq!(tokio["probe_breadth"], json!(2));
    assert_eq!(tokio["total_probes"], json!(2));
    assert_eq!(tokio["hit_files"], json!(2));
    // breadth-2 repo ranks above breadth-1
    assert_eq!(repos[0]["repo"], json!("tokio-rs/tokio"));
}

#[tokio::test]
async fn search_code_regex_probe_error_propagates_typed() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search/code"))
        .respond_with(ResponseTemplate::new(200).set_body_json(github_body(0, vec![])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(grep_body(0, vec![])))
        .mount(&server)
        .await;

    let engine = live_engine(&server).await;
    let err = call(
        &engine,
        "oss_search_code",
        json!({"probes": ["/[a-z]+/"]}),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), "invalid_value");
    let j = err.to_json();
    assert!(j["error"]["message"].as_str().unwrap().contains("indexable"));
    // nothing was sent to any backend for an invalid plan
    assert_eq!(server.received_requests().await.unwrap().len(), 0);
}

#[tokio::test]
async fn search_code_snippets_mode_rows() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search/code"))
        .respond_with(ResponseTemplate::new(200).set_body_json(github_body(
            1,
            vec![gh_item("a/b", "src/x.rs")],
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(grep_body(0, vec![])))
        .mount(&server)
        .await;

    let engine = live_engine(&server).await;
    let out = call(
        &engine,
        "oss_search_code",
        json!({"probes": ["spawn_worker"], "mode": "snippets"}),
    )
    .await
    .unwrap();
    let rows = out["results"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["repo"], json!("a/b"));
    assert_eq!(rows[0]["path"], json!("src/x.rs"));
    assert_eq!(rows[0]["probe"], json!("spawn_worker"));
}

// --------------------------------------------------------------- search_repos

#[tokio::test]
async fn search_repos_ranks_with_per_signal_contributions() {
    let server = MockServer::start().await;
    // npms search returns two packages; left-pad has no repo metadata
    Mock::given(method("GET"))
        .and(path("/v2/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "total": 2,
            "results": [
                {
                    "package": {
                        "name": "small-dep",
                        "description": "small but load-bearing",
                        "links": { "npm": "https://www.npmjs.com/package/small-dep" },
                        "license": "MIT"
                    },
                    "score": { "final": 0.5 }
                },
                {
                    "package": {
                        "name": "big-star",
                        "description": "popular periphery",
                        "links": { "npm": "https://www.npmjs.com/package/big-star", "repository": "https://github.com/acme/big-star" },
                        "license": "Apache-2.0"
                    },
                    "score": { "final": 0.9 }
                }
            ]
        })))
        .mount(&server)
        .await;
    // package metadata: small-dep is depended on by 9000 packages
    Mock::given(method("GET"))
        .and(path("/api/v1/registries/npmjs.org/packages/small-dep"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "small-dep",
            "dependent_packages_count": 9000,
            "repository_url": "https://github.com/acme/small-dep",
            "normalized_licenses": ["MIT"]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/registries/npmjs.org/packages/big-star"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "name": "big-star",
            "dependent_packages_count": 12,
            "repository_url": "https://github.com/acme/big-star",
            "normalized_licenses": ["Apache-2.0"]
        })))
        .mount(&server)
        .await;
    // repo metadata: small-dep healthy with stars; big-star archived + stale
    Mock::given(method("GET"))
        .and(path("/api/v1/repositories/lookup"))
        .and(query_param("url", "https://github.com/acme/small-dep"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "full_name": "acme/small-dep",
            "description": "small but load-bearing",
            "license": "mit",
            "stargazers_count": 900,
            "forks_count": 90,
            "pushed_at": pushed_days_ago(5),
            "archived": false,
            "language": "TypeScript",
            "html_url": "https://github.com/acme/small-dep",
            "repository_url": "https://github.com/acme/small-dep"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repositories/lookup"))
        .and(query_param("url", "https://github.com/acme/big-star"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "full_name": "acme/big-star",
            "stargazers_count": 50_000,
            "forks_count": 4_000,
            "pushed_at": pushed_days_ago(900),
            "archived": true,
            "html_url": "https://github.com/acme/big-star",
            "repository_url": "https://github.com/acme/big-star"
        })))
        .mount(&server)
        .await;

    let engine = live_engine(&server).await;
    let out = call(
        &engine,
        "oss_search_repos",
        json!({"query": "test", "limit": 10}),
    )
    .await
    .unwrap();

    let rows = out["results"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    for row in rows {
        let contributions = row["contributions"].as_object().expect("contributions map");
        assert!(contributions.contains_key("dependents"), "row {row}");
        assert!(contributions.contains_key("popularity"));
        assert!(contributions.contains_key("maintenance"));
        assert!(contributions.contains_key("maintenance_gate"));
        assert!(row["score"].as_f64().unwrap() > 0.0);
    }
    // healthy + depended-on outranks archived + stale despite 50k stars
    assert_eq!(rows[0]["repo"], json!("acme/small-dep"));
    let small = &rows[0];
    assert_eq!(small["stars"], json!(900));
    assert_eq!(small["dependents"], json!(9000));
    assert_eq!(small["license"], json!("MIT"));
    assert_eq!(small["star_tier"], json!("medium"));
    // archived repo shows the gate biting in its contribution map
    let big = &rows[1];
    assert!(big["contributions"]["maintenance_gate"].as_f64().unwrap() < 0.0);

    assert_eq!(backend_status(&out, "npms.io")["status"], json!("ok"));
    assert_eq!(backend_status(&out, "ecosyste.ms")["status"], json!("ok"));
    assert_eq!(out["partial"], json!(false));
}

fn pushed_days_ago(days: i64) -> String {
    // deterministic RFC3339 for a date N days before 2026-09-10; the util
    // only reads the date part, and 2026-09-10 is "today" for the fixture
    // only in spirit — compute a real past date instead.
    let epoch_days = 20_666 - days; // 2026-09-10 is day 20666 since epoch
    let date = civil_from_days(epoch_days);
    format!("{:04}-{:02}-{:02}T00:00:00.000Z", date.0, date.1, date.2)
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[tokio::test]
async fn search_repos_npms_down_is_partial_not_negative() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v2/search"))
        .respond_with(ResponseTemplate::new(429))
        .mount(&server)
        .await;

    let engine = live_engine(&server).await;
    let out = call(
        &engine,
        "oss_search_repos",
        json!({"query": "test"}),
    )
    .await
    .unwrap();

    assert_eq!(out["results"].as_array().unwrap().len(), 0);
    assert_eq!(out["partial"], json!(true));
    assert_eq!(backend_status(&out, "npms.io")["status"], json!("error"));
    assert_eq!(
        backend_status(&out, "ecosyste.ms")["status"],
        json!("error"),
        "ecosyste.ms is reported as skipped, not consulted"
    );
}

// ------------------------------------------------------------- repo_profile

#[tokio::test]
async fn repo_profile_merges_ecosystems_and_deps_dev() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repositories/lookup"))
        .and(query_param("url", "https://github.com/acme/lib"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "full_name": "acme/lib",
            "description": "does one thing well",
            "license": "mit",
            "stargazers_count": 4200,
            "forks_count": 300,
            "open_issues_count": 21,
            "pushed_at": "2026-09-01T00:00:00.000Z",
            "archived": false,
            "language": "Rust",
            "topics": ["parser"],
            "html_url": "https://github.com/acme/lib",
            "repository_url": "https://github.com/acme/lib",
            "metadata": { "files": { "readme": "README.md" } }
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v3/systems/npm/packages/lib"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "packageVersions": [
                { "versionKey": { "system": "NPM", "name": "lib", "version": "2.1.0" }, "isDefault": true, "publishedAt": "2026-08-01T00:00:00Z" },
                { "versionKey": { "system": "NPM", "name": "lib", "version": "1.9.4" } }
            ]
        })))
        .mount(&server)
        .await;

    let engine = live_engine(&server).await;
    let out = call(
        &engine,
        "oss_repo_profile",
        json!({"repo": "acme/lib"}),
    )
    .await
    .unwrap();

    let row = &out["results"][0];
    assert_eq!(row["repo"], json!("acme/lib"));
    assert_eq!(row["stars"], json!(4200));
    assert_eq!(row["license"], json!("MIT"));
    assert_eq!(row["language"], json!("Rust"));
    assert_eq!(row["readme_path"], json!("README.md"));
    assert_eq!(row["deps_dev"]["default_version"], json!("2.1.0"));
    assert_eq!(row["deps_dev"]["versions_tracked"], json!(2));
    assert_eq!(backend_status(&out, "ecosyste.ms")["status"], json!("ok"));
    assert_eq!(backend_status(&out, "deps.dev")["status"], json!("ok"));
}

#[tokio::test]
async fn repo_profile_unknown_repo_is_not_found() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repositories/lookup"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({"error": "not found"})))
        .mount(&server)
        .await;
    let engine = live_engine(&server).await;
    let err = call(
        &engine,
        "oss_repo_profile",
        json!({"repo": "acme/missing"}),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), "not_found");
}

#[tokio::test]
async fn repo_profile_deps_dev_absent_is_ok_not_degraded() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repositories/lookup"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "full_name": "acme/native",
            "stargazers_count": 10,
            "pushed_at": "2026-09-01T00:00:00.000Z",
            "metadata": { "files": {} }
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v3/systems/npm/packages/native"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let engine = live_engine(&server).await;
    let out = call(
        &engine,
        "oss_repo_profile",
        json!({"repo": "acme/native"}),
    )
    .await
    .unwrap();
    assert_eq!(backend_status(&out, "deps.dev")["status"], json!("ok"));
    assert_eq!(out["partial"], json!(false));
}

// ------------------------------------------------------------ tree / file

#[tokio::test]
async fn repo_tree_filters_prefix_glob_and_pages() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/lib/git/trees/HEAD"))
        .and(query_param("recursive", "1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "sha": "s1",
            "truncated": false,
            "tree": [
                { "path": "src/main.rs", "type": "blob", "size": 1 },
                { "path": "src/lib.rs", "type": "blob", "size": 2 },
                { "path": "tests/it.rs", "type": "blob", "size": 3 },
                { "path": "README.md", "type": "blob", "size": 4 }
            ]
        })))
        .mount(&server)
        .await;
    let engine = live_engine(&server).await;

    let out = call(
        &engine,
        "oss_repo_tree",
        json!({"repo": "acme/lib", "path_prefix": "src/", "limit": 1}),
    )
    .await
    .unwrap();
    let rows = out["results"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["path"], json!("src/lib.rs"));
    assert_eq!(out["total"], json!(2));
    assert_eq!(out["has_more"], json!(true));
    assert!(out["next_cursor"].as_str().unwrap().starts_with("off:"));
    assert_eq!(backend_status(&out, "github-contents")["status"], json!("ok"));
}

#[tokio::test]
async fn repo_tree_truncated_tree_is_partial_degraded() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/mono/git/trees/HEAD"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "sha": "s1",
            "truncated": true,
            "tree": [ { "path": "a.txt", "type": "blob", "size": 1 } ]
        })))
        .mount(&server)
        .await;
    let engine = live_engine(&server).await;
    let out = call(&engine, "oss_repo_tree", json!({"repo": "acme/mono"}))
        .await
        .unwrap();
    assert_eq!(out["partial"], json!(true));
    assert_eq!(backend_status(&out, "github-contents")["status"], json!("degraded"));
}

#[tokio::test]
async fn fetch_file_ranges_lines_and_missing_file_is_not_found() {
    let server = MockServer::start().await;
    let content = "l1\nl2\nl3\nl4\nl5";
    let b64 = b64_multiline(content);
    Mock::given(method("GET"))
        .and(path("/repos/acme/lib/contents/src/lib.rs"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "path": "src/lib.rs",
            "size": content.len(),
            "encoding": "base64",
            "content": b64,
            "html_url": "https://github.com/acme/lib/blob/main/src/lib.rs"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/lib/contents/src/guessed.rs"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let engine = live_engine(&server).await;

    let out = call(
        &engine,
        "oss_fetch_file",
        json!({"repo": "acme/lib", "path": "src/lib.rs", "line_range": {"start": 2, "end": 4}}),
    )
    .await
    .unwrap();
    let row = &out["results"][0];
    assert_eq!(row["total_lines"], json!(5));
    assert_eq!(row["start_line"], json!(2));
    assert_eq!(row["end_line"], json!(4));
    assert_eq!(row["content"], json!("l2\nl3\nl4"));

    let err = call(
        &engine,
        "oss_fetch_file",
        json!({"repo": "acme/lib", "path": "src/guessed.rs"}),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code(), "not_found");
    assert!(err.to_json()["error"]["correction"]
        .as_str()
        .unwrap()
        .contains("oss_repo_tree"));
}

fn b64_multiline(s: &str) -> String {
    // mirrors GitHub's actual formatting: base64 split across lines
    let encoded = {
        use base64_sim::b64;
        b64(s.as_bytes())
    };
    encoded
        .as_bytes()
        .chunks(16)
        .map(|c| String::from_utf8(c.to_vec()).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
}

// fake minimal base64 (standard alphabet, padding) to avoid a dev-dep just
// for fixtures
mod base64_sim {
    const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    pub fn b64(data: &[u8]) -> String {
        let mut out = String::new();
        for chunk in data.chunks(3) {
            let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
            let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
            out.push(TABLE[(n >> 18) as usize & 63] as char);
            out.push(TABLE[(n >> 12) as usize & 63] as char);
            out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
            out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
        }
        out
    }
}

// ------------------------------------------------------------- fetch_docs

#[tokio::test]
async fn fetch_docs_searches_readme_with_breadcrumbs() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v1/repositories/lookup"))
        .and(query_param("url", "https://github.com/acme/lib"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "full_name": "acme/lib",
            "html_url": "https://github.com/acme/lib",
            "metadata": { "files": { "readme": "README.md" } }
        })))
        .mount(&server)
        .await;
    let readme = "# lib\n\nUse `spawn_worker` to start.\nAlso spawn_worker twice.";
    Mock::given(method("GET"))
        .and(path("/repos/acme/lib/contents/README.md"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "path": "README.md",
            "size": readme.len(),
            "encoding": "base64",
            "content": base64_sim::b64(readme.as_bytes())
        })))
        .mount(&server)
        .await;
    let engine = live_engine(&server).await;
    let out = call(
        &engine,
        "oss_fetch_docs",
        json!({"repo": "acme/lib", "pattern": "spawn_worker"}),
    )
    .await
    .unwrap();
    let row = &out["results"][0];
    assert_eq!(row["page"], json!("README.md"));
    let hits = row["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0]["line"], json!(3));
    assert!(hits[0]["breadcrumb"].as_str().unwrap().starts_with("Source: https://github.com/acme/lib"));
    assert_eq!(backend_status(&out, "github-contents")["status"], json!("ok"));
}

#[tokio::test]
async fn fetch_docs_never_fakes_content_when_unavailable() {
    let server = MockServer::start().await;
    // repo exists but publishes no readme path
    Mock::given(method("GET"))
        .and(path("/api/v1/repositories/lookup"))
        .and(query_param("url", "https://github.com/acme/undoc"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "full_name": "acme/undoc",
            "metadata": { "files": {} }
        })))
        .mount(&server)
        .await;
    let engine = live_engine(&server).await;
    let out = call(
        &engine,
        "oss_fetch_docs",
        json!({"repo": "acme/undoc", "pattern": "install"}),
    )
    .await
    .unwrap();
    assert_eq!(out["results"].as_array().unwrap().len(), 0);
    assert_eq!(out["partial"], json!(true));
    assert_eq!(backend_status(&out, "github-contents")["status"], json!("error"));

    // ecosyste.ms down entirely (5xx): still no fake content
    Mock::given(method("GET"))
        .and(path("/dead/api/v1/repositories/lookup"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let engine2 = {
        let base = server.uri();
        tokio::task::spawn_blocking(move || {
            let mut cfg = LiveEngineConfig::all_at(base);
            cfg.ecosystems_repos_base = format!("{}/dead", cfg.ecosystems_repos_base);
            cfg.ecosystems_packages_base = format!("{}/dead", cfg.ecosystems_packages_base);
            LiveEngine::with_config(cfg)
        })
        .await
        .unwrap()
    };
    let out2 = call(
        &engine2,
        "oss_fetch_docs",
        json!({"repo": "acme/undoc", "pattern": "install"}),
    )
    .await
    .unwrap();
    assert_eq!(out2["results"].as_array().unwrap().len(), 0);
    assert_eq!(backend_status(&out2, "ecosyste.ms")["status"], json!("error"));
}

// ------------------------------------------------------------------- guide

#[tokio::test]
async fn guide_is_static_and_describes_grammar() {
    let server = MockServer::start().await;
    let engine = live_engine(&server).await;
    let out = call(&engine, "oss_guide", json!({})).await.unwrap();
    let row = &out["results"][0];
    assert!(row["query_grammar"]["json_fields"].is_array());
    assert!(row["query_grammar"]["text_dialect"]["filters"].is_object());
    // zero network calls for the guide
    assert_eq!(server.received_requests().await.unwrap().len(), 0);
}

// -------------------------------------------------- engine selection wiring

#[test]
fn stub_stays_default_and_live_is_separate() {
    // the trait object must be constructible for both engines; stub without
    // any network, live without contacting anything at construction time
    let stub: Arc<dyn SearchEngine> = Arc::new(oss_core::StubEngine::new());
    assert!(!stub.backend_status("oss_guide").is_empty());
}

#[tokio::test]
async fn token_absence_is_reported_not_fatal() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search/code"))
        .respond_with(ResponseTemplate::new(200).set_body_json(github_body(1, vec![gh_item("a/b", "x.rs")])))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(grep_body(0, vec![])))
        .mount(&server)
        .await;

    let base = server.uri();
    let engine = tokio::task::spawn_blocking(move || {
        let mut cfg = LiveEngineConfig::all_at(base);
        cfg.github_token = None; // anonymous
        LiveEngine::with_config(cfg)
    })
    .await
    .unwrap();

    assert!(engine.github_token_absent());
    let out = call(
        &engine,
        "oss_search_code",
        json!({"probes": ["spawn_worker"]}),
    )
    .await
    .unwrap();
    assert_eq!(out["results"].as_array().unwrap().len(), 1);
    let gh = backend_status(&out, "github");
    assert_eq!(gh["status"], json!("ok"));
    assert!(gh["detail"].as_str().unwrap().contains("GITHUB_TOKEN"));
}
