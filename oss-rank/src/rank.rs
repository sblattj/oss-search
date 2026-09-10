//! The multi-signal [`Ranker`]: explicit, inspectable weights over normalized
//! signals plus a maintenance gate, returning a per-signal contribution map
//! with every result (a hard requirement for the MCP tool's "why did this
//! rank here?" answer).
//!
//! Default weights follow the ranking research: graph centrality (PageRank
//! over dependents) and direct dependents carry half the base weight —
//! load-bearing-ness is the signal neither stars nor downloads proxy; a
//! flat dependent count (OpenSSF criticality's heaviest signal) is kept
//! alongside centrality because centrality needs the graph while the count
//! is available per-repo. Maintenance acts as a gate multiplier on the whole
//! score: archived or >2yr-stale repos tank regardless of popularity, per
//! the "is this maintained" heuristics (archived = hard negative; libraries.io
//! uses ≤12mo for "maintained", Scorecard requires commit activity in 90d).
//! Awesome-list membership earns a small but real bonus: curation is the
//! strongest cheap proxy for human judgment the research found.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::candidates::Candidate;
use crate::normalize::log_saturate;

/// Per-signal weights and thresholds for the [`Ranker`].
///
/// Default base weights sum to 1.0: centrality 0.30 + dependents 0.20
/// (heaviest, per research), popularity 0.15, maintenance 0.15, quality
/// 0.10, awesome-list 0.05, HN 0.05. The maintenance gate then multiplies
/// the whole weighted base.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RankerConfig {
    /// Weight of graph centrality (normalized PageRank over dependents).
    pub weight_centrality: f64,
    /// Weight of direct-dependents count (log-saturated).
    pub weight_dependents: f64,
    /// Weight of popularity (log-saturated stars/forks blend).
    pub weight_popularity: f64,
    /// Weight of the maintenance composite (recency/cadence/response).
    pub weight_maintenance: f64,
    /// Weight of quality (OpenSSF score, license, tests heuristic).
    pub weight_quality: f64,
    /// Weight of awesome-list memberships (saturating at
    /// `awesome_saturation` lists).
    pub weight_awesome_list: f64,
    /// Weight of HN mentions (log-saturated).
    pub weight_hn_mentions: f64,
    /// Score multiplier when `archived` (hard negative; tanks the score).
    pub archived_penalty: f64,
    /// Days of no pushes past which the repo counts as stale (2 years).
    pub stale_days_threshold: u32,
    /// Score multiplier when the last push is older than
    /// `stale_days_threshold` (stacks with the archived penalty).
    pub stale_penalty: f64,
    /// Stars count that log-saturates to 1.0.
    pub stars_soft_cap: f64,
    /// Forks count that log-saturates to 1.0.
    pub forks_soft_cap: f64,
    /// Direct-dependents count that log-saturates to 1.0.
    pub dependents_soft_cap: f64,
    /// HN-mentions count that log-saturates to 1.0.
    pub hn_soft_cap: f64,
    /// Awesome-list memberships that saturate to 1.0.
    pub awesome_saturation: f64,
}

impl Default for RankerConfig {
    fn default() -> Self {
        Self {
            weight_centrality: 0.30,
            weight_dependents: 0.20,
            weight_popularity: 0.15,
            weight_maintenance: 0.15,
            weight_quality: 0.10,
            weight_awesome_list: 0.05,
            weight_hn_mentions: 0.05,
            archived_penalty: 0.25,
            stale_days_threshold: 730,
            stale_penalty: 0.50,
            stars_soft_cap: 30_000.0,
            forks_soft_cap: 5_000.0,
            dependents_soft_cap: 10_000.0,
            hn_soft_cap: 50.0,
            awesome_saturation: 3.0,
        }
    }
}

impl RankerConfig {
    /// Whether the base weights sum to 1.0 (the gate is applied after, so
    /// totals can still fall below 1.0 — that is intended).
    pub fn weights_sum_to_one(&self) -> bool {
        let s = self.weight_centrality
            + self.weight_dependents
            + self.weight_popularity
            + self.weight_maintenance
            + self.weight_quality
            + self.weight_awesome_list
            + self.weight_hn_mentions;
        (s - 1.0).abs() < 1e-9
    }
}

