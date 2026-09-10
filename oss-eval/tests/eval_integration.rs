use std::fs;

use oss_eval::run_all;

#[test]
fn full_eval_harness_end_to_end() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let out = tmp.path().join("reports");
    let summary = run_all(&out).expect("run_all succeeds");

    assert!(
        summary.code.ndcg_at_10 >= 0.5,
        "golden nDCG@10 {} below 0.5",
        summary.code.ndcg_at_10
    );
    assert!(summary.code.queries >= 25);
    assert_eq!(summary.code.corpus_docs, 300);
    assert!(
        summary.latency.p50_us < 100_000,
        "latency p50 {}us >= 100ms",
        summary.latency.p50_us
    );
    assert!(summary.latency.queries >= 50);
    assert!((summary.repo.eval.hit_rate_at_k - 1.0).abs() < 1e-9);

    let eval_md = out.join("EVAL.md");
    assert!(eval_md.is_file());
    let md = fs::read_to_string(&eval_md).expect("read EVAL.md");
    assert!(md.contains("ndcg@10"));
    assert!(md.contains("recall@50"));
    assert!(md.contains("hit_rate@10"));
    assert!(md.contains("p50_us"));
    assert!(md.contains("| query |"));

    for name in ["code_eval.json", "repo_eval.json", "latency.json"] {
        let raw = fs::read_to_string(out.join(name)).unwrap_or_else(|e| panic!("read {name}: {e}"));
        let v: serde_json::Value =
            serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse {name}: {e}"));
        assert!(v.is_object(), "{name} is not a JSON object");
    }

    let code_json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(out.join("code_eval.json")).unwrap()).unwrap();
    assert_eq!(
        code_json["queries"].as_u64(),
        Some(summary.code.queries as u64)
    );
    assert!((code_json["ndcg_at_10"].as_f64().unwrap() - summary.code.ndcg_at_10).abs() < 1e-9);
}
