pub mod code_eval;
pub mod golden;
pub mod latency;
pub mod metrics;
pub mod repo_eval;
pub mod report;

use std::io;
use std::path::{Path, PathBuf};

use code_eval::CodeEvalReport;
use latency::LatencyReport;
use repo_eval::RepoEvalReport;

pub const LATENCY_EXTRA_DOCS: usize = 2_700;
pub const LATENCY_QUERIES: usize = 60;

#[derive(Debug, Clone, PartialEq)]
pub struct EvalSummary {
    pub out_dir: PathBuf,
    pub code: CodeEvalReport,
    pub repo: RepoEvalReport,
    pub latency: LatencyReport,
}

pub fn run_all(out_dir: &Path) -> io::Result<EvalSummary> {
    std::fs::create_dir_all(out_dir)?;
    let docs = golden::corpus_docs();
    let code = code_eval::run(&golden::golden_set(), &docs)?;
    report::write_json(out_dir, "code_eval.json", &code)?;
    let repo = repo_eval::run();
    report::write_json(out_dir, "repo_eval.json", &repo)?;
    let latency = latency::run(
        &golden::golden_set(),
        &docs,
        LATENCY_EXTRA_DOCS,
        LATENCY_QUERIES,
    )?;
    report::write_json(out_dir, "latency.json", &latency)?;
    report::write_eval_md(out_dir, &code, &repo, &latency)?;
    Ok(EvalSummary {
        out_dir: out_dir.to_path_buf(),
        code,
        repo,
        latency,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_all_writes_reports_meeting_quality_bars() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let summary = run_all(tmp.path()).expect("run_all");
        assert!(
            summary.code.ndcg_at_10 >= 0.5,
            "ndcg@10 {}",
            summary.code.ndcg_at_10
        );
        assert!(
            summary.latency.p50_us < 100_000,
            "p50_us {}",
            summary.latency.p50_us
        );
        assert!(summary.latency.queries >= 50);
        for name in [
            "EVAL.md",
            "code_eval.json",
            "repo_eval.json",
            "latency.json",
        ] {
            let p = tmp.path().join(name);
            assert!(p.is_file(), "missing {name}");
        }
        let md = std::fs::read_to_string(tmp.path().join("EVAL.md")).expect("read EVAL.md");
        for needle in [
            "ndcg@10",
            "mrr",
            "recall@50",
            "hit_rate@10",
            "p50_us",
            "| query |",
        ] {
            assert!(md.contains(needle), "EVAL.md lacks {needle}");
        }
        let json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(tmp.path().join("code_eval.json")).expect("read json"),
        )
        .expect("parse code_eval.json");
        let got = json["ndcg_at_10"].as_f64().expect("ndcg field");
        assert!((got - summary.code.ndcg_at_10).abs() < 1e-9);
    }

    #[test]
    fn run_all_code_metrics_are_deterministic() {
        let a = tempfile::tempdir().expect("tempdir a");
        let b = tempfile::tempdir().expect("tempdir b");
        let sa = run_all(a.path()).expect("run a");
        let sb = run_all(b.path()).expect("run b");
        assert_eq!(sa.code, sb.code);
        assert_eq!(sa.repo.eval.hit_rate_at_k, sb.repo.eval.hit_rate_at_k);
    }
}
