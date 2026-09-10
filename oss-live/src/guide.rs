//! `oss_guide`: static, engine-independent description of the query grammar
//! and tool surface. The grammar facts are imported from oss-query constants
//! so this cannot drift from the parser.

use oss_query::KNOWN_JSON_FIELDS;
use serde_json::{json, Value};

pub fn guide_document() -> Value {
    json!({
        "engine": "live",
        "which_tool_when": [
            "oss_search_repos: discovery — 'does a tool for X exist?' (npm + ecosyste.ms, ranked with per-signal contributions)",
            "oss_search_code: vetting — 'who actually implements idiom X?' (github + grep.app code search, merged with per-backend attribution)",
            "oss_repo_profile: one-repo brief (ecosyste.ms + deps.dev merged)",
            "oss_repo_tree: file map via the GitHub trees API before guessing any path",
            "oss_fetch_file: byte-exact file read via the GitHub contents API, prefer line_range",
            "oss_fetch_docs: readme search via ecosyste.ms metadata + GitHub raw contents, with Source: breadcrumbs",
        ],
        "query_grammar": {
            "text_dialect": {
                "pattern": "bare words form the pattern; multiple words are one pattern",
                "filters": {
                    "repo:owner/name": "restrict to one repo (repeatable, comma-separated values allowed)",
                    "-repo:owner/name": "exclude a repo",
                    "lang:rust": "language filter (repeatable, negatable with -lang:)",
                    "path:src/": "path substring filter (repeatable, negatable with -path:)",
                    "symbol:main": "symbol filter (grep.app cannot honor it; results come back unfiltered with a warning)",
                    "case:yes|case:no": "case sensitivity (default no)",
                    "scope:def|test|comment|string|any": "content scope (grep.app cannot honor it)",
                    "in:hotset|remote|both": "repo scope: local index, remote backends, or both (default both)",
                    "type:literal|type:regex": "pattern kind (default literal)",
                },
                "example": "retry repo:tokio-rs/tokio lang:rust",
            },
            "json_fields": KNOWN_JSON_FIELDS,
            "json_example": {
                "pattern": "retry.*backoff",
                "kind": "regex",
                "case": "sensitive",
                "repos": ["tokio-rs/tokio"],
                "langs": ["rust"],
                "paths": ["src/"],
                "exclude_paths": ["tests/"],
                "repo_scope": "remote"
            },
            "regex_rules": [
                "RE2-style syntax only: no backreferences, no look-around",
                "a regex must contain a literal run of >= 3 characters to be plannable (trigram index requirement) — 'retry.*backoff' is fine, '[a-z]+' is rejected",
                "in oss_search_code probes, wrap regexes in slashes: '/early[Cc]loses/'",
            ],
        },
        "probe_writing": {
            "good": ["spawn_worker", "special_closes", "x-ratelimit-remaining", "/early[Cc]loses/"],
            "bad": ["market calendars", "rust http library", "error handling"],
            "rule": "search for code, not concepts; uniqueness beats correctness; 2-5 orthogonal probes per call",
        },
        "live_backend_matrix": {
            "github": "code search: legacy syntax only (no regex/symbol:/boolean), needs GITHUB_TOKEN, 10 req/min client-side, 1000 results/query cap",
            "grep.app": "regex-capable corpus of ~1M popular repos; the legacy JSON endpoint is undocumented and often bot-challenged (429/HTML) — tolerated as Unavailable, never faked",
            "npms.io": "npm package search + quality/popularity/maintenance scores; scores can be months stale",
            "ecosyste.ms": "repo/package metadata incl. dependents, stars, last push; 5000 req/hour per IP per service",
            "deps.dev": "exact package/version/advisory lookup across 7 systems",
            "github-contents": "trees + contents APIs; 5000 req/hour authenticated, 60/hour anonymous; files <= 1 MB",
        },
        "rate_limit_traps": "an empty result set with an error backend_status is a fetch failure, not a negative finding; partial=true means at least one backend failed while others answered",
        "license_classes": {
            "permissive": "MIT/Apache-2.0/BSD — safe to vendor",
            "copyleft": "GPL family — pattern-only study, linking has obligations",
            "AGPL": "network copyleft — treat as toxic for SaaS embedding",
        },
        "response_budget": "default calls stay under ~25KB; truncated=true means results were dropped to fit — narrow the query or page with next_cursor",
    })
}
