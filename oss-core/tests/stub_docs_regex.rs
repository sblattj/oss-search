use oss_core::{call_tool, StubEngine};
use serde_json::json;

fn hit_lines(pattern: &str) -> Vec<u64> {
    let out = call_tool(
        &StubEngine::new(),
        "oss_fetch_docs",
        Some(json!({"repo": "gabe/hashopen", "pattern": pattern})),
    )
    .unwrap();
    out["results"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|r| r["hits"].as_array().unwrap().clone())
        .map(|h| h["line"].as_u64().unwrap())
        .collect()
}

#[test]
fn slash_patterns_are_real_regexes() {
    // docs/calendars.md: line 3 names NYSE, line 4 "half-day sessions", line 5 "half-days"
    assert_eq!(hit_lines("/nyse|half-day/"), vec![3, 4, 5]);
    assert_eq!(hit_lines("/[Ss]pecial.*closes/"), vec![4, 3]); // calendars.md:4, export.md:3
    assert_eq!(hit_lines("nyse"), vec![3]);
}

#[test]
fn invalid_regex_is_a_typed_error() {
    let err = call_tool(
        &StubEngine::new(),
        "oss_fetch_docs",
        Some(json!({"repo": "gabe/hashopen", "pattern": "/cho(se/"})),
    )
    .unwrap_err();
    assert!(format!("{err:?}").contains("invalid regex"));
}
