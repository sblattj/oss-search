//! Integration tests for oss-semantic. No network, no model downloads —
//! all embedding goes through the deterministic `StubEmbedder`.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use oss_semantic::chunk_store::StoredChunk;
use oss_semantic::chunking::{by_extension, by_name, chunk_by_extension, chunk_source, window};
use oss_semantic::embedding::StubEmbedder;
use oss_semantic::hybrid::{self, FusedComponents, QueryKind};
use oss_semantic::vector_store::{FlatVectorStore, MetadataFilter, VectorMeta, VectorStore};
use oss_semantic::{ChunkKind, ChunkStore, SemanticPipeline};

fn fixture(name: &str) -> String {
    let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "tests", "fixtures", name]
        .iter()
        .collect();
    std::fs::read_to_string(path).unwrap()
}

fn temp_path(suffix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir()
        .join(format!(
            "oss-semantic-test-{}-{}",
            std::process::id(),
            nanos
        ))
        .with_extension(suffix)
}

fn span_of<'a>(chunks: &'a [oss_semantic::Chunk], symbol: &str) -> &'a oss_semantic::Chunk {
    chunks
        .iter()
        .find(|c| c.symbol.as_deref() == Some(symbol))
        .unwrap_or_else(|| panic!("no chunk for symbol {symbol}: {chunks:#?}"))
}

// ---------------------------------------------------------------- chunking

#[test]
fn rust_three_functions_exact_spans() {
    let chunks = chunk_source(&fixture("three_fns.rs"), "rust", "three_fns.rs");
    assert_eq!(chunks.len(), 3, "expected exactly 3 chunks: {chunks:#?}");

    let fib = span_of(&chunks, "fibonacci");
    // Doc comment (line 3) + attribute (line 4) + fn (lines 5-10).
    assert_eq!((fib.start_line, fib.end_line), (3, 10));
    assert_eq!(fib.kind, ChunkKind::Function);
    assert!(fib.text.starts_with("/// Fast identity-ish doubling."));
    assert!(fib.text.contains("pub fn fibonacci(n: u64) -> u64"));

    let grow = span_of(&chunks, "grow");
    assert_eq!((grow.start_line, grow.end_line), (12, 15));
    assert!(grow.text.starts_with("/// Slow growth."));

    let shrink = span_of(&chunks, "shrink");
    assert_eq!((shrink.start_line, shrink.end_line), (17, 19));
    assert!(!shrink.text.contains("///"), "no doc attached to shrink");
}

#[test]
fn rust_module_comment_not_attached_across_blank_line() {
    let chunks = chunk_source(&fixture("three_fns.rs"), "rust", "three_fns.rs");
    let fib = span_of(&chunks, "fibonacci");
    assert_eq!(
        fib.start_line, 3,
        "line 1 comment is separated by a blank line"
    );
}

#[test]
fn rust_kinds_struct_enum_trait_impl() {
    let chunks = chunk_source(&fixture("items.rs"), "rust", "items.rs");
    assert_eq!(chunks.len(), 4);

    let s = chunks.iter().find(|c| c.kind == ChunkKind::Struct).unwrap();
    assert_eq!(s.symbol.as_deref(), Some("Config"));
    assert_eq!((s.start_line, s.end_line), (1, 3));

    let e = chunks.iter().find(|c| c.kind == ChunkKind::Enum).unwrap();
    assert_eq!(e.symbol.as_deref(), Some("Mode"));
    assert_eq!((e.start_line, e.end_line), (5, 8));

    let t = chunks.iter().find(|c| c.kind == ChunkKind::Trait).unwrap();
    assert_eq!(t.symbol.as_deref(), Some("Runner"));
    assert_eq!((t.start_line, t.end_line), (10, 12));

    // impl symbol falls back to the implemented type: Config.
    let i = chunks.iter().find(|c| c.kind == ChunkKind::Impl).unwrap();
    assert_eq!(i.symbol.as_deref(), Some("Config"));
    assert_eq!((i.start_line, i.end_line), (14, 18));
    assert!(i.text.contains("impl Runner for Config"));
}

