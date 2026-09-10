use std::io;
use std::time::Instant;

use oss_index::{Filters, TrigramIndex};
use serde::{Deserialize, Serialize};

use crate::golden::{GoldenSet, synthetic_doc};

pub const WARMUP: usize = 5;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LatencyRow {
    pub query: String,
    pub micros: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LatencyReport {
    pub queries: usize,
    pub docs: usize,
    pub p50_us: u64,
    pub p90_us: u64,
    pub p99_us: u64,
    pub max_us: u64,
    pub mean_us: f64,
    pub per_query: Vec<LatencyRow>,
}

pub fn percentile(sorted: &[u64], pct: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let n = sorted.len();
    let rank = ((pct / 100.0) * n as f64).ceil() as usize;
    sorted[rank.clamp(1, n) - 1]
}

fn query_texts(golden: &GoldenSet, count: usize) -> Vec<String> {
    let mut texts: Vec<String> = golden.queries.iter().map(|q| q.text.to_string()).collect();
    let mut i = 0;
    while texts.len() < count {
        let idx = 500 + i * 7;
        texts.push(format!("panel_state_{idx:04}"));
        i += 1;
    }
    texts.truncate(count);
    texts
}

pub fn run(
    golden: &GoldenSet,
    base_docs: &[(String, String, Vec<u8>)],
    extra_docs: usize,
    query_count: usize,
) -> io::Result<LatencyReport> {
    let mut docs = base_docs.to_vec();
    for i in 0..extra_docs {
        docs.push(synthetic_doc(500 + i));
    }
    let index = TrigramIndex::build(docs.clone()).map_err(io::Error::other)?;
    let filters = Filters::default();
    let texts = query_texts(golden, query_count);
    for text in texts.iter().take(WARMUP.min(texts.len())) {
        let _ = index.search_literal(text, &filters, 50);
    }
    let mut per_query = Vec::with_capacity(texts.len());
    for text in &texts {
        let start = Instant::now();
        let outcome = index
            .search_literal(text, &filters, 50)
            .map_err(io::Error::other)?;
        let micros = start.elapsed().as_nanos() as u64 / 1_000;
        let _ = outcome.results.len();
        per_query.push(LatencyRow {
            query: text.clone(),
            micros,
        });
    }
    let mut sorted: Vec<u64> = per_query.iter().map(|r| r.micros).collect();
    sorted.sort_unstable();
    let total: u64 = sorted.iter().sum();
    Ok(LatencyReport {
        queries: sorted.len(),
        docs: docs.len(),
        p50_us: percentile(&sorted, 50.0),
        p90_us: percentile(&sorted, 90.0),
        p99_us: percentile(&sorted, 99.0),
        max_us: sorted.last().copied().unwrap_or(0),
        mean_us: if sorted.is_empty() {
            0.0
        } else {
            total as f64 / sorted.len() as f64
        },
        per_query,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::golden::{corpus_docs, golden_set};

    #[test]
    fn percentile_nearest_rank_hand_computed() {
        let v = vec![10, 20, 30, 40, 50];
        assert_eq!(percentile(&v, 0.0), 10);
        assert_eq!(percentile(&v, 50.0), 30);
        assert_eq!(percentile(&v, 90.0), 50);
        assert_eq!(percentile(&v, 100.0), 50);
        let w = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        assert_eq!(percentile(&w, 50.0), 5);
        assert_eq!(percentile(&w, 90.0), 9);
        assert_eq!(percentile(&w, 10.0), 1);
        assert_eq!(percentile(&[], 50.0), 0);
    }

    #[test]
    fn percentiles_are_monotonic() {
        let report = run(&golden_set(), &corpus_docs(), 300, 60).expect("latency run");
        assert!(report.p50_us <= report.p90_us);
        assert!(report.p90_us <= report.p99_us);
        assert!(report.p99_us <= report.max_us);
        assert!(report.mean_us <= report.max_us as f64);
    }

    #[test]
    fn runs_at_least_fifty_queries_over_expanded_corpus() {
        let report = run(&golden_set(), &corpus_docs(), 500, 60).expect("latency run");
        assert_eq!(report.queries, 60);
        assert_eq!(report.docs, 300 + 500);
        assert!(report.per_query.iter().all(|r| r.micros > 0));
    }
}
