//! Candidate generation: merging candidates from multiple discovery sources,
//! deduped by canonical repo URL, carrying source provenance.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::signals::RepoSignals;

/// Where a candidate repo was discovered. Carried through ranking so the MCP
/// tool can answer "why was this even considered?".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CandidateSource {
    /// Registry search (npm search, PyPI, crates.io, GitHub search, ...).
    RegistrySearch,
    /// Curated awesome-list membership, with the list's name.
    AwesomeList { list: String },
    /// Neighborhood of the dependency graph around a known-good repo.
    DependentsNeighborhood { of: String },
    /// Manually supplied (user pin, prior conversation, etc.).
    Manual,
}

/// One raw discovery observation before dedup.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CandidateSeed {
    pub repo_url: String,
    pub source: CandidateSource,
    pub signals: RepoSignals,
}

impl CandidateSeed {
    pub fn new(repo_url: impl Into<String>, source: CandidateSource) -> Self {
        Self {
            repo_url: repo_url.into(),
            source,
            signals: RepoSignals::default(),
        }
    }
}

/// A deduped candidate: canonical repo URL, merged signals, union of
/// provenance, plus the graph centrality used by the ranker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    /// Canonical repo URL (see [`canonical_repo_url`]).
    pub repo_url: String,
    /// Distinct sources that surfaced this repo, in first-seen order.
    pub provenance: Vec<CandidateSource>,
    /// Field-wise merge of all observations.
    pub signals: RepoSignals,
    /// Normalized centrality in `[0, 1]` (e.g. PageRank max-normalized over
    /// the scored set — see [`crate::rank::normalize_by_max`]). Raw PageRank
    /// is scale-free and spans orders of magnitude; normalize before use.
    pub centrality: f64,
}

impl Candidate {
    pub fn new(repo_url: impl Into<String>) -> Self {
        Self {
            repo_url: canonical_repo_url(&repo_url.into()),
            provenance: Vec::new(),
            signals: RepoSignals::default(),
            centrality: 0.0,
        }
    }

    pub fn from_seed(seed: CandidateSeed) -> Self {
        let url = canonical_repo_url(&seed.repo_url);
        Self {
            repo_url: url,
            provenance: vec![seed.source],
            signals: seed.signals,
            centrality: 0.0,
        }
    }

    /// Builder: replace signals.
    pub fn with_signals(mut self, signals: RepoSignals) -> Self {
        self.signals = signals;
        self
    }

    /// Builder: set normalized centrality (clamped to `[0, 1]`).
    pub fn with_centrality(mut self, centrality: f64) -> Self {
        self.centrality = centrality.clamp(0.0, 1.0);
        self
    }

    /// Builder: append provenance.
    pub fn with_source(mut self, source: CandidateSource) -> Self {
        if !self.provenance.contains(&source) {
            self.provenance.push(source);
        }
        self
    }
}

/// Canonicalize a repo URL for dedup: lowercase scheme and host, strip a
/// trailing `.git` and trailing slashes, always emit `scheme://host/path`.
/// A scheme-less input is treated as `https`. Path case is preserved
/// (owner/repo redirects make matching case-insensitive in practice, but we
/// only canonicalize the parts that are unambiguous).
pub fn canonical_repo_url(raw: &str) -> String {
    let s = raw.trim();
    let (scheme, rest) = match s.split_once("://") {
        Some((scheme, rest)) => (scheme.to_ascii_lowercase(), rest.to_string()),
        None => ("https".to_string(), s.to_string()),
    };
    let (host, path) = match rest.split_once('/') {
        Some((h, p)) => (h.to_ascii_lowercase(), p.to_string()),
        None => (rest.to_ascii_lowercase(), String::new()),
    };
    let mut path = path.trim_end_matches('/').to_string();
    if let Some(stripped) = path.strip_suffix(".git") {
        path = stripped.trim_end_matches('/').to_string();
    }
    format!("{scheme}://{host}/{path}")
}