/// Scored result: the final score, and the per-signal contribution map whose
/// values sum exactly to the score (the gate enters as a negative
/// `maintenance_gate` entry when it bites).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RankResult {
    pub repo_url: String,
    pub score: f64,
    /// Contribution of each signal, in weight×normalized-signal units; sums
    /// to `score`. Keys: `centrality`, `dependents`, `popularity`,
    /// `maintenance`, `quality`, `awesome_list`, `hn_mentions`, and
    /// `maintenance_gate` (zero unless the gate bites).
    pub contributions: BTreeMap<String, f64>,
    /// The raw gate multiplier applied (1.0 when nothing bites).
    pub maintenance_gate: f64,
    /// Provenance carried through from the candidate.
    pub provenance: Vec<crate::candidates::CandidateSource>,
}

/// Normalize raw PageRank scores (scale-free, orders of magnitude of range)
/// into `[0, 1]` by dividing by the max. An all-zero or empty input maps to
/// all zeros.
pub fn normalize_by_max(values: &[f64]) -> Vec<f64> {
    let max = values.iter().copied().fold(0.0f64, f64::max);
    if max <= 0.0 {
        return vec![0.0; values.len()];
    }
    values.iter().map(|v| (v / max).clamp(0.0, 1.0)).collect()
}

/// The multi-signal ranker.
#[derive(Debug, Clone)]
pub struct Ranker {
    config: RankerConfig,
}

impl Ranker {
    pub fn new(config: RankerConfig) -> Self {
        Self { config }
    }

    pub fn with_default_config() -> Self {
        Self::new(RankerConfig::default())
    }

    pub fn config(&self) -> &RankerConfig {
        &self.config
    }

    /// Score one candidate, returning the full contribution map.
    pub fn score(&self, candidate: &Candidate) -> RankResult {
        let cfg = &self.config;
        let s = &candidate.signals;

        let popularity = 0.7 * log_saturate(s.stars as f64, cfg.stars_soft_cap)
            + 0.3 * log_saturate(s.forks as f64, cfg.forks_soft_cap);
        let dependents = log_saturate(s.dependents_count as f64, cfg.dependents_soft_cap);
        let centrality = candidate.centrality.clamp(0.0, 1.0);
        let maintenance = Self::maintenance_composite(s, cfg.stale_days_threshold);
        let quality = Self::quality_composite(s);
        let awesome = (s.awesome_list_memberships as f64 / cfg.awesome_saturation).clamp(0.0, 1.0);
        let hn = log_saturate(s.hn_mentions as f64, cfg.hn_soft_cap);

        let mut contributions: BTreeMap<String, f64> = BTreeMap::new();
        contributions.insert("centrality".into(), cfg.weight_centrality * centrality);
        contributions.insert("dependents".into(), cfg.weight_dependents * dependents);
        contributions.insert("popularity".into(), cfg.weight_popularity * popularity);
        contributions.insert("maintenance".into(), cfg.weight_maintenance * maintenance);
        contributions.insert("quality".into(), cfg.weight_quality * quality);
        contributions.insert("awesome_list".into(), cfg.weight_awesome_list * awesome);
        contributions.insert("hn_mentions".into(), cfg.weight_hn_mentions * hn);

        let base: f64 = contributions.values().sum();
        let gate = self.maintenance_gate(s);
        // The gate multiplies the whole base; entering it as
        // base·(gate − 1) keeps the map an exact decomposition of the score.
        contributions.insert("maintenance_gate".into(), base * (gate - 1.0));

        let score: f64 = contributions.values().sum();
        RankResult {
            repo_url: candidate.repo_url.clone(),
            score,
            contributions,
            maintenance_gate: gate,
            provenance: candidate.provenance.clone(),
        }
    }

