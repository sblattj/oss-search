//! Security-hardening tests: ReDoS-shaped patterns through query
//! validation (regex compile + trigram-planning gate) in bounded time.

use oss_query::{CaseMode, PatternKind, Query, QueryError};

const BOUND: std::time::Duration = std::time::Duration::from_secs(2);

fn regex_query(pattern: String) -> Query {
    Query {
        pattern,
        kind: PatternKind::Regex,
        case: CaseMode::Insensitive,
        ..Default::default()
    }
}

#[test]
fn validate_classic_alternation_bounded() {
    let start = std::time::Instant::now();
    let q = regex_query("(a|a)*$".into());
    let outcome = q.validate();
    assert!(start.elapsed() < BOUND, "(a|a)*$ validate exceeded bound");
    match outcome {
        Err(QueryError::NotIndexable { .. }) => {}
        Err(QueryError::RegexError { .. }) => {}
        Ok(()) => panic!("(a|a)*$ has no 3+ char literal run; must not validate"),
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn validate_nested_quantifier_bounded() {
    let start = std::time::Instant::now();
    let q = regex_query("(x+)+$".into());
    let outcome = q.validate();
    assert!(start.elapsed() < BOUND, "(x+)+$ validate exceeded bound");
    match outcome {
        Err(QueryError::NotIndexable { .. }) => {}
        Err(QueryError::RegexError { .. }) => {}
        Ok(()) => panic!("(x+)+$ has no 3+ char literal run; must not validate"),
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn validate_alternation_bomb_10kb_bounded() {
    // Bomb with only 2-char alternation arms: oss-query's trigram gate
    // (literal_trigrams_ok) requires SOME 3+ char literal run anywhere, so
    // 2-char arms leave it unindexable — but only AFTER compiling the full
    // 10KB alternation, the expensive step being bounded.
    let mut bomb = String::from("(");
    for _ in 0..3400 {
        bomb.push_str("aa|");
    }
    bomb.push_str("aa)$");
    assert!(bomb.len() > 10_000);
    let start = std::time::Instant::now();
    let outcome = regex_query(bomb).validate();
    assert!(
        start.elapsed() < BOUND,
        "10KB bomb validate took {:?}",
        start.elapsed()
    );
    assert!(
        matches!(outcome, Err(QueryError::NotIndexable { .. })),
        "2-char-arm bomb has no 3+ char literal run"
    );
}

#[test]
fn validate_alternation_bomb_with_literal_bounded() {
    // Trailing top-level literal clears the trigram gate: validation fully
    // compiles the 10KB alternation and returns Ok — in bounded time.
    let mut bomb = String::from("(");
    for _ in 0..2500 {
        bomb.push_str("aaaa|");
    }
    bomb.push_str("aaaa)zzzz$");
    let start = std::time::Instant::now();
    let outcome = regex_query(bomb).validate();
    assert!(
        start.elapsed() < BOUND,
        "indexable 10KB bomb validate took {:?}",
        start.elapsed()
    );
    assert!(outcome.is_ok());
}

#[test]
fn validate_hostile_nesting_shapes_bounded() {
    let hostile = [
        "((((((((((a))))))))))*$",
        "a{1000}{1000}{1000}$",
        "(((?:a|aa)+)+)+$",
        "(?:a|a){100}{100}{100}$",
    ];
    for pat in hostile {
        let start = std::time::Instant::now();
        let outcome = regex_query(pat.to_string()).validate();
        assert!(
            start.elapsed() < BOUND,
            "{pat} validate took {:?}",
            start.elapsed()
        );
        let _ = outcome;
    }
}