#[test]
fn python_functions_and_class_with_docstrings() {
    let chunks = chunk_source(&fixture("sample.py"), "python", "sample.py");
    assert_eq!(chunks.len(), 2);

    let add = span_of(&chunks, "add");
    assert_eq!(add.kind, ChunkKind::Function);
    assert_eq!((add.start_line, add.end_line), (4, 6));
    assert!(
        add.text.contains("\"\"\"Add two integers.\"\"\""),
        "body docstring included"
    );

    let greeter = span_of(&chunks, "Greeter");
    assert_eq!(greeter.kind, ChunkKind::Class);
    assert_eq!((greeter.start_line, greeter.end_line), (9, 13));
    assert!(
        greeter.text.contains("def hello"),
        "method inside class chunk"
    );
}

#[test]
fn python_broken_code_still_chunks_surrounding_items() {
    // GLR error recovery: the malformed `def broken(a:` must not prevent
    // chunks for the surrounding valid functions.
    let chunks = chunk_source(&fixture("broken.py"), "python", "broken.py");
    let add = span_of(&chunks, "add");
    assert_eq!((add.start_line, add.end_line), (1, 2));
    let sub = span_of(&chunks, "sub");
    assert_eq!((sub.start_line, sub.end_line), (7, 8));
}

#[test]
fn rust_broken_code_still_chunks_valid_prefix() {
    let chunks = chunk_source(&fixture("broken.rs"), "rust", "broken.rs");
    assert!(!chunks.is_empty(), "must yield chunks, got none");
    let ok = span_of(&chunks, "ok");
    assert_eq!((ok.start_line, ok.end_line), (1, 3));
}

#[test]
fn javascript_export_wrappers_and_class() {
    let chunks = chunk_source(&fixture("sample.js"), "javascript", "sample.js");
    assert_eq!(
        chunks.len(),
        2,
        "export fn + class; const arrow fn not chunked"
    );

    let add = span_of(&chunks, "add");
    assert_eq!(add.kind, ChunkKind::Function);
    assert_eq!(
        (add.start_line, add.end_line),
        (3, 5),
        "peeks inside export_statement"
    );

    let calc = span_of(&chunks, "Calculator");
    assert_eq!(calc.kind, ChunkKind::Class);
    assert_eq!((calc.start_line, calc.end_line), (9, 14));
}

#[test]
fn typescript_interface_with_jsdoc_and_function() {
    let chunks = chunk_source(&fixture("sample.ts"), "typescript", "sample.ts");
    assert_eq!(chunks.len(), 2);

    let point = span_of(&chunks, "Point");
    assert_eq!(point.kind, ChunkKind::Interface);
    assert_eq!((point.start_line, point.end_line), (1, 5), "jsdoc attached");
    assert!(point.text.starts_with("/** Shape of a point. */"));

    let norm = span_of(&chunks, "norm");
    assert_eq!(norm.kind, ChunkKind::Function);
    assert_eq!((norm.start_line, norm.end_line), (7, 9));
}

#[test]
fn go_types_functions_methods() {
    let chunks = chunk_source(&fixture("sample.go"), "go", "sample.go");
    assert_eq!(chunks.len(), 3);

    let point = span_of(&chunks, "Point");
    assert_eq!(point.kind, ChunkKind::Type);
    assert_eq!((point.start_line, point.end_line), (3, 6));

    let dist = span_of(&chunks, "Dist");
    assert_eq!(dist.kind, ChunkKind::Function);
    assert_eq!((dist.start_line, dist.end_line), (8, 11));

    let scale = span_of(&chunks, "Scale");
    assert_eq!(scale.kind, ChunkKind::Function);
    assert_eq!((scale.start_line, scale.end_line), (13, 15));
}

#[test]
fn c_struct_declaration_and_function() {
    let chunks = chunk_source(&fixture("sample.c"), "c", "sample.c");
    assert_eq!(chunks.len(), 2);

    let counter = chunks
        .iter()
        .find(|c| c.kind == ChunkKind::Struct)
        .expect("struct chunk via declaration peek");
    assert_eq!((counter.start_line, counter.end_line), (3, 6));
    assert!(counter.text.starts_with("/* A counter. */"));

    let bump = chunks
        .iter()
        .find(|c| c.text.contains("int bump"))
        .expect("function chunk");
    assert_eq!(bump.kind, ChunkKind::Function);
    assert_eq!((bump.start_line, bump.end_line), (8, 11));
}

