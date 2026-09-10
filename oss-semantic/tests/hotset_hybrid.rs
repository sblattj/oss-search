//! REAL-corpus routing + hybrid retrieval proof (goal action a7).
//!
//! Against the semantic layer built by `embed_hotset.rs` over the real
//! corpus (`data/corpus/semantic`) and the trigram index built by
//! `index_hotset.rs` (`data/corpus/index`):
//!
//! (a) `classify_query("deserialize_struct")` is `Identifier` and routes
//!     lexical-only — ungated, pure function.
//! (b) the natural-language query "retry a failed request with exponential
//!     backoff" runs dense top-k + lexical search fused with RRF and a REAL
//!     backoff/retry chunk (urllib3's `Retry` class in `util/retry.py`,
//!     home of `get_backoff_time`/`_sleep_backoff`, found by grep first)
//!     appears in the fused top-10.
//! (c) routing decisions are logged.
//!
//! Gated: every corpus-dependent test returns early when the artifacts are
//! absent, so fresh clones / CI-less environments skip cleanly.

use std::path::{Path, PathBuf};

use oss_index::{Filters, TrigramIndex};
use oss_semantic::chunk_store::ChunkStore;
use oss_semantic::embedding::{Embedder, StubEmbedder};
use oss_semantic::hybrid::{self, QueryKind};
use oss_semantic::vector_store::{FlatVectorStore, MetadataFilter, VectorStore};

const NL_QUERY: &str = "retry a failed request with exponential backoff";
/// The distinctive lexical anchor for the NL query, verified to exist.
const LEXICAL_ANCHOR: &str = "backoff";
/// Target file (found by grep): urllib3's Retry implementation, home of
/// `get_backoff_time`, `_sleep_backoff`, and `sleep`.
const TARGET_PATH_SUFFIX: &str = "util/retry.py";
const CANDIDATE_DEPTH: usize = 30;

/// Resolve the semantic + index dirs when both artifacts exist.
fn hotset_dirs() -> Option<(PathBuf, PathBuf)> {
    let manifest_parent = Path::new(env!("CARGO_MANIFEST_DIR")).parent()?;
    for (sem, idx) in [
        (
            Path::new("data/corpus/semantic"),
            Path::new("data/corpus/index"),
        ),
        (
            &manifest_parent.join("data/corpus/semantic"),
            &manifest_parent.join("data/corpus/index"),
        ),
    ] {
        if sem.join("vectors.ossv").exists() && idx.join("docs.json").exists() {
            return Some((sem.to_path_buf(), idx.to_path_buf()));
        }
    }
    None
}

fn make_query_embedder(meta_dim: usize, embedder_name: &str) -> Option<Box<dyn Embedder>> {
    match embedder_name {
        "stub" => {
            eprintln!("store built with StubEmbedder (dim {meta_dim}): stub query embedding");
            Some(Box::new(StubEmbedder::new(meta_dim)))
        }
        "fastembed" => {
            #[cfg(feature = "fastembed")]
            {
                match oss_semantic::fast_embedder::FastEmbedder::default_model() {
                    Ok(e) => {
                        eprintln!("query embedder: fastembed BGESmallENV15 (dim {})", e.dim());
                        Some(Box::new(e))
                    }
                    Err(err) => {
                        eprintln!(
                            "skipping dense leg: store is fastembed-built but init failed: {err}"
                        );
                        None
                    }
                }
            }
            #[cfg(not(feature = "fastembed"))]
            {
                let _ = meta_dim;
                eprintln!(
                    "skipping dense leg: store is fastembed-built; rerun with \
                     `cargo test -p oss-semantic --features fastembed`"
                );
                None
            }
        }
        other => {
            eprintln!("skipping dense leg: unknown embedder {other:?} in meta.json");
            None
        }
    }
}

// ---------------------------------------------------------------- (a) + (c)

#[test]
fn routing_identifier_vs_natural_language() {
    let identifier = "deserialize_struct";
    let verdict = hybrid::classify_query(identifier);
    println!("routing: {identifier:?} -> {verdict:?}");
    assert_eq!(verdict, QueryKind::Identifier);
    assert_eq!(verdict, hybrid::classify_query("debounce"));
    assert_eq!(verdict, hybrid::classify_query("get_backoff_time"));

    let nl = hybrid::classify_query(NL_QUERY);
    println!("routing: {NL_QUERY:?} -> {nl:?}");
    assert_eq!(nl, QueryKind::NaturalLanguage);
    // Routing consequence: identifier queries never hit the dense leg.
    println!(
        "consequence: identifier queries route lexical-only; NL queries run dense + lexical and fuse"
    );
}

