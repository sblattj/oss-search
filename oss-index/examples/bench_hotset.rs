//! Measure REAL query latency against the saved hot-set index
//! (data/corpus/index, built by `index_hotset.rs`).
//!
//! Runs 56 queries — literal (case-insensitive + case-sensitive) and regex
//! — where every pattern is an identifier or shape verified to exist in the
//! real corpus (picked from serde, flask, lodash, urllib3, rayon, chrono,
//! clap, regex, hashbrown, smallvec, pydantic, click, requests, dayjs,
//! nanoid, node-deep-equal, gin, logrus, go-toml, ...). Asserts each query
//! returns at least one hit, collects p50/p90/p99, and writes
//! data/corpus/index/LATENCY.md.

use std::path::PathBuf;
use std::time::Instant;

use oss_index::{Filters, TrigramIndex};

#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    LiteralCI,
    LiteralCS,
    Regex,
}

struct Query {
    kind: Kind,
    pattern: &'static str,
    origin: &'static str,
}

const fn lit(pattern: &'static str, origin: &'static str) -> Query {
    Query {
        kind: Kind::LiteralCI,
        pattern,
        origin,
    }
}
const fn lit_cs(pattern: &'static str, origin: &'static str) -> Query {
    Query {
        kind: Kind::LiteralCS,
        pattern,
        origin,
    }
}
const fn rx(pattern: &'static str, origin: &'static str) -> Query {
    Query {
        kind: Kind::Regex,
        pattern,
        origin,
    }
}

const QUERIES: &[Query] = &[
    // --- literal, case-insensitive (search_literal) ---
    lit("Serialize", "serde"),
    lit("deserialize_struct", "serde de"),
    lit("serialize_str", "serde ser"),
    lit("Visitor", "serde de"),
    lit("route", "flask"),
    lit("render_template", "flask"),
    lit("before_request", "flask"),
    lit("debounce", "lodash"),
    lit("throttle", "lodash"),
    lit("memoize", "lodash"),
    lit("chunkSize", "lodash"),
    lit("backoff_factor", "urllib3 retry"),
    lit("get_backoff_time", "urllib3 retry"),
    lit("parse_obj_as", "pydantic"),
    lit("BaseModel", "pydantic"),
    lit("par_iter", "rayon"),
    lit("join_context", "rayon"),
    lit("BytesMut", "bytes"),
    lit("NaiveDateTime", "chrono"),
    lit("ArgMatches", "clap"),
    lit("GlobMatcher", "globset"),
    lit("RawTable", "hashbrown"),
    lit("maybe_uninit", "smallvec"),
    lit("urlAlphabet", "nanoid"),
    lit("deepEqual", "node-deep-equal"),
    lit("isBefore", "dayjs"),
    lit("WithError", "logrus"),
    lit("NewRandom", "go.uuid"),
    lit("Unmarshal", "go-toml"),
    lit("Middleware", "gin"),
    lit("Mount", "chi"),
    lit("stringify", "qs/json5"),
    lit("module.exports", "js packages"),
    lit("subscriber", "tracing"),
    // --- literal, case-sensitive (search_literal_case_sensitive) ---
    lit_cs("Serialize", "serde trait"),
    lit_cs("deserialize_struct", "serde macro"),
    lit_cs("Retry", "urllib3 class"),
    lit_cs("DateTime", "chrono"),
    lit_cs("debounce", "lodash"),
    lit_cs("deepEqual", "node-deep-equal"),
    lit_cs("Span", "tracing"),
    // --- regex (search_regex, case-sensitive, line-scoped) ---
    rx("function debounce", "lodash"),
    rx("fn visit_", "serde de"),
    rx("class Retry", "urllib3"),
    rx(r"self\.sleep", "urllib3 retry"),
    rx("serialize_(u8|u16)", "serde ser"),
    rx("impl.*Serializer", "serde ser"),
    rx(r"def __init__", "python corpus"),
    rx(r"fn main\(\)", "rust corpus"),
    rx(r"module\.exports", "javascript corpus"),
    rx(r"\.MustParse|ParseU", "go.uuid/google-uuid"),
];

