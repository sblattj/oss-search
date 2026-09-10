//! E2E regex query demo: load the saved real-corpus trigram index
//! (`data/corpus/index`) and run regex queries, printing matched repo, file,
//! line number, and line text so the hits are visible evidence.

use std::path::PathBuf;
use std::time::Instant;

use oss_index::{Filters, TrigramIndex};

fn main() {
    let root: PathBuf = std::env::var("OSS_CORPUS_ROOT")
        .unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../data/corpus").to_string())
        .into();
    let index_dir = root.join("index");
    let t = Instant::now();
    let idx = TrigramIndex::load(&index_dir).expect("load index");
    println!(
        "loaded {} docs in {:.2}s",
        idx.doc_count(),
        t.elapsed().as_secs_f64()
    );
    let filters = Filters::default();
    for pattern in [
        r"fn visit_\w+",
        r"class Retry\b",
        r"serialize_(u8|u16)",
        r"module\.exports",
    ] {
        let t = Instant::now();
        let out = idx
            .search_regex(pattern, &filters, 5)
            .unwrap_or_else(|e| panic!("regex {pattern:?} failed: {e}"));
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        assert!(!out.results.is_empty(), "regex {pattern:?}: no hits");
        println!(
            "\nregex /{pattern}/ — {} hits shown of {} candidates scanned, {ms:.2} ms",
            out.results.len(),
            out.candidates_scanned
        );
        for r in out.results.iter().take(3) {
            let m = &r.matches[0];
            let text: String = m.line_text.trim().chars().take(100).collect();
            println!(
                "  {}:{}:L{}  {}",
                r.repo, r.path, m.line_no, text
            );
        }
    }
    println!("\nREGEX DEMO PASS");
}
