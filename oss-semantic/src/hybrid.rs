//! Hybrid retrieval: Reciprocal Rank Fusion and query-shape routing.
//!
//! RRF reads only each document's *rank* in each input list — `score = Σ
//! 1/(k + rank)` — deliberately discarding score magnitude so bounded
//! cosine scores and unbounded BM25 scores are never compared directly.
//! Research note 14: for code workloads (few relevant docs per query) the
//! best `k` is 2–5; [`DEFAULT_RRF_K`] is 3. Identifier-shaped queries
//! should skip the dense leg entirely — BM25-class lexical matching wins on
//! exact symbols — which is what [`classify_query`] detects.

/// Default RRF `k` constant (rank smoothing). Research range for code: 2–5.
pub const DEFAULT_RRF_K: u32 = 3;

/// How a query should be routed through the retrieval pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryKind {
    /// Identifier-shaped (camelCase, snake_case, qualified paths, no
    /// spaces): route to the lexical index alone.
    Identifier,
    /// Natural-language / concept query: run dense + lexical and fuse.
    NaturalLanguage,
}

/// Per-list contribution of one fused document.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FusedComponents {
    /// 1-based rank in the lexical list, if present there.
    pub lexical_rank: Option<usize>,
    /// Raw lexical score, if present there.
    pub lexical_score: Option<f32>,
    /// 1-based rank in the dense list, if present there.
    pub dense_rank: Option<usize>,
    /// Raw dense score, if present there.
    pub dense_score: Option<f32>,
    /// This document's RRF contribution from the lexical list.
    pub lexical_contribution: f32,
    /// This document's RRF contribution from the dense list.
    pub dense_contribution: f32,
}

/// Fuse ranked lists with Reciprocal Rank Fusion.
///
/// `lexical` and `dense` are each `(id, score)` slices in *descending*
/// relevance order (rank 1 = best). Returns `(id, fused_score,
/// components)` sorted by fused score descending, ties broken by id for
/// determinism. The union of both lists is covered: a document found by
/// only one retriever still fuses (and ranks above documents found only
/// later by the other).
pub fn rrf_fuse(
    lexical: &[(String, f32)],
    dense: &[(String, f32)],
    k: u32,
) -> Vec<(String, f32, FusedComponents)> {
    // Collect contributions keyed by id, preserving first-seen order.
    let mut order: Vec<String> = Vec::new();
    let mut parts: std::collections::HashMap<String, FusedComponents> =
        std::collections::HashMap::new();

    let contribution = |rank: usize| 1.0 / (k as f32 + rank as f32);

    for (rank, (id, score)) in lexical.iter().enumerate() {
        let rank1 = rank + 1;
        let entry = parts.entry(id.clone()).or_insert_with(|| {
            order.push(id.clone());
            FusedComponents {
                lexical_rank: None,
                lexical_score: None,
                dense_rank: None,
                dense_score: None,
                lexical_contribution: 0.0,
                dense_contribution: 0.0,
            }
        });
        entry.lexical_rank = Some(rank1);
        entry.lexical_score = Some(*score);
        entry.lexical_contribution = contribution(rank1);
    }
    for (rank, (id, score)) in dense.iter().enumerate() {
        let rank1 = rank + 1;
        let entry = parts.entry(id.clone()).or_insert_with(|| {
            order.push(id.clone());
            FusedComponents {
                lexical_rank: None,
                lexical_score: None,
                dense_rank: None,
                dense_score: None,
                lexical_contribution: 0.0,
                dense_contribution: 0.0,
            }
        });
        entry.dense_rank = Some(rank1);
        entry.dense_score = Some(*score);
        entry.dense_contribution = contribution(rank1);
    }

    let mut fused: Vec<(String, f32, FusedComponents)> = order
        .into_iter()
        .map(|id| {
            let parts = &parts[&id];
            let total = parts.lexical_contribution + parts.dense_contribution;
            (id, total, *parts)
        })
        .collect();
    fused.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    fused
}

/// Fuse with the default `k` ([`DEFAULT_RRF_K`]).
pub fn rrf_fuse_default(
    lexical: &[(String, f32)],
    dense: &[(String, f32)],
) -> Vec<(String, f32, FusedComponents)> {
    rrf_fuse(lexical, dense, DEFAULT_RRF_K)
}

/// Heuristic query-shape classifier used for retrieval routing.
///
/// Identifier signals (any one is sufficient):
/// * no whitespace at all (`parseJSON`, `rate_limit`, `Vec::new`, `a.b.c`);
/// * a camelCase hump (`[a-z][A-Z]`) anywhere;
/// * a qualified-symbol shape — `::`, a trailing `()` call, or a leading
///   `.` / `$` member reference;
/// * an `all_snake_case_word` token with underscores joining word chars.
///
/// Otherwise the query is natural language. Callers route
/// [`QueryKind::Identifier`] to the lexical index alone and fuse both
/// retrievers for [`QueryKind::NaturalLanguage`].
pub fn classify_query(query: &str) -> QueryKind {
    let q = query.trim();
    if q.is_empty() {
        return QueryKind::NaturalLanguage;
    }
    if !q.contains(char::is_whitespace) {
        return QueryKind::Identifier;
    }
    // Multi-word: still identifier-shaped if it embeds a strong symbol.
    let has_hump = q
        .chars()
        .zip(q.chars().skip(1))
        .any(|(a, b)| a.is_lowercase() && b.is_uppercase());
    let has_snake_token = q.split_whitespace().any(|tok| {
        tok.len() > 1 && tok.contains('_') && tok.chars().all(|c| c.is_alphanumeric() || c == '_')
    });
    if has_hump
        || q.contains("::")
        || q.ends_with("()")
        || q.starts_with('.')
        || q.starts_with('$')
        || has_snake_token
    {
        QueryKind::Identifier
    } else {
        QueryKind::NaturalLanguage
    }
}
