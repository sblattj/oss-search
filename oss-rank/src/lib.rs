//! oss-rank: repo discovery ranking signals and the multi-signal ranker for
//! agent-facing OSS search ("which library should I use").
//!
//! The crate has four layers:
//!
//! 1. [`RepoSignals`] — the raw, cheaply-fetchable per-repo signal set
//!    (GitHub repo object + releases + cached third-party scores).
//! 2. [`normalize`] — signal normalizers: min-max, the npms.io cubic-Bezier
//!    scoring curve, and saturating log-scale normalizations for heavy-tailed
//!    counts (stars, dependents).
//! 3. [`graph`] — [`DepGraph`], a uint32 CSR dependency graph (edges point
//!    dependent → dependency), with power-iteration PageRank (damping 0.85,
//!    dangling-mass correction) and a personalized-PageRank variant whose
//!    teleport vector concentrates on a seed set for domain-scoped ranking.
//! 4. [`rank`] — [`Ranker`], an explicitly weighted multi-signal combiner
//!    with a maintenance gate (archived / >2yr-stale tanks the score) that
//!    returns a per-signal contribution map with every result so the MCP
//!    tool can show *why* a repo ranked where it did.
//!
//! [`candidates`] merges candidate lists from multiple sources (registry
//! search, awesome lists, dependents neighborhoods) deduped by canonical repo
//! URL while carrying provenance; [`eval`] provides hit-rate@k and MRR
//! evaluation against awesome-list ground truth for offline tuning.

pub mod candidates;
pub mod eval;
pub mod graph;
pub mod normalize;
pub mod rank;
pub mod signals;

pub use candidates::{
    Candidate, CandidateSeed, CandidateSource, canonical_repo_url, merge_candidates,
};
pub use eval::{AwesomeListEval, EvalReport, TopicResult};
pub use graph::{DepGraph, PageRankResult};
pub use normalize::{Aggregation, log_saturate, min_max, npms_bezier_score, npms_score};
pub use rank::{RankResult, Ranker, RankerConfig, normalize_by_max};
pub use signals::RepoSignals;