fn percentile(sorted: &[f64], p: f64) -> f64 {
    debug_assert!(!sorted.is_empty());
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn main() {
    let root: PathBuf = std::env::var("OSS_CORPUS_ROOT")
        .unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../data/corpus").to_string())
        .into();
    let index_dir = root.join("index");
    assert!(
        index_dir.join("docs.json").exists(),
        "index not built: run `cargo run --release -p oss-index --example index_hotset` first"
    );

    let t_load = Instant::now();
    let idx = TrigramIndex::load(&index_dir).expect("load index");
    let load_secs = t_load.elapsed().as_secs_f64();
    println!(
        "loaded {} docs from {} in {load_secs:.2}s",
        idx.doc_count(),
        index_dir.display()
    );

    let filters = Filters::default();
    let mut rows: Vec<(&Query, f64, usize, u64)> = Vec::with_capacity(QUERIES.len());
    let t_all = Instant::now();
    for q in QUERIES {
        let t = Instant::now();
        let out = match q.kind {
            Kind::LiteralCI => idx.search_literal(q.pattern, &filters, 50),
            Kind::LiteralCS => idx.search_literal_case_sensitive(q.pattern, &filters, 50),
            Kind::Regex => idx.search_regex(q.pattern, &filters, 50),
        }
        .unwrap_or_else(|e| panic!("query {:?} failed: {e}", q.pattern));
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        assert!(
            !out.results.is_empty(),
            "pattern {:?} ({}): expected hits in the real corpus, got none",
            q.pattern,
            q.origin
        );
        rows.push((q, ms, out.results.len(), out.candidates_scanned));
    }
    let total_ms = t_all.elapsed().as_secs_f64() * 1000.0;

    let mut lat: Vec<f64> = rows.iter().map(|r| r.1).collect();
    lat.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p50 = percentile(&lat, 0.50);
    let p90 = percentile(&lat, 0.90);
    let p99 = percentile(&lat, 0.99);
    let max = lat[lat.len() - 1];

    println!(
        "\n{} queries: total {total_ms:.0}ms  p50={p50:.2}ms p90={p90:.2}ms p99={p99:.2}ms max={max:.2}ms",
        QUERIES.len()
    );
    for (q, ms, hits, scanned) in &rows {
        println!(
            "{:>7.2}ms  {:<28} {:<12} hits={:<3} scanned={}",
            ms,
            q.pattern,
            format!("{:?}", q.kind),
            hits,
            scanned
        );
    }

    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let mut md = String::new();
    md.push_str("# Hot-set query latency (real corpus)\n\n");
    md.push_str(&format!(
        "Measured {} against the trigram index built by `index_hotset.rs` over the real corpus \
         (`data/corpus/work`, {} files). Every pattern below was verified to exist in the corpus \
         and asserted to return at least one hit. Build profile: **{}** (`cargo run --{} -p \
         oss-index --example bench_hotset`). Index load time: {load_secs:.2}s; docs: {}.\n\n",
        "by `oss-index/examples/bench_hotset.rs`",
        idx.doc_count(),
        profile,
        profile,
        idx.doc_count(),
    ));
    md.push_str(&format!(
        "| stat | value |\n|---|---|\n| queries | {} |\n| p50 | {p50:.2} ms |\n| p90 | {p90:.2} ms \
         |\n| p99 | {p99:.2} ms |\n| max | {max:.2} ms |\n| total | {total_ms:.0} ms |\n| requirement \
         | p50 < 100 ms |\n| verdict | {} |\n\n",
        QUERIES.len(),
        if p50 < 100.0 { "PASS" } else { "FAIL" }
    ));
    md.push_str("| # | kind | pattern | origin | ms | hits | candidates scanned |\n|---|---|---|---|---|---|---|\n");
    for (i, (q, ms, hits, scanned)) in rows.iter().enumerate() {
        let kind = match q.kind {
            Kind::LiteralCI => "literal (ci)",
            Kind::LiteralCS => "literal (cs)",
            Kind::Regex => "regex",
        };
        md.push_str(&format!(
            "| {} | {} | `{}` | {} | {:.2} | {} | {} |\n",
            i + 1,
            kind,
            q.pattern,
            q.origin,
            ms,
            hits,
            scanned
        ));
    }
    let out_path = index_dir.join("LATENCY.md");
    std::fs::write(&out_path, md).expect("write LATENCY.md");
    println!("\nwrote {}", out_path.display());
    assert!(p50 < 100.0, "p50 {p50:.2}ms exceeds 100ms budget");
}