// ------------------------------------------------------- window fallback

#[test]
fn fallback_window_spans_overlap_10() {
    let lines: Vec<String> = (1..=135).map(|i| format!("line {i:03}")).collect();
    let source = lines.join("\n");
    let chunks = chunk_source(&source, "totally-unknown-lang", "blob.bin");
    assert!(!chunks.is_empty());
    assert!(chunks.iter().all(|c| c.kind == ChunkKind::Window));

    // 135 lines, 60-line window, 50-line step => (1,60), (51,110), (101,135).
    let spans: Vec<(usize, usize)> = chunks.iter().map(|c| (c.start_line, c.end_line)).collect();
    assert_eq!(spans, vec![(1, 60), (51, 110), (101, 135)]);

    // Overlap is exactly 10 lines and content matches the source lines.
    let second = &chunks[1];
    assert_eq!(second.text.lines().next().unwrap(), "line 051");
    assert_eq!(second.text.lines().last().unwrap(), "line 110");
}

#[test]
fn fallback_window_short_file_is_one_chunk() {
    let chunks = chunk_source("only a few\nlines here", "ini", "x.ini");
    assert_eq!(chunks.len(), 1);
    assert_eq!((chunks[0].start_line, chunks[0].end_line), (1, 2));
    assert_eq!(chunks[0].kind, ChunkKind::Window);
}

#[test]
fn fallback_window_exact_boundary() {
    let src60 = (1..=60)
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(chunk_source(&src60, "txt", "a.txt").len(), 1);
    let src61 = (1..=61)
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    let spans: Vec<(usize, usize)> = chunk_source(&src61, "txt", "a.txt")
        .iter()
        .map(|c| (c.start_line, c.end_line))
        .collect();
    assert_eq!(spans, vec![(1, 60), (51, 61)]);
}

#[test]
fn parse_with_no_items_falls_back_to_window() {
    // Valid rust grammar, but no function/class items at all: the cascade
    // still yields window chunks so the file stays searchable.
    let chunks = chunk_source(&fixture("imports_only.rs"), "rust", "imports_only.rs");
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].kind, ChunkKind::Window);
    assert_eq!((chunks[0].start_line, chunks[0].end_line), (1, 4));
}

#[test]
fn window_module_constants_match_behavior() {
    assert_eq!(window::WINDOW_LINES, 60);
    assert_eq!(window::WINDOW_OVERLAP, 10);
}

// ---------------------------------------------------------------- registry

#[test]
fn registry_lookup_by_extension() {
    assert_eq!(by_extension("rs").unwrap().name, "rust");
    assert_eq!(by_extension(".py").unwrap().name, "python");
    assert_eq!(by_extension("pyi").unwrap().name, "python");
    assert_eq!(by_extension("js").unwrap().name, "javascript");
    assert_eq!(by_extension("ts").unwrap().name, "typescript");
    assert_eq!(by_extension("tsx").unwrap().name, "tsx");
    assert_eq!(by_extension("go").unwrap().name, "go");
    assert_eq!(by_extension("c").unwrap().name, "c");
    assert_eq!(by_extension("h").unwrap().name, "c");
    assert!(by_extension("zig").is_none());
    assert!(by_extension("").is_none());
}

#[test]
fn registry_lookup_by_name_and_parser() {
    for name in [
        "rust",
        "python",
        "javascript",
        "typescript",
        "tsx",
        "go",
        "c",
    ] {
        let support = by_name(name).unwrap_or_else(|| panic!("missing {name}"));
        let parser = support.parser().expect("grammar loads");
        drop(parser);
    }
    assert!(by_name("kotlin").is_none());
    assert!(by_name("Rust").is_none(), "names are canonical lowercase");
}

