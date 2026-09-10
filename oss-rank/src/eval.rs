//! Offline evaluation of ranking functions against awesome-list ground
//! truth: hit-rate@k and MRR (mean reciprocal rank), used by `oss-eval` to
//! tune [`crate::rank::RankerConfig`] weights.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::candidates::canonical_repo_url;

/// Per-topic outcome of one evaluation run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TopicResult {
    pub topic: String,
    /// Whether any expected repo appeared in the top `k`.
    pub hit_at_k: bool,
    /// 1-based rank of the first expected repo (`None` = no expected repo
    /// was ranked at all).
    pub first_hit_rank: Option<usize>,
    /// `1 / first_hit_rank`, or 0.0 on a miss.
    pub reciprocal_rank: f64,
}

/// Aggregate metrics over all topics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvalReport {
    pub k: usize,
    pub topics: usize,
    /// Fraction of topics with a hit in the top `k`.
    pub hit_rate_at_k: f64,
    /// Mean reciprocal rank over topics (misses contribute 0).
    pub mrr: f64,
    /// Per-topic detail, in ground-truth key order (deterministic).
    pub per_topic: Vec<TopicResult>,
}

/// Evaluates a ranking function against a ground-truth map of
/// topic → expected repos (e.g. the curated content of awesome lists,
/// held out from candidate generation).
///
/// Repo comparison is on canonical URLs (case/host/`.git`/slash variants
/// match), topics are visited in sorted order for determinism, and the
/// ranking function runs offline — no network.
#[derive(Debug, Clone)]
pub struct AwesomeListEval {
    /// Depth for hit-rate@k.
    pub k: usize,
}

impl AwesomeListEval {
    pub fn new(k: usize) -> Self {
        Self {
            k: usize::max(k, 1),
        }
    }

    pub fn evaluate<F>(
        &self,
        ground_truth: &BTreeMap<String, Vec<String>>,
        rank_fn: F,
    ) -> EvalReport
    where
        F: Fn(&str) -> Vec<String>,
    {
        let mut per_topic = Vec::with_capacity(ground_truth.len());
        for (topic, expected) in ground_truth {
            let expected_set: HashSet<String> =
                expected.iter().map(|u| canonical_repo_url(u)).collect();
            let ranked: Vec<String> = rank_fn(topic)
                .iter()
                .map(|u| canonical_repo_url(u))
                .collect();
            let first_hit_rank = ranked
                .iter()
                .position(|u| expected_set.contains(u))
                .map(|i| i + 1);
            let hit_at_k = first_hit_rank.is_some_and(|r| r <= self.k);
            let reciprocal_rank = first_hit_rank.map_or(0.0, |r| 1.0 / r as f64);
            per_topic.push(TopicResult {
                topic: topic.clone(),
                hit_at_k,
                first_hit_rank,
                reciprocal_rank,
            });
        }
        let topics = per_topic.len();
        let hits = per_topic.iter().filter(|t| t.hit_at_k).count();
        let hit_rate_at_k = if topics == 0 {
            0.0
        } else {
            hits as f64 / topics as f64
        };
        let mrr = if topics == 0 {
            0.0
        } else {
            per_topic.iter().map(|t| t.reciprocal_rank).sum::<f64>() / topics as f64
        };
        EvalReport {
            k: self.k,
            topics,
            hit_rate_at_k,
            mrr,
            per_topic,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eval_toy_ground_truth_hit_rate_and_mrr() {
        let mut gt = BTreeMap::new();
        gt.insert(
            "web-framework".to_string(),
            vec!["github.com/o/w".to_string(), "github.com/o/w2".to_string()],
        );
        gt.insert("orm".to_string(), vec!["github.com/o/orm".to_string()]);
        let eval = AwesomeListEval::new(2);
        let report = eval.evaluate(&gt, |topic| match topic {
            "web-framework" => vec![
                "github.com/o/other".into(),
                "github.com/o/w".into(),
                "github.com/o/w2".into(),
            ],
            _ => vec!["github.com/o/unrelated".into(), "github.com/o/also".into()],
        });
        assert_eq!(report.topics, 2);
        // web-framework: first hit at rank 2 (within k=2) → hit, RR 0.5.
        // orm: no hit → miss, RR 0.
        assert!(approx(report.hit_rate_at_k, 0.5));
        assert!(approx(report.mrr, 0.25));
        // BTreeMap order: "orm" first, then "web-framework".
        assert_eq!(report.per_topic[0].topic, "orm");
        assert_eq!(report.per_topic[0].first_hit_rank, None);
        assert_eq!(report.per_topic[1].first_hit_rank, Some(2));
    }

    #[test]
    fn eval_mrr_beyond_k_counts_for_mrr_not_hits() {
        let mut gt = BTreeMap::new();
        gt.insert("t".to_string(), vec!["github.com/o/target".to_string()]);
        let eval = AwesomeListEval::new(1);
        let report = eval.evaluate(&gt, |_| {
            vec![
                "github.com/o/a".to_string(),
                "github.com/o/b".to_string(),
                "github.com/o/target".to_string(),
            ]
        });
        assert!(!report.per_topic[0].hit_at_k);
        assert!(approx(report.mrr, 1.0 / 3.0));
        assert!(approx(report.hit_rate_at_k, 0.0));
    }

    #[test]
    fn eval_matches_url_variants() {
        let mut gt = BTreeMap::new();
        gt.insert(
            "t".to_string(),
            vec!["https://github.com/Org/Repo".to_string()],
        );
        let eval = AwesomeListEval::new(3);
        let report = eval.evaluate(&gt, |_| {
            vec!["https://github.com/Org/Repo.git/".to_string()]
        });
        assert!(report.per_topic[0].hit_at_k);
        assert!(approx(report.mrr, 1.0));
    }

    #[test]
    fn eval_empty_ground_truth_is_zeroes() {
        let eval = AwesomeListEval::new(5);
        let report = eval.evaluate(&BTreeMap::new(), |_| vec!["github.com/x/y".to_string()]);
        assert_eq!(report.topics, 0);
        assert!(approx(report.hit_rate_at_k, 0.0));
        assert!(approx(report.mrr, 0.0));
    }

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }
}