// ------------------------------------------------------------------- (b)+(c)

#[test]
fn hybrid_nl_query_fuses_to_real_backoff_chunk() {
    let Some((sem_dir, index_dir)) = hotset_dirs() else {
        eprintln!("skipping: data/corpus/{{semantic,index}} not built");
        return;
    };

    let meta: serde_json::Value =
        serde_json::from_slice(&std::fs::read(sem_dir.join("meta.json")).unwrap()).unwrap();
    let embedder_name = meta["embedder"].as_str().unwrap_or("?");
    let dim = meta["dim"].as_u64().unwrap_or(0) as usize;
    println!(
        "routing: {NL_QUERY:?} -> {:?}",
        hybrid::classify_query(NL_QUERY)
    );

    let vectors = FlatVectorStore::load(sem_dir.join("vectors.ossv")).expect("load vectors");
    let chunks = ChunkStore::open(sem_dir.join("chunks.db")).expect("open chunks.db");
    assert!(vectors.len() > 1_000, "expected the real semantic store");

    // ---- lexical leg: trigram index over the real corpus ----
    let index = TrigramIndex::load(&index_dir).expect("load trigram index");
    let lexical_files = index
        .search_literal_case_sensitive(LEXICAL_ANCHOR, &Filters::default(), CANDIDATE_DEPTH)
        .expect("lexical search");
    assert!(
        !lexical_files.results.is_empty(),
        "lexical anchor must hit the corpus"
    );
    let mut lexical: Vec<(String, f32)> = Vec::new();
    'file: for file in &lexical_files.results {
        // Chunk ids/path are repo-prefixed by embed_hotset; expand each
        // ranked file to its chunks that actually contain the anchor.
        let prefixed = format!("{}/{}", file.repo, file.path);
        let Ok(file_chunks) = chunks.by_path(&file.repo, &prefixed) else {
            continue;
        };
        for c in file_chunks {
            if c.text.to_lowercase().contains(LEXICAL_ANCHOR) {
                lexical.push((c.embedding_id.clone(), file.score));
                if lexical.len() >= CANDIDATE_DEPTH {
                    break 'file;
                }
            }
        }
    }
    println!(
        "lexical leg: {} files -> {} chunks containing {LEXICAL_ANCHOR:?}; top: {:?}",
        lexical_files.results.len(),
        lexical.len(),
        lexical
            .iter()
            .take(3)
            .map(|(id, _)| id.as_str())
            .collect::<Vec<_>>()
    );

    // ---- dense leg (embedder must match the store) ----
    let Some(embedder) = make_query_embedder(dim, embedder_name) else {
        return;
    };
    let qv = &embedder
        .embed(&[NL_QUERY.to_string()])
        .expect("embed query")[0];
    let dense_hits = vectors.search(qv, CANDIDATE_DEPTH, &MetadataFilter::none());
    let dense: Vec<(String, f32)> = dense_hits.iter().map(|h| (h.id.clone(), h.score)).collect();
    println!(
        "dense leg ({}): top-3 {:?}",
        embedder_name,
        dense
            .iter()
            .take(3)
            .map(|(id, s)| (id.as_str(), *s))
            .collect::<Vec<_>>()
    );

    // ---- fuse ----
    let fused = hybrid::rrf_fuse_default(&lexical, &dense);
    println!("fused top-10 (RRF k={}):", hybrid::DEFAULT_RRF_K);
    let mut target_rank = None;
    for (rank, (id, score, parts)) in fused.iter().take(10).enumerate() {
        let chunk = chunks.resolve_embedding_id(id).ok().flatten();
        let loc = chunk
            .as_ref()
            .map(|c| format!("{}:{}", c.repo, c.path))
            .unwrap_or_else(|| id.clone());
        println!(
            "{:>2}. rrf={:.4} lex_rank={:?} dense_rank={:?}  {loc}",
            rank + 1,
            score,
            parts.lexical_rank,
            parts.dense_rank
        );
        if let Some(c) = &chunk
            && c.path.ends_with(TARGET_PATH_SUFFIX)
            && c.text.to_lowercase().contains(LEXICAL_ANCHOR)
        {
            target_rank = Some(rank + 1);
        }
    }
    let Some(rank) = target_rank else {
        panic!(
            "real backoff/retry chunk (*{TARGET_PATH_SUFFIX} containing {LEXICAL_ANCHOR}) \
             not in fused top-10; lexical {:?}",
            lexical
                .iter()
                .take(5)
                .map(|(i, _)| i.as_str())
                .collect::<Vec<_>>()
        )
    };
    println!("PASS: real *{TARGET_PATH_SUFFIX} backoff/retry chunk fused at rank {rank}");
}