#[test]
fn chunk_by_extension_dispatches() {
    let chunks = chunk_by_extension("fn a() {}", "rs", "a.rs");
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].kind, ChunkKind::Function);

    let win = chunk_by_extension("hello\nworld", "dat", "a.dat");
    assert_eq!(win.len(), 1);
    assert_eq!(win[0].kind, ChunkKind::Window);
}

// ----------------------------------------------------------- vector store

fn meta(repo: &str, path: &str, kind: &str) -> VectorMeta {
    VectorMeta {
        repo: repo.into(),
        path: path.into(),
        kind: kind.into(),
        lang: "rust".into(),
        start_line: 1,
        end_line: 2,
    }
}

#[test]
fn vector_store_cosine_top_k() {
    let mut store = FlatVectorStore::new();
    store
        .add(
            "a".into(),
            vec![1.0, 0.0, 0.0, 0.0],
            meta("r", "a.rs", "function"),
        )
        .unwrap();
    store
        .add(
            "b".into(),
            vec![0.0, 1.0, 0.0, 0.0],
            meta("r", "b.rs", "function"),
        )
        .unwrap();
    store
        .add(
            "c".into(),
            vec![1.0, 0.0, 0.0, 1.0],
            meta("r", "c.rs", "function"),
        )
        .unwrap();

    let hits = store.search(&[1.0, 0.0, 0.0, 0.0], 3, &MetadataFilter::none());
    assert_eq!(hits.len(), 3);
    assert_eq!(hits[0].id, "a");
    assert!(
        (hits[0].score - 1.0).abs() < 1e-6,
        "parallel vector scores 1.0"
    );
    let inv_sqrt2 = std::f32::consts::FRAC_1_SQRT_2;
    assert!(
        (hits[1].score - inv_sqrt2).abs() < 1e-3,
        "c = cos 45deg, got {}",
        hits[1].score
    );
    assert_eq!(hits[1].id, "c");
    assert_eq!(hits[2].id, "b");
    assert!(hits[2].score.abs() < 1e-6, "orthogonal scores 0");

    let top1 = store.search(&[0.0, 1.0, 0.0, 0.0], 1, &MetadataFilter::none());
    assert_eq!(top1.len(), 1);
    assert_eq!(top1[0].id, "b");
}

#[test]
fn vector_store_pre_filters_before_scoring() {
    let mut store = FlatVectorStore::new();
    store
        .add(
            "a1".into(),
            vec![1.0, 0.0],
            meta("repoA", "src/lib.rs", "function"),
        )
        .unwrap();
    store
        .add(
            "a2".into(),
            vec![0.9, 0.1],
            meta("repoA", "tests/t.rs", "function"),
        )
        .unwrap();
    store
        .add(
            "b1".into(),
            vec![1.0, 0.1],
            meta("repoB", "src/lib.rs", "function"),
        )
        .unwrap();
    store
        .add(
            "a3".into(),
            vec![1.0, 0.0],
            meta("repoA", "srcx/edge.rs", "class"),
        )
        .unwrap();

    // repo filter: only repoA rows can appear even though b1 is similar.
    let hits = store.search(&[1.0, 0.0], 10, &MetadataFilter::repo("repoA"));
    assert_eq!(hits.len(), 3);
    assert!(hits.iter().all(|h| h.meta.repo == "repoA"));

    // path prefix is component-wise: "src" matches src/lib.rs but NOT
    // srcx/edge.rs.
    let hits = store.search(
        &[1.0, 0.0],
        10,
        &MetadataFilter {
            repo: Some("repoA".into()),
            path_prefix: Some("src".into()),
            ..MetadataFilter::none()
        },
    );
    assert_eq!(
        hits.iter().map(|h| h.id.clone()).collect::<Vec<_>>(),
        ["a1"]
    );

    // kind filter.
    let hits = store.search(
        &[1.0, 0.0],
        10,
        &MetadataFilter {
            kind: Some("class".into()),
            ..MetadataFilter::none()
        },
    );
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, "a3");
}

