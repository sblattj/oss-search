//! E2E gate driver: run the full oss-eval suite (`run_all`) into a target
//! directory (default `data/e2e/eval/`), print the quality bars, and assert
//! them before exiting 0.

use std::path::PathBuf;

fn main() {
    let out_dir = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../data/e2e/eval")
        });
    println!("running oss_eval::run_all into {}", out_dir.display());
    let t = std::time::Instant::now();
    let summary = oss_eval::run_all(&out_dir).expect("run_all");
    let secs = t.elapsed().as_secs_f64();
    println!("code  : {} queries over {} docs", summary.code.queries, summary.code.corpus_docs);
    println!("        ndcg@10 = {:.4} (bar >= 0.5)", summary.code.ndcg_at_10);
    println!("        mrr     = {:.4}", summary.code.mrr);
    println!("        recall@50 = {:.4}", summary.code.recall_at_50);
    println!(
        "repo  : hit_rate@10 = {:.4}",
        summary.repo.eval.hit_rate_at_k
    );
    println!(
        "latency: {} queries, p50 = {:.0} us (bar < 100_000), p90 = {:.0} us",
        summary.latency.queries, summary.latency.p50_us, summary.latency.p90_us
    );
    assert!(
        summary.code.ndcg_at_10 >= 0.5,
        "ndcg@10 {} below bar",
        summary.code.ndcg_at_10
    );
    assert!(
        summary.latency.p50_us < 100_000,
        "p50_us {} below bar",
        summary.latency.p50_us
    );
    assert!(summary.latency.queries >= 50);
    for name in [
        "EVAL.md",
        "code_eval.json",
        "repo_eval.json",
        "latency.json",
    ] {
        let p = out_dir.join(name);
        assert!(p.is_file(), "missing {name}");
        println!("wrote {}", p.display());
    }
    println!("\nEVAL E2E PASS in {secs:.1}s: EVAL.md + 3 JSON reports at {}", out_dir.display());
}