/// Merge candidate seeds from multiple sources into deduped candidates.
///
/// Dedup key is [`canonical_repo_url`]; output order is first-appearance
/// (deterministic for a given input). On merge, provenance is unioned in
/// first-seen order and signals merged via [`RepoSignals::absorb`].
pub fn merge_candidates(seeds: Vec<CandidateSeed>) -> Vec<Candidate> {
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut out: Vec<Candidate> = Vec::with_capacity(seeds.len());
    for seed in seeds {
        let url = canonical_repo_url(&seed.repo_url);
        match index.get(&url) {
            Some(&i) => {
                let existing = &mut out[i];
                if !existing.provenance.contains(&seed.source) {
                    existing.provenance.push(seed.source);
                }
                existing.signals.absorb(&seed.signals);
            }
            None => {
                index.insert(url.clone(), out.len());
                out.push(Candidate::from_seed(CandidateSeed {
                    repo_url: url,
                    source: seed.source,
                    signals: seed.signals,
                }));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_url_normalizes_case_git_and_slashes() {
        assert_eq!(
            canonical_repo_url("HTTPS://GitHub.com/Owner/Repo.git/"),
            "https://github.com/Owner/Repo"
        );
        assert_eq!(
            canonical_repo_url("github.com/Owner/Repo/"),
            "https://github.com/Owner/Repo"
        );
        assert_eq!(
            canonical_repo_url("https://github.com/Owner/Repo"),
            "https://github.com/Owner/Repo"
        );
    }

    #[test]
    fn merge_dedupes_url_variants_and_unions_provenance() {
        let seeds = vec![
            CandidateSeed::new(
                "https://github.com/Owner/Repo",
                CandidateSource::RegistrySearch,
            ),
            CandidateSeed::new(
                "https://github.com/Owner/Repo.git",
                CandidateSource::AwesomeList {
                    list: "awesome-web".into(),
                },
            ),
        ];
        let merged = merge_candidates(seeds);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].repo_url, "https://github.com/Owner/Repo");
        assert_eq!(
            merged[0].provenance,
            vec![
                CandidateSource::RegistrySearch,
                CandidateSource::AwesomeList {
                    list: "awesome-web".into()
                },
            ]
        );
    }

    #[test]
    fn merge_combines_signals_and_dedupes_provenance() {
        let mut s1 = CandidateSeed::new("github.com/a/b", CandidateSource::RegistrySearch);
        s1.signals.stars = 100;
        s1.signals.last_push_days_ago = Some(60);
        let mut s2 = CandidateSeed::new("github.com/a/b", CandidateSource::RegistrySearch);
        s2.signals.stars = 50;
        s2.signals.archived = true;
        let mut s3 = CandidateSeed::new(
            "https://github.com/a/b/",
            CandidateSource::DependentsNeighborhood {
                of: "github.com/x/y".into(),
            },
        );
        s3.signals.dependents_count = 40;

        let merged = merge_candidates(vec![s1, s2, s3]);
        assert_eq!(merged.len(), 1);
        let c = &merged[0];
        assert_eq!(c.signals.stars, 100);
        assert!(c.signals.archived);
        assert_eq!(c.signals.dependents_count, 40);
        assert_eq!(c.signals.last_push_days_ago, Some(60));
        assert_eq!(c.provenance.len(), 2);
    }

    #[test]
    fn merge_preserves_first_appearance_order() {
        let seeds = vec![
            CandidateSeed::new("github.com/z/last", CandidateSource::Manual),
            CandidateSeed::new("github.com/a/first", CandidateSource::Manual),
            CandidateSeed::new("github.com/z/last", CandidateSource::RegistrySearch),
        ];
        let merged = merge_candidates(seeds);
        let urls: Vec<&str> = merged.iter().map(|c| c.repo_url.as_str()).collect();
        assert_eq!(
            urls,
            vec!["https://github.com/z/last", "https://github.com/a/first"]
        );
    }
}
