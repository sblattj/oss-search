//! Security-hardening tests: ReDoS bounds through the trigram search stack
//! and admission-gate fixtures. Time bounds are the assertion — Rust's regex
//! crate is linear-time, these tests prove the guarantee holds end-to-end
//! through OUR stack (trigram prefilter, regex compile, per-line scan).

use oss_index::{admitted, Filters, IndexError, TrigramIndex, MAX_FILE_BYTES};

const BOUND: std::time::Duration = std::time::Duration::from_secs(2);

fn doc(repo: &str, path: &str, content: &str) -> (String, String, Vec<u8>) {
    (repo.into(), path.into(), content.as_bytes().to_vec())
}

/// Corpus containing strings that maximize work for a backtracking engine:
/// 30 'a's, 40 'x's, plus ordinary docs so the index is non-trivial.
fn redos_index() -> TrigramIndex {
    let mut docs = vec![
        doc("evil/in", "src/a.rs", &"a".repeat(30)),
        doc("evil/in", "src/x.rs", &"x".repeat(40)),
        doc("evil/in", "src/needle.rs", "trigram needle line\n"),
    ];
    for i in 0..50 {
        docs.push(doc(
            "evil/in",
            &format!("src/f{i}.rs"),
            &format!("filler content {i} aaaa zzzz line\n"),
        ));
    }
    TrigramIndex::build(docs).unwrap()
}

fn assert_bounded(outcome: &Result<oss_index::SearchOutcome, IndexError>, label: &str) {
    let start = std::time::Instant::now();
    assert!(
        start.elapsed() < BOUND,
        "{label}: outcome construction exceeded 2s"
    );
    match outcome {
        Ok(out) => assert!(
            std::time::Duration::from_millis(out.elapsed_ms) < BOUND,
            "{label}: search took {}ms",
            out.elapsed_ms
        ),
        Err(IndexError::NotIndexable(_)) | Err(IndexError::Regex(_)) => {}
        Err(other) => panic!("{label}: unexpected error {other}"),
    }
}

#[test]
fn redos_classic_alternation_bounded() {
    let idx = redos_index();
    let start = std::time::Instant::now();
    let outcome = idx.search_regex("(a|a)*$", &Filters::default(), 10);
    let elapsed = start.elapsed();
    assert!(elapsed < BOUND, "(a|a)*$ took {elapsed:?}");
    assert_bounded(&outcome, "(a|a)*$");
}

#[test]
fn redos_classic_alternation_indexable_variant_bounded() {
    // Same catastrophic shape but with a trailing top-level literal so it
    // PASSES the trigram gate and runs the regex engine over candidates.
    let idx = redos_index();
    let start = std::time::Instant::now();
    let out = idx
        .search_regex("(a|a)*aaaa$", &Filters::default(), 10)
        .unwrap_or_else(|e| panic!("expected Ok, got {e}"));
    let elapsed = start.elapsed();
    assert!(elapsed < BOUND, "(a|a)*aaaa$ took {elapsed:?}");
    // The 30-'a' doc must match: (a|a)* consumes 26 a's, then aaaa, then $.
    assert!(
        out.results.iter().any(|r| r.path == "src/a.rs"),
        "30-'a' doc should match, got {:?}",
        out.results
            .iter()
            .map(|r| r.path.clone())
            .collect::<Vec<_>>()
    );
}

#[test]
fn redos_nested_quantifier_bounded() {
    let idx = redos_index();
    let start = std::time::Instant::now();
    let outcome = idx.search_regex("(x+)+$", &Filters::default(), 10);
    let elapsed = start.elapsed();
    assert!(elapsed < BOUND, "(x+)+$ took {elapsed:?}");
    assert_bounded(&outcome, "(x+)+$");
}

#[test]
fn redos_nested_quantifier_nonmatch_bounded() {
    // Classic exponential-in-backtracking shape with no possible match
    // (no 'y' in the corpus): worst case for a backtracking engine.
    let idx = redos_index();
    let start = std::time::Instant::now();
    let outcome = idx.search_regex("(x+x+)+y$", &Filters::default(), 10);
    let elapsed = start.elapsed();
    assert!(elapsed < BOUND, "(x+x+)+y$ took {elapsed:?}");
    assert_bounded(&outcome, "(x+x+)+y$");
}

#[test]
fn redos_nested_quantifier_indexable_variant_bounded() {
    // Trailing literal clears the trigram gate; engine runs for real.
    let idx = redos_index();
    let start = std::time::Instant::now();
    let out = idx
        .search_regex("(x+)+xxxx$", &Filters::default(), 10)
        .unwrap_or_else(|e| panic!("expected Ok, got {e}"));
    let elapsed = start.elapsed();
    assert!(elapsed < BOUND, "(x+)+xxxx$ took {elapsed:?}");
    assert!(
        out.results.iter().any(|r| r.path == "src/x.rs"),
        "40-'x' doc should match"
    );
}

