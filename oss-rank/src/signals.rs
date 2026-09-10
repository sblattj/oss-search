//! Raw per-repo discovery signals.
//!
//! Every field here is Tier-0/Tier-1 fetchable per the ranking-signals
//! research: the GitHub repo object (stars/forks/pushed_at/archived/license)
//! rides along on one REST call, releases and cached scores (OpenSSF) are one
//! call each, and dependents counts come from registry/deps.dev metadata.

use serde::{Deserialize, Serialize};

/// Cheaply-fetchable per-repo signals for discovery ranking.
///
/// `Option` fields encode "not known" (the fetch layer fills what it can);
/// the ranker treats unknown values as neutral rather than punishing them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RepoSignals {
    /// GitHub stargazer count.
    pub stars: u64,
    /// GitHub fork count.
    pub forks: u64,
    /// Direct runtime dependents (NOT the transitive closure — transitive
    /// counts are dominated by mega-hubs and double-count monorepo edges).
    pub dependents_count: u64,
    /// Days since the last push to any branch; `None` = unknown.
    pub last_push_days_ago: Option<u32>,
    /// Hard dead-repo flag from the GitHub repo object.
    pub archived: bool,
    /// Releases per year (recent cadence); `None` = unknown.
    pub release_cadence: Option<f32>,
    /// Median days to first response on recent issues; `None` = unknown.
    pub issue_response_days: Option<f32>,
    /// OpenSSF Scorecard aggregate (0–10) when a cached score exists.
    pub openssf_score: Option<f32>,
    /// Number of curated awesome lists the repo appears in.
    pub awesome_list_memberships: u32,
    /// Hacker News story mentions (HN Algolia, joined on repo URL).
    pub hn_mentions: u32,
    /// A license file/detector hit is present.
    pub license_present: bool,
    /// Heuristic: tests or a CI test job appear to exist.
    pub has_tests_heuristic: bool,
}

impl RepoSignals {
    /// Absorb another observation of the same repo, used when merging
    /// candidates across sources. Deterministic conflict policy:
    /// counts take the max, booleans OR together, days-ago take the
    /// freshest (min), OpenSSF takes the conservative (min), and cadence
    /// keeps the first known value.
    pub fn absorb(&mut self, other: &RepoSignals) {
        self.stars = self.stars.max(other.stars);
        self.forks = self.forks.max(other.forks);
        self.dependents_count = self.dependents_count.max(other.dependents_count);
        self.awesome_list_memberships = self
            .awesome_list_memberships
            .max(other.awesome_list_memberships);
        self.hn_mentions = self.hn_mentions.max(other.hn_mentions);
        self.last_push_days_ago = min_opt(self.last_push_days_ago, other.last_push_days_ago);
        self.openssf_score = min_opt_f32(self.openssf_score, other.openssf_score);
        self.issue_response_days = min_opt_f32(self.issue_response_days, other.issue_response_days);
        if self.release_cadence.is_none() {
            self.release_cadence = other.release_cadence;
        }
        self.archived |= other.archived;
        self.license_present |= other.license_present;
        self.has_tests_heuristic |= other.has_tests_heuristic;
    }
}

fn min_opt(a: Option<u32>, b: Option<u32>) -> Option<u32> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (x, None) => x,
        (None, y) => y,
    }
}

fn min_opt_f32(a: Option<f32>, b: Option<f32>) -> Option<f32> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (x, None) => x,
        (None, y) => y,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn signals_serde_round_trip() {
        let s = RepoSignals {
            stars: 42_000,
            forks: 1_200,
            dependents_count: 3_400,
            last_push_days_ago: Some(12),
            archived: false,
            release_cadence: Some(14.5),
            issue_response_days: Some(3.25),
            openssf_score: Some(7.8),
            awesome_list_memberships: 2,
            hn_mentions: 17,
            license_present: true,
            has_tests_heuristic: true,
        };
        let json = serde_json::to_string(&s).expect("serialize");
        let back: RepoSignals = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, s);
    }

    #[test]
    fn signals_default_is_all_unknown_or_zero() {
        let s = RepoSignals::default();
        assert_eq!(s.stars, 0);
        assert!(!s.archived);
        assert_eq!(s.last_push_days_ago, None);
        assert_eq!(s.openssf_score, None);
    }

    #[test]
    fn absorb_takes_max_counts_or_flags_and_freshest_days() {
        let mut a = RepoSignals {
            stars: 100,
            dependents_count: 5,
            last_push_days_ago: Some(90),
            openssf_score: Some(6.0),
            archived: false,
            ..RepoSignals::default()
        };
        let b = RepoSignals {
            stars: 50,
            dependents_count: 9,
            last_push_days_ago: Some(10),
            openssf_score: Some(4.0),
            archived: true,
            release_cadence: Some(12.0),
            ..RepoSignals::default()
        };
        a.absorb(&b);
        assert_eq!(a.stars, 100);
        assert_eq!(a.dependents_count, 9);
        assert_eq!(a.last_push_days_ago, Some(10));
        assert!(approx(a.openssf_score.unwrap() as f64, 4.0));
        assert!(a.archived);
        assert!(approx(a.release_cadence.unwrap() as f64, 12.0));
    }
}