#[test]
fn vector_store_persistence_roundtrip() {
    let path = temp_path("ossv");
    let mut store = FlatVectorStore::new();
    store
        .add(
            "a".into(),
            vec![0.1, 0.2, 0.3],
            meta("r", "a.rs", "function"),
        )
        .unwrap();
    store
        .add("b".into(), vec![0.4, 0.5, 0.6], meta("r", "b.rs", "class"))
        .unwrap();
    store.save(&path).unwrap();

    let loaded = FlatVectorStore::load(&path).unwrap();
    assert_eq!(loaded.dim(), Some(3));
    assert_eq!(loaded.len(), 2);
    let before = store.search(&[0.1, 0.2, 0.3], 2, &MetadataFilter::none());
    let after = loaded.search(&[0.1, 0.2, 0.3], 2, &MetadataFilter::none());
    assert_eq!(before, after);
    assert_eq!(after[0].id, "a");

    let garbage = temp_path("ossv");
    std::fs::write(&garbage, b"not a store at all").unwrap();
    assert!(FlatVectorStore::load(&garbage).is_err());
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&garbage);
}

#[test]
fn vector_store_dim_mismatch_and_id_replace() {
    let mut store = FlatVectorStore::new();
    store
        .add("a".into(), vec![1.0, 0.0], meta("r", "a.rs", "function"))
        .unwrap();
    assert!(
        store
            .add(
                "x".into(),
                vec![1.0, 0.0, 0.0],
                meta("r", "x.rs", "function")
            )
            .is_err()
    );

    // Re-adding the same id replaces instead of duplicating.
    store
        .add("a".into(), vec![0.0, 1.0], meta("r2", "a2.rs", "class"))
        .unwrap();
    assert_eq!(store.len(), 1);
    let hits = store.search(&[0.0, 1.0], 5, &MetadataFilter::none());
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].meta.repo, "r2");
    assert_eq!(hits[0].meta.kind, "class");
}

#[test]
fn vector_store_zero_norms_score_zero() {
    let mut store = FlatVectorStore::new();
    store
        .add("zero".into(), vec![0.0, 0.0], meta("r", "z.rs", "function"))
        .unwrap();
    let hits = store.search(&[0.0, 0.0], 5, &MetadataFilter::none());
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].score, 0.0, "zero vectors must not produce NaN");
    assert!(store.search(&[], 5, &MetadataFilter::none()).is_empty());
}

// ------------------------------------------------------------ chunk store

fn sample_chunks() -> Vec<oss_semantic::Chunk> {
    chunk_source(&fixture("three_fns.rs"), "rust", "src/three_fns.rs")
}