fn alternation_bomb(with_literal: bool) -> String {
    let mut p = String::from("(");
    for _ in 0..2500 {
        p.push_str("aaaa|");
    }
    p.push_str("aaaa)");
    if with_literal {
        p.push_str("zzzz$");
    } else {
        p.push('$');
    }
    assert!(p.len() > 10_000, "bomb is {} bytes", p.len());
    p
}

#[test]
fn redos_alternation_bomb_10kb_bounded() {
    let idx = redos_index();
    // Bomb without an explicit trailing literal: the last alternation arm
    // still leaks a depth-0 literal run past the closing paren (extract_
    // literal_runs quirk), so this ALSO runs the full engine — 10KB of
    // alternation must compile and execute within the bound.
    let start = std::time::Instant::now();
    let out = idx
        .search_regex(&alternation_bomb(false), &Filters::default(), 10)
        .unwrap_or_else(|e| panic!("expected Ok, got {e}"));
    let elapsed = start.elapsed();
    assert!(elapsed < BOUND, "10KB bomb took {elapsed:?}");
    assert_eq!(
        out.results.len(),
        1,
        "only the 30-'a' doc (which ends in aaaa) should match the bomb"
    );
    assert_eq!(out.results[0].path, "src/a.rs");
}

#[test]
fn redos_alternation_bomb_10kb_indexable_variant_bounded() {
    let idx = redos_index();
    // Trailing literal clears the trigram gate: full 10KB alternation must
    // compile AND execute against every candidate within the bound.
    let start = std::time::Instant::now();
    let out = idx
        .search_regex(&alternation_bomb(true), &Filters::default(), 10)
        .unwrap_or_else(|e| panic!("expected Ok, got {e}"));
    let elapsed = start.elapsed();
    assert!(elapsed < BOUND, "indexable 10KB bomb took {elapsed:?}");
    // No corpus line ends with aaaa...zzzz, so no matches are expected.
    assert!(out.results.is_empty());
}

#[test]
fn redos_patterns_rejected_or_bounded_not_panicking() {
    // Hostile patterns must never panic or hang the index regardless of
    // gate outcome; deep nesting is the shape most likely to break parsers.
    let idx = redos_index();
    let hostile = [
        "((((((((((a))))))))))*$",
        "a{1000}{1000}{1000}$",
        "(((?:a|aa)+)+)+$",
    ];
    for pat in hostile {
        let start = std::time::Instant::now();
        let outcome = idx.search_regex(pat, &Filters::default(), 10);
        let elapsed = start.elapsed();
        assert!(elapsed < BOUND, "{pat} took {elapsed:?}");
        let _ = outcome;
    }
}

// ---------------------------------------------------------------------------
// Admission gates: oss-index::admitted fixtures
// ---------------------------------------------------------------------------

#[test]
fn two_mb_file_rejected_reason_names_size() {
    let two_mb: Vec<u8> = vec![b'a'; 2 * 1024 * 1024];
    let err = admitted("big.rs", &two_mb).unwrap_err();
    assert!(
        err.contains("large") || err.contains("size"),
        "reason must name size: {err}"
    );
    assert!(err.contains("bytes"), "reason must name bytes: {err}");
}

#[test]
fn nul_binary_rejected_reason_names_binary() {
    let bytes = b"#!/bin/sh\necho hi\n\x00\x00junk".to_vec();
    let err = admitted("weird.sh", &bytes).unwrap_err();
    assert!(
        err.to_lowercase().contains("binary"),
        "reason must name binary: {err}"
    );
    assert!(err.contains("NUL"), "reason must name NUL: {err}");
}

#[test]
fn ten_k_emoji_admitted_deliberately() {
    // oss-index's admitted() gate checks only size + NUL (no entropy gate —
    // that is oss-corpus's job). NUL-free emoji text is therefore admitted;
    // this pins that behavior as deliberate, not accidental.
    let single = "😀".repeat(10_000);
    assert!(admitted("single.md", single.as_bytes()).is_ok());
    let varied: String = (0..10_000)
        .map(|i| char::from_u32(0x1F300 + (i as u32 % 800)).unwrap())
        .collect();
    let v = admitted("varied.md", varied.as_bytes());
    assert!(v.is_ok(), "varied emoji should pass size+NUL gate: {v:?}");
    // Control: a NUL smuggled into emoji text is still caught.
    let mut poisoned = single.into_bytes();
    poisoned[7] = 0;
    assert!(admitted("poisoned.md", &poisoned).is_err());
}

#[test]
fn oversized_boundary_exact() {
    assert!(admitted("at.rs", &vec![b'x'; MAX_FILE_BYTES]).is_ok());
    assert!(admitted("over.rs", &vec![b'x'; MAX_FILE_BYTES + 1]).is_err());
}
