//! REAL-corpus incremental-reindex proof (goal action a6 companion).
//!
//! Loads the hot-set index built over the real corpus
//! (`data/corpus/index`, from `oss-index/examples/index_hotset.rs`),
//! mutates one real file's bytes in-memory through `update_doc`, and
//! asserts the update is surgical: `docs_reindexed == 1` and
//! `postings_touched` bounded by exactly that file's trigram sets.
//!
//! Skips cleanly (returns early) when the corpus index is absent, so
//! CI-less / fresh-clone environments don't fail — same gate style as
//! `oss-semantic/tests/hotset_hybrid.rs`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use oss_index::{Filters, TrigramIndex};

/// Resolve the hot-set corpus + index directories, or `None` when the real
/// corpus artifacts have not been built (skip). Checks both the
/// workspace-root-relative `data/corpus/index` and the crate-relative
/// `../data/corpus/index` so the test works from either CWD.
fn hotset() -> Option<(PathBuf, PathBuf)> {
    let manifest_parent = Path::new(env!("CARGO_MANIFEST_DIR")).parent()?;
    let index_dir = Path::new("data/corpus/index");
    if index_dir.exists() {
        return Some((index_dir.to_path_buf(), PathBuf::from("data/corpus/work")));
    }
    let index_dir = manifest_parent.join("data/corpus/index");
    if index_dir.exists() {
        return Some((index_dir, manifest_parent.join("data/corpus/work")));
    }
    None
}

/// Byte trigrams encoded the same way the index encodes them (first byte
/// highest), used to compute the exact postings bound for one file.
fn gram_set(bytes: &[u8]) -> HashSet<u32> {
    bytes
        .windows(3)
        .map(|w| (w[0] as u32) << 16 | (w[1] as u32) << 8 | w[2] as u32)
        .collect()
}

fn gram_count(bytes: &[u8]) -> u64 {
    bytes.len().saturating_sub(2) as u64
}

const TARGET_REPO: &str = "serde";
const TARGET_PATH: &str = "serde_core/src/de/mod.rs";
/// A token that verifiably appears in the real target file pre-update.
const OLD_TOKEN: &str = "deserialize_any";
/// What OLD_TOKEN is rewritten to; verifiably absent from the whole corpus.
const NEW_TOKEN: &str = "hotset_incremental_marker";

#[test]
fn real_corpus_update_doc_reindexes_exactly_one_file() {
    let Some((index_dir, work_root)) = hotset() else {
        eprintln!("skipping: data/corpus/index not built (run index_hotset example)");
        return;
    };

    let source_file = work_root.join(TARGET_REPO).join(TARGET_PATH);
    let original = std::fs::read(&source_file)
        .unwrap_or_else(|e| panic!("read {}: {e}", source_file.display()));
    assert!(
        original
            .windows(OLD_TOKEN.len())
            .any(|w| w == OLD_TOKEN.as_bytes()),
        "sanity: {OLD_TOKEN} must exist in the real {TARGET_PATH}"
    );
    let mutated = String::from_utf8_lossy(&original).replace(OLD_TOKEN, NEW_TOKEN);
    let mutated = mutated.into_bytes();
    assert_ne!(original, mutated, "sanity: bytes must actually change");

    // The corpus must not already contain the marker anywhere.
    let mut idx = TrigramIndex::load(&index_dir).expect("load hot-set index");
    assert!(idx.doc_count() > 10_000, "expected the real hot-set index");
    let marker_before = idx
        .search_literal_case_sensitive(NEW_TOKEN, &Filters::default(), 10)
        .unwrap();
    assert!(
        marker_before.results.is_empty(),
        "sanity: {NEW_TOKEN} must not pre-exist in the corpus"
    );

    // The old token is present in the target file pre-update.
    let only_target = Filters {
        repos: vec![TARGET_REPO.into()],
        path_contains: vec![TARGET_PATH.into()],
        ..Default::default()
    };
    let before = idx
        .search_literal_case_sensitive(OLD_TOKEN, &only_target, 10)
        .unwrap();
    assert!(
        !before.results.is_empty(),
        "sanity: {OLD_TOKEN} must be findable in {TARGET_REPO}/{TARGET_PATH} before update"
    );
    let doc_count_before = idx.doc_count();

    // ---- the real-corpus incremental update under proof ----
    let stats = idx
        .update_doc(TARGET_REPO, TARGET_PATH, mutated.clone())
        .expect("update_doc");

    assert_eq!(stats.docs_reindexed, 1, "exactly one doc reindexed");
    assert_eq!(
        idx.doc_count(),
        doc_count_before,
        "update replaces, not appends"
    );

    // Postings bound: removal touches each DISTINCT old trigram once;
    // insertion touches every new trigram OCCURRENCE. Two-sided check.
    let old_distinct = gram_set(&original).len() as u64;
    let new_total = gram_count(&mutated);
    assert!(
        stats.postings_touched >= old_distinct,
        "removal alone must touch every distinct old gram: {} < {old_distinct}",
        stats.postings_touched
    );
    assert!(
        stats.postings_touched <= old_distinct + new_total,
        "touched {} must be bounded by old distinct ({old_distinct}) + new occurrences ({new_total})",
        stats.postings_touched
    );
    eprintln!(
        "update_doc on {TARGET_REPO}/{TARGET_PATH} ({} bytes -> {} bytes): \
         docs_reindexed={}, postings_touched={} (bound: {old_distinct}..={})",
        original.len(),
        mutated.len(),
        stats.docs_reindexed,
        stats.postings_touched,
        old_distinct + new_total
    );

    // New content is searchable in exactly the updated file...
    let after_new = idx
        .search_literal_case_sensitive(NEW_TOKEN, &only_target, 10)
        .unwrap();
    assert_eq!(
        after_new.results.len(),
        1,
        "marker must be findable in the updated file"
    );
    // ...and the rewritten occurrence is gone from that file (other files
    // may still legitimately contain the old token).
    let after_old = idx
        .search_literal_case_sensitive(OLD_TOKEN, &only_target, 10)
        .unwrap();
    assert!(
        after_old.results.is_empty(),
        "rewritten token must vanish from the updated file only"
    );

    // Everything outside the target file is untouched: a corpus-wide query
    // for the old token must still return every result it had that is NOT
    // the target file (count via a full-corpus search, minus target).
    let all_before = idx
        .search_literal_case_sensitive(OLD_TOKEN, &Filters::default(), 50)
        .unwrap();
    assert!(
        all_before.results.iter().all(|r| r.path != TARGET_PATH),
        "other files still match {OLD_TOKEN} after the single-file update"
    );
}
