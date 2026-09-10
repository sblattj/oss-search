use std::collections::BTreeMap;

use oss_rank::{
    AwesomeListEval, Candidate, CandidateSource, EvalReport, Ranker, RepoSignals,
    canonical_repo_url,
};
use serde::{Deserialize, Serialize};

pub const HIT_K: usize = 10;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecoyRank {
    pub topic: String,
    pub repo_url: String,
    pub rank: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepoEvalReport {
    pub eval: EvalReport,
    pub decoy_ranks: Vec<DecoyRank>,
}

const DECOY_ARCHIVED_SUFFIX: &str = "-classic";
const DECOY_VAPORWARE_SUFFIX: &str = "-next";

fn topic_word(topic: &str) -> String {
    topic
        .split_whitespace()
        .last()
        .unwrap_or("misc")
        .to_string()
}

fn gt_signals(top: bool) -> RepoSignals {
    if top {
        RepoSignals {
            stars: 27_000,
            forks: 2_100,
            dependents_count: 9_200,
            last_push_days_ago: Some(4),
            archived: false,
            release_cadence: Some(12.0),
            issue_response_days: Some(2.5),
            openssf_score: Some(8.0),
            awesome_list_memberships: 3,
            hn_mentions: 38,
            license_present: true,
            has_tests_heuristic: true,
        }
    } else {
        RepoSignals {
            stars: 11_000,
            forks: 900,
            dependents_count: 6_100,
            last_push_days_ago: Some(12),
            archived: false,
            release_cadence: Some(10.0),
            issue_response_days: Some(4.0),
            openssf_score: Some(7.2),
            awesome_list_memberships: 2,
            hn_mentions: 12,
            license_present: true,
            has_tests_heuristic: true,
        }
    }
}

fn mid_signals() -> RepoSignals {
    RepoSignals {
        stars: 20_000,
        forks: 1_500,
        dependents_count: 320,
        last_push_days_ago: Some(21),
        archived: false,
        release_cadence: Some(4.0),
        issue_response_days: Some(10.0),
        openssf_score: Some(5.5),
        awesome_list_memberships: 1,
        hn_mentions: 25,
        license_present: true,
        has_tests_heuristic: false,
    }
}

fn decoy_archived_signals() -> RepoSignals {
    RepoSignals {
        stars: 90_000,
        forks: 9_000,
        dependents_count: 1_250,
        last_push_days_ago: Some(950),
        archived: true,
        release_cadence: Some(0.0),
        issue_response_days: None,
        openssf_score: Some(4.0),
        awesome_list_memberships: 1,
        hn_mentions: 80,
        license_present: true,
        has_tests_heuristic: true,
    }
}

fn decoy_vaporware_signals() -> RepoSignals {
    RepoSignals {
        stars: 5_000,
        forks: 190,
        dependents_count: 40,
        last_push_days_ago: Some(5),
        archived: false,
        release_cadence: Some(0.5),
        issue_response_days: Some(28.0),
        openssf_score: Some(2.0),
        awesome_list_memberships: 0,
        hn_mentions: 45,
        license_present: false,
        has_tests_heuristic: false,
    }
}

pub fn ground_truth() -> BTreeMap<String, Vec<String>> {
    let mut gt = BTreeMap::new();
    let mut add = |topic: &str, repos: &[&str]| {
        gt.insert(
            topic.to_string(),
            repos.iter().map(|r| r.to_string()).collect(),
        );
    };
    add(
        "rust async runtime",
        &["github.com/tokio-rs/tokio", "github.com/smol-rs/async-std"],
    );
    add(
        "rust http client",
        &[
            "github.com/seanmonstar/reqwest",
            "github.com/hyperium/hyper",
        ],
    );
    add(
        "rust json serialization",
        &["github.com/serde-rs/json", "github.com/serde-rs/serde"],
    );
    add(
        "rust logging",
        &["github.com/rust-lang/log", "github.com/tokio-rs/tracing"],
    );
    add(
        "rust error handling",
        &["github.com/dtolnay/anyhow", "github.com/dtolnay/thiserror"],
    );
    add(
        "rust lru cache",
        &["github.com/moka-rs/moka", "github.com/jeromefroe/lru-rs"],
    );
    gt
}

pub fn candidate_pool(topic: &str) -> Vec<Candidate> {
    let word = topic_word(topic);
    let gt_urls = ground_truth().get(topic).cloned().unwrap_or_default();
    let mut pool = Vec::new();
    for (i, url) in gt_urls.iter().enumerate() {
        pool.push(
            Candidate::new(url)
                .with_signals(gt_signals(i == 0))
                .with_centrality(if i == 0 { 0.95 } else { 0.80 })
                .with_source(CandidateSource::AwesomeList {
                    list: format!("awesome-{word}"),
                }),
        );
    }
    pool.push(
        Candidate::new(format!("github.com/brightideas/{word}-lite"))
            .with_signals(mid_signals())
            .with_centrality(0.35)
            .with_source(CandidateSource::RegistrySearch),
    );
    pool.push(
        Candidate::new(format!("github.com/hobbyist/{word}-kit"))
            .with_signals(mid_signals())
            .with_centrality(0.42)
            .with_source(CandidateSource::RegistrySearch),
    );
    pool.push(
        Candidate::new(format!(
            "github.com/legacy-archive/{word}{DECOY_ARCHIVED_SUFFIX}"
        ))
        .with_signals(decoy_archived_signals())
        .with_centrality(0.55)
        .with_source(CandidateSource::RegistrySearch),
    );
    pool.push(
        Candidate::new(format!(
            "github.com/vaporware/{word}{DECOY_VAPORWARE_SUFFIX}"
        ))
        .with_signals(decoy_vaporware_signals())
        .with_centrality(0.10)
        .with_source(CandidateSource::RegistrySearch),
    );
    pool
}

pub fn run() -> RepoEvalReport {
    let gt = ground_truth();
    let eval = AwesomeListEval::new(HIT_K);
    let report = eval.evaluate(&gt, |topic| {
        let ranked = Ranker::with_default_config().rank(&candidate_pool(topic));
        ranked.into_iter().map(|r| r.repo_url).collect()
    });
    let mut decoy_ranks = Vec::new();
    for topic in gt.keys() {
        let ranked = Ranker::with_default_config().rank(&candidate_pool(topic));
        let word = topic_word(topic);
        for decoy in [
            format!("github.com/legacy-archive/{word}{DECOY_ARCHIVED_SUFFIX}"),
            format!("github.com/vaporware/{word}{DECOY_VAPORWARE_SUFFIX}"),
        ] {
            let canonical = canonical_repo_url(&decoy);
            let rank = ranked
                .iter()
                .position(|r| r.repo_url == canonical)
                .map_or(usize::MAX, |i| i + 1);
            decoy_ranks.push(DecoyRank {
                topic: topic.clone(),
                repo_url: canonical,
                rank,
            });
        }
    }
    RepoEvalReport {
        eval: report,
        decoy_ranks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ground_truth_covers_at_least_five_topics() {
        let gt = ground_truth();
        assert!(gt.len() >= 5);
        for (topic, expected) in &gt {
            assert!(!expected.is_empty(), "topic {topic} has no expected repos");
        }
    }

    #[test]
    fn ranker_hits_every_topic_in_top_10() {
        let report = run();
        assert_eq!(report.eval.topics, ground_truth().len());
        assert!((report.eval.hit_rate_at_k - 1.0).abs() < 1e-9);
        assert!(report.eval.mrr >= 0.9, "mrr {}", report.eval.mrr);
    }

    #[test]
    fn decoys_rank_below_every_ground_truth_repo() {
        let report = run();
        for topic_result in &report.eval.per_topic {
            let ranked = Ranker::with_default_config().rank(&candidate_pool(&topic_result.topic));
            let gt = ground_truth();
            let expected: Vec<String> = gt[&topic_result.topic]
                .iter()
                .map(|u| canonical_repo_url(u))
                .collect();
            let worst_gt_rank = ranked
                .iter()
                .filter(|r| expected.contains(&r.repo_url))
                .map(|r| {
                    ranked
                        .iter()
                        .position(|x| x.repo_url == r.repo_url)
                        .unwrap()
                        + 1
                })
                .max()
                .expect("a ground truth repo is ranked");
            for decoy in &report.decoy_ranks {
                if decoy.topic == topic_result.topic {
                    assert!(
                        decoy.rank > worst_gt_rank,
                        "{}: decoy {} at {} not below gt worst {}",
                        topic_result.topic,
                        decoy.repo_url,
                        decoy.rank,
                        worst_gt_rank
                    );
                }
            }
        }
    }
}