    /// Score and rank candidates: descending score, ties broken by repo URL
    /// ascending — fully deterministic for identical input.
    pub fn rank(&self, candidates: &[Candidate]) -> Vec<RankResult> {
        let mut results: Vec<RankResult> = candidates.iter().map(|c| self.score(c)).collect();
        results.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.repo_url.cmp(&b.repo_url))
        });
        results
    }

    /// Maintenance gate multiplier: `archived_penalty` when archived,
    /// `stale_penalty` when the last push is older than
    /// `stale_days_threshold` (they stack — an archived 3-year-stale repo
    /// gets the product). Unknown push date does not trigger the gate.
    fn maintenance_gate(&self, s: &crate::signals::RepoSignals) -> f64 {
        let mut gate = 1.0;
        if s.archived {
            gate *= self.config.archived_penalty;
        }
        if matches!(s.last_push_days_ago, Some(d) if d > self.config.stale_days_threshold) {
            gate *= self.config.stale_penalty;
        }
        gate
    }

    /// Maintenance composite in `[0, 1]`, weighted 0.4 recency / 0.3 release
    /// cadence / 0.3 issue responsiveness. Recency decays linearly to 0 at
    /// the stale threshold; cadence saturates at 12 releases/year (monthly);
    /// responsiveness saturates at a 30-day median first response. Unknown
    /// components score neutral 0.5 rather than punishing missing data.
    fn maintenance_composite(s: &crate::signals::RepoSignals, stale_days: u32) -> f64 {
        let recency = match s.last_push_days_ago {
            None => 0.5,
            Some(d) => 1.0 - (d as f64 / stale_days as f64).clamp(0.0, 1.0),
        };
        let cadence = match s.release_cadence {
            None => 0.5,
            Some(c) => (c as f64 / 12.0).clamp(0.0, 1.0),
        };
        let response = match s.issue_response_days {
            None => 0.5,
            Some(r) => 1.0 - (r as f64 / 30.0).clamp(0.0, 1.0),
        };
        0.4 * recency + 0.3 * cadence + 0.3 * response
    }

    /// Quality composite in `[0, 1]`: 0.5 OpenSSF (0–10 rescaled; unknown =
    /// neutral 0.5), 0.25 license present, 0.25 tests heuristic.
    fn quality_composite(s: &crate::signals::RepoSignals) -> f64 {
        let openssf = match s.openssf_score {
            None => 0.5,
            Some(v) => (v as f64 / 10.0).clamp(0.0, 1.0),
        };
        0.5 * openssf
            + 0.25 * f64::from(s.license_present)
            + 0.25 * f64::from(s.has_tests_heuristic)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::candidates::CandidateSource;
    use crate::signals::RepoSignals;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-12
    }

    fn healthy_signals() -> RepoSignals {
        RepoSignals {
            stars: 12_000,
            forks: 800,
            dependents_count: 4_000,
            last_push_days_ago: Some(5),
            archived: false,
            release_cadence: Some(12.0),
            issue_response_days: Some(3.0),
            openssf_score: Some(7.0),
            awesome_list_memberships: 2,
            hn_mentions: 10,
            license_present: true,
            has_tests_heuristic: true,
        }
    }

    #[test]
    fn contributions_sum_exactly_to_score() {
        let ranker = Ranker::with_default_config();
        let c = Candidate::new("github.com/a/b")
            .with_signals(healthy_signals())
            .with_centrality(0.8)
            .with_source(CandidateSource::Manual);
        let r = ranker.score(&c);
        let sum: f64 = r.contributions.values().sum();
        assert!(approx(sum, r.score), "map sum {sum} != score {}", r.score);
        assert_eq!(r.contributions.len(), 8);
        assert!(r.contributions.contains_key("maintenance_gate"));
        assert!(r.score > 0.0 && r.score <= 1.0);
    }

    #[test]
    fn well_maintained_centrality_beats_archived_high_stars() {
        let ranker = Ranker::with_default_config();
        let good = Candidate::new("github.com/good/lib")
            .with_signals(healthy_signals())
            .with_centrality(0.9);
        let mut abandoned = RepoSignals {
            stars: 90_000,
            forks: 9_000,
            hn_mentions: 60,
            ..RepoSignals::default()
        };
        abandoned.last_push_days_ago = Some(900);
        abandoned.archived = true;
        let bad = Candidate::new("github.com/dead/lib").with_signals(abandoned);
        let ranked = ranker.rank(&[bad, good]);
        assert_eq!(ranked[0].repo_url, "https://github.com/good/lib");
        assert!(ranked[0].score > ranked[1].score * 3.0);
    }

    #[test]
    fn stale_over_threshold_tanks_via_gate() {
        let ranker = Ranker::with_default_config();
        let fresh = Candidate::new("github.com/a/fresh").with_signals(RepoSignals {
            last_push_days_ago: Some(30),
            ..healthy_signals()
        });
        let stale = Candidate::new("github.com/a/stale").with_signals(RepoSignals {
            last_push_days_ago: Some(800),
            ..healthy_signals()
        });
        let rf = ranker.score(&fresh);
        let rs = ranker.score(&stale);
        assert!(approx(rf.maintenance_gate, 1.0));
        assert!(approx(rs.maintenance_gate, 0.5));
        assert!(rf.score > rs.score);
    }

    #[test]
    fn archived_and_stale_penalties_stack() {
        let ranker = Ranker::with_default_config();
        let both = Candidate::new("github.com/a/both").with_signals(RepoSignals {
            archived: true,
            last_push_days_ago: Some(1_000),
            ..healthy_signals()
        });
        let r = ranker.score(&both);
        assert!(approx(r.maintenance_gate, 0.25 * 0.5));
    }

    #[test]
    fn unknown_push_date_does_not_trigger_gate() {
        let ranker = Ranker::with_default_config();
        let unknown = Candidate::new("github.com/a/unknown").with_signals(RepoSignals {
            last_push_days_ago: None,
            ..healthy_signals()
        });
        let r = ranker.score(&unknown);
        assert!(approx(r.maintenance_gate, 1.0));
    }

    #[test]
    fn awesome_list_bonus_matches_weight() {
        let ranker = Ranker::with_default_config();
        let base = healthy_signals();
        let without = Candidate::new("github.com/a/none").with_signals(RepoSignals {
            awesome_list_memberships: 0,
            ..base.clone()
        });
        let with = Candidate::new("github.com/a/some").with_signals(RepoSignals {
            awesome_list_memberships: 2,
            ..base
        });
        let r0 = ranker.score(&without);
        let r1 = ranker.score(&with);
        let cfg = RankerConfig::default();
        let expected = cfg.weight_awesome_list * (2.0 / cfg.awesome_saturation);
        assert!(approx(r1.score - r0.score, expected));
        assert!(r1.score > r0.score);
    }

    #[test]
    fn rank_is_deterministic_with_url_tiebreak() {
        let ranker = Ranker::with_default_config();
        let a = Candidate::new("github.com/z/z").with_signals(healthy_signals());
        let b = Candidate::new("github.com/a/a").with_signals(healthy_signals());
        let r1 = ranker.rank(&[a.clone(), b.clone()]);
        let r2 = ranker.rank(&[b, a]);
        assert_eq!(r1, r2);
        assert_eq!(r1[0].repo_url, "https://github.com/a/a");
    }

    #[test]
    fn centrality_and_dependents_dominate_default_ordering() {
        let ranker = Ranker::with_default_config();
        // Popular-and-curated but peripheral vs. load-bearing and healthy.
        let peripheral = Candidate::new("github.com/x/hot")
            .with_signals(RepoSignals {
                stars: 30_000,
                forks: 5_000,
                hn_mentions: 50,
                awesome_list_memberships: 3,
                last_push_days_ago: Some(10),
                release_cadence: Some(12.0),
                issue_response_days: Some(2.0),
                license_present: true,
                has_tests_heuristic: true,
                ..RepoSignals::default()
            })
            .with_centrality(0.0);
        let core = Candidate::new("github.com/x/core")
            .with_signals(RepoSignals {
                stars: 3_000,
                forks: 200,
                dependents_count: 9_000,
                last_push_days_ago: Some(10),
                release_cadence: Some(12.0),
                issue_response_days: Some(2.0),
                license_present: true,
                has_tests_heuristic: true,
                openssf_score: Some(7.0),
                ..RepoSignals::default()
            })
            .with_centrality(1.0);
        let ranked = ranker.rank(&[peripheral, core]);
        assert_eq!(ranked[0].repo_url, "https://github.com/x/core");
    }

    #[test]
    fn custom_config_single_signal_controls_ordering() {
        let cfg = RankerConfig {
            weight_centrality: 1.0,
            weight_dependents: 0.0,
            weight_popularity: 0.0,
            weight_maintenance: 0.0,
            weight_quality: 0.0,
            weight_awesome_list: 0.0,
            weight_hn_mentions: 0.0,
            ..RankerConfig::default()
        };
        let ranker = Ranker::new(cfg);
        let low = Candidate::new("github.com/a/low")
            .with_signals(healthy_signals())
            .with_centrality(0.1);
        let high = Candidate::new("github.com/a/high")
            .with_signals(RepoSignals {
                stars: 90_000,
                ..healthy_signals()
            })
            .with_centrality(0.9);
        let ranked = ranker.rank(&[low, high]);
        assert_eq!(ranked[0].repo_url, "https://github.com/a/high");
        assert!(approx(ranked[0].contributions["centrality"], 0.9));
    }

    #[test]
    fn normalize_by_max_basics() {
        assert_eq!(normalize_by_max(&[]), Vec::<f64>::new());
        assert_eq!(normalize_by_max(&[0.0, 0.0]), vec![0.0, 0.0]);
        let out = normalize_by_max(&[0.2, 1.0, 0.5]);
        assert!(approx(out[0], 0.2) && approx(out[1], 1.0) && approx(out[2], 0.5));
    }

    #[test]
    fn default_weights_sum_to_one() {
        assert!(RankerConfig::default().weights_sum_to_one());
    }
}