#[test]
fn chunk_store_roundtrip_across_connections() {
    let path = temp_path("db");
    {
        let mut store = ChunkStore::open(&path).unwrap();
        let n = store
            .add_chunks("acme/lib", "rust", sample_chunks(), None)
            .unwrap();
        assert_eq!(n, 3);
        assert_eq!(store.count().unwrap(), 3);
    }
    // Reopen from disk: schema + rows survive.
    let store = ChunkStore::open(&path).unwrap();
    assert_eq!(store.count().unwrap(), 3);
    let chunks = store.by_path("acme/lib", "src/three_fns.rs").unwrap();
    assert_eq!(chunks.len(), 3);
    let first = &chunks[0];
    assert_eq!(first.repo, "acme/lib");
    assert_eq!(first.lang, "rust");
    assert_eq!(first.kind, "function");
    assert_eq!((first.start_line, first.end_line), (3, 10));
    assert!(first.text.contains("fibonacci"));
    assert_eq!(
        first.embedding_id, first.id,
        "embedding id defaults to chunk id"
    );

    let got = store.get(&first.id).unwrap().unwrap();
    assert_eq!(got, *first);
    assert!(store.get("no-such-id").unwrap().is_none());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn chunk_store_upsert_replaces() {
    let mut store = ChunkStore::open_in_memory().unwrap();
    let mut chunks = sample_chunks();
    store.add_chunks("r", "rust", chunks.clone(), None).unwrap();
    chunks[0].text = "REPLACED".into();
    store.add_chunks("r", "rust", chunks, None).unwrap();
    assert_eq!(store.count().unwrap(), 3, "re-insert must not duplicate");
    let got = store.by_path("r", "src/three_fns.rs").unwrap();
    assert!(got.iter().any(|c| c.text == "REPLACED"));
}

#[test]
fn chunk_store_resolves_embedding_ids() {
    let mut store = ChunkStore::open_in_memory().unwrap();
    let chunks = sample_chunks();
    let custom_ids = vec![
        "emb-fib".to_string(),
        "emb-grow".to_string(),
        "emb-shrink".to_string(),
    ];
    store
        .add_chunks("r", "rust", chunks, Some(&custom_ids))
        .unwrap();
    let resolved: StoredChunk = store.resolve_embedding_id("emb-fib").unwrap().unwrap();
    assert!(resolved.text.contains("fibonacci"));
    assert!(store.resolve_embedding_id("emb-missing").unwrap().is_none());
}

// ----------------------------------------------------------------- hybrid

#[test]
fn rrf_hand_computed_example() {
    // k = 3.
    //   a: lex rank 1 (1/4) + dense rank 3 (1/6) = 5/12
    //   b: lex rank 2 (1/5) + dense rank 1 (1/4) = 0.45
    //   c: lex rank 3 (1/6)              = 1/6
    //   d:               dense rank 2 (1/5)      = 0.2
    // Order: b, a, d, c.
    let lexical = vec![
        ("a".to_string(), 9.0),
        ("b".to_string(), 8.0),
        ("c".to_string(), 7.0),
    ];
    let dense = vec![
        ("b".to_string(), 0.9),
        ("d".to_string(), 0.8),
        ("a".to_string(), 0.7),
    ];
    let fused = hybrid::rrf_fuse(&lexical, &dense, 3);
    let ids: Vec<&str> = fused.iter().map(|(id, _, _)| id.as_str()).collect();
    assert_eq!(ids, ["b", "a", "d", "c"]);

    let (id, score, parts) = &fused[0];
    assert_eq!(id, "b");
    assert!((score - 0.45).abs() < 1e-9, "b = 1/5 + 1/4, got {score}");
    assert_eq!(
        parts,
        &FusedComponents {
            lexical_rank: Some(2),
            lexical_score: Some(8.0),
            dense_rank: Some(1),
            dense_score: Some(0.9),
            lexical_contribution: 1.0 / 5.0,
            dense_contribution: 1.0 / 4.0,
        }
    );
    let (_, a_score, a_parts) = &fused[1];
    assert_eq!(a_parts.dense_rank, Some(3));
    assert!(
        (a_score - 5.0 / 12.0).abs() < 1e-6,
        "a = 1/4 + 1/6, got {a_score}"
    );
    let d = &fused[2];
    assert!((d.1 - 0.2).abs() < 1e-9);
    assert_eq!(d.2.lexical_rank, None, "d absent from lexical list");
    assert_eq!(d.2.lexical_score, None);
    assert_eq!(d.2.lexical_contribution, 0.0);
    let c = &fused[3];
    assert_eq!(c.2.dense_rank, None);
    assert!((c.1 - 1.0 / 6.0).abs() < 1e-9);
}

#[test]
fn rrf_covers_union_and_breaks_ties_deterministically() {
    let lexical = vec![("x".to_string(), 1.0)];
    let dense = vec![("y".to_string(), 0.5)];
    let fused = hybrid::rrf_fuse(&lexical, &dense, 60);
    assert_eq!(fused.len(), 2, "union of both lists");
    // Equal contributions (both rank 1): tie broken by id ascending.
    assert_eq!(fused[0].0, "x");
    assert_eq!(fused[1].0, "y");
    assert!((fused[0].1 - 1.0 / 61.0).abs() < 1e-9);

    // Default k is 3 and matches rrf_fuse_default.
    let lex = vec![("x".to_string(), 1.0)];
    let den = vec![("y".to_string(), 1.0)];
    let by_param = hybrid::rrf_fuse(&lex, &den, hybrid::DEFAULT_RRF_K);
    let by_default = hybrid::rrf_fuse_default(&lex, &den);
    assert_eq!(by_param, by_default);
    assert_eq!(hybrid::DEFAULT_RRF_K, 3);
}

#[test]
fn rrf_empty_inputs() {
    let fused = hybrid::rrf_fuse(&[], &[], 3);
    assert!(fused.is_empty());
    let fused = hybrid::rrf_fuse(&[("a".into(), 1.0)], &[], 3);
    assert_eq!(fused.len(), 1);
    assert!((fused[0].1 - 0.25).abs() < 1e-9);
}

#[test]
fn classify_identifier_queries() {
    for q in [
        "parseJSON",
        "rate_limit_config",
        "Vec::with_capacity",
        "http.Client.Do",
        "SNake_MIXED_case",
        "where is parseJSON used",   // multiword but embeds a hump
        "find Foo::bar usages",      // qualified symbol in NL wrapper
        "what calls foo()",          // call shape
        "set the max_retries value", // snake_case token
    ] {
        assert_eq!(
            hybrid::classify_query(q),
            QueryKind::Identifier,
            "expected Identifier for {q:?}"
        );
    }
}

#[test]
fn classify_natural_language_queries() {
    for q in [
        "how do I parse JSON safely",
        "where is rate limiting enforced",
        "how does auth handle token refresh",
        "explain the retry decorator",
        "which function sorts the results",
        "",
        "   ",
    ] {
        assert_eq!(
            hybrid::classify_query(q),
            QueryKind::NaturalLanguage,
            "expected NaturalLanguage for {q:?}"
        );
    }
}

// --------------------------------------------------------------- pipeline

#[test]
fn pipeline_index_and_search_resolves_file_line() {
    let mut pipeline = SemanticPipeline::new(StubEmbedder::new(64), None).unwrap();

    let rust_src = fixture("three_fns.rs");
    let py_src = fixture("sample.py");
    assert_eq!(
        pipeline
            .index_source("acme/lib", "rust", "math.rs", &rust_src)
            .unwrap(),
        3
    );
    assert_eq!(
        pipeline
            .index_source("acme/lib", "python", "greeter.py", &py_src)
            .unwrap(),
        2
    );
    assert_eq!(pipeline.vectors().len(), 5);
    assert_eq!(pipeline.chunks().count().unwrap(), 5);

    // Query sharing tokens with the fibonacci chunk should rank it first
    // under the stub embedder (token-overlap similarity).
    let hits = pipeline
        .search("fibonacci n u64", 3, &MetadataFilter::none())
        .unwrap();
    assert_eq!(hits.len(), 3);
    let top = &hits[0];
    assert!(
        top.hit.id.starts_with("math.rs:3-"),
        "expected fibonacci chunk first, got {} ({})",
        top.hit.id,
        top.hit.score
    );
    assert_eq!(top.location(), "math.rs:3");
    assert!(top.chunk.as_ref().unwrap().text.contains("fibonacci"));

    // Metadata pre-filter propagates: python-only search never returns the
    // rust chunk.
    let py_hits = pipeline
        .search(
            "fibonacci n u64",
            10,
            &MetadataFilter {
                lang: Some("python".into()),
                ..MetadataFilter::none()
            },
        )
        .unwrap();
    assert!(py_hits.iter().all(|h| h.hit.meta.path == "greeter.py"));

    // Re-indexing the same file replaces rows instead of duplicating.
    pipeline
        .index_source("acme/lib", "rust", "math.rs", &rust_src)
        .unwrap();
    assert_eq!(pipeline.vectors().len(), 5);
    assert_eq!(pipeline.chunks().count().unwrap(), 5);
}

#[test]
fn pipeline_vector_store_persistence_via_pipeline() {
    let path = temp_path("ossv");
    let mut pipeline = SemanticPipeline::new(StubEmbedder::new(16), None).unwrap();
    pipeline
        .index_source("r", "rust", "a.rs", "fn one() { 1 }")
        .unwrap();
    pipeline.vectors().save(&path).unwrap();

    let loaded = FlatVectorStore::load(&path).unwrap();
    assert_eq!(loaded.len(), 1);
    let hit = loaded.search(&[1.0; 16], 1, &MetadataFilter::none());
    assert!(!hit.is_empty());
    let _ = std::fs::remove_file(&path);
}
