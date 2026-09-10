use std::collections::HashSet;
use std::io;

use oss_index::{Filters, SearchOutcome, TrigramIndex};
use serde::{Deserialize, Serialize};

use crate::golden::{GoldenSet, QueryKind};
use crate::metrics::{mrr, ndcg_at_k, recall_at_k};

pub const DEPTH: usize = 50;
pub const K: usize = 10;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryMetrics {
    pub query_id: String,
    pub text: String,
    pub kind: String,
    pub ndcg_at_10: f64,
    pub reciprocal_rank: f64,
    pub recall_at_50: f64,
    pub judged_relevant: usize,
    pub relevant_returned: usize,
    pub returned: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeEvalReport {
    pub queries: usize,
    pub corpus_docs: usize,
    pub ndcg_at_10: f64,
    pub mrr: f64,
    pub recall_at_50: f64,
    pub per_query: Vec<QueryMetrics>,
}

fn doc_key(repo: &str, path: &str) -> String {
    format!("{repo}/{path}")
}

fn run_query(index: &TrigramIndex, text: &str, kind: QueryKind) -> io::Result<SearchOutcome> {
    let filters = Filters::default();
    let out = match kind {
        QueryKind::Regex => index.search_regex(text, &filters, DEPTH),
        _ => index.search_literal(text, &filters, DEPTH),
    };
    out.map_err(io::Error::other)
}

pub fn run(golden: &GoldenSet, docs: &[(String, String, Vec<u8>)]) -> io::Result<CodeEvalReport> {
    let index = TrigramIndex::build(docs.to_vec()).map_err(io::Error::other)?;
    let mut per_query = Vec::with_capacity(golden.queries.len());
    for q in &golden.queries {
        let judged = golden
            .qrels
            .get(q.id)
            .map(|m| {
                m.iter()
                    .map(|(doc, gain)| ((*doc).to_string(), *gain))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let relevant: HashSet<String> = judged
            .iter()
            .filter(|(_, g)| *g >= 1)
            .map(|(doc, _)| doc.clone())
            .collect();
        let outcome = run_query(&index, q.text, q.kind)?;
        let keys: Vec<String> = outcome
            .results
            .iter()
            .map(|r| doc_key(&r.repo, &r.path))
            .collect();
        let gains: Vec<u8> = keys
            .iter()
            .take(K)
            .map(|k| {
                judged
                    .iter()
                    .find(|(doc, _)| doc == k)
                    .map(|(_, g)| *g)
                    .unwrap_or(0)
            })
            .collect();
        let relevant_flags: Vec<bool> = keys.iter().take(K).map(|k| relevant.contains(k)).collect();
        let relevant_returned = keys.iter().filter(|k| relevant.contains(*k)).count();
        per_query.push(QueryMetrics {
            query_id: q.id.to_string(),
            text: q.text.to_string(),
            kind: format!("{:?}", q.kind).to_lowercase(),
            ndcg_at_10: ndcg_at_k(&gains, K),
            reciprocal_rank: mrr(&relevant_flags),
            recall_at_50: recall_at_k(&keys, &relevant, DEPTH),
            judged_relevant: relevant.len(),
            relevant_returned,
            returned: keys.len(),
        });
    }
    let n = per_query.len();
    let mean = |f: fn(&QueryMetrics) -> f64| {
        if n == 0 {
            0.0
        } else {
            per_query.iter().map(f).sum::<f64>() / n as f64
        }
    };
    Ok(CodeEvalReport {
        queries: n,
        corpus_docs: docs.len(),
        ndcg_at_10: mean(|m| m.ndcg_at_10),
        mrr: mean(|m| m.reciprocal_rank),
        recall_at_50: mean(|m| m.recall_at_50),
        per_query,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::golden::{corpus_docs, golden_set};

    fn report() -> CodeEvalReport {
        run(&golden_set(), &corpus_docs()).expect("code eval runs")
    }

    #[test]
    fn golden_eval_meets_the_ndcg_bar() {
        let r = report();
        assert_eq!(r.queries, 29);
        assert!(r.ndcg_at_10 >= 0.5, "ndcg@10 {}", r.ndcg_at_10);
        assert!(r.mrr >= 0.5, "mrr {}", r.mrr);
        assert!(r.recall_at_50 >= 0.9, "recall@50 {}", r.recall_at_50);
    }

    #[test]
    fn identifier_query_puts_canonical_doc_first() {
        let r = report();
        let q = r
            .per_query
            .iter()
            .find(|m| m.query_id == "q01")
            .expect("q01 present");
        assert!((q.reciprocal_rank - 1.0).abs() < 1e-9);
        assert!((q.recall_at_50 - 1.0).abs() < 1e-9);
        assert!((q.ndcg_at_10 - 1.0).abs() < 1e-9);
    }

    #[test]
    fn regex_query_returns_judged_docs() {
        let r = report();
        let q = r
            .per_query
            .iter()
            .find(|m| m.query_id == "q29")
            .expect("q29 present");
        assert_eq!(q.kind, "regex");
        assert_eq!(q.relevant_returned, 3);
        assert!((q.recall_at_50 - 1.0).abs() < 1e-9);
        assert!(q.ndcg_at_10 > 0.9, "regex ndcg {}", q.ndcg_at_10);
    }

    #[test]
    fn metrics_are_finite() {
        let r = report();
        assert!(r.per_query.iter().all(|m| m.ndcg_at_10.is_finite()
            && m.reciprocal_rank.is_finite()
            && m.recall_at_50.is_finite()));
    }
}
