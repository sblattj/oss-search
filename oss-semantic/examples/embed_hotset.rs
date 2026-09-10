//! Embed the REAL hot-set corpus's rust+python+javascript sources into the
//! semantic layer (data/corpus/semantic/), then run a live query.
//!
//! Pipeline: walk `data/corpus/work/<repo>/**` for source files, chunk with
//! the tree-sitter chunker (window fallback for anything unparseable),
//! embed, and persist the vector store (`vectors.ossv`) plus SQLite chunk
//! metadata (`chunks.db`). Chunk paths are prefixed with the repo name so
//! chunk ids stay unique across repos (the flat store replaces duplicate
//! ids; 100 repos WILL contain colliding `src/lib.rs` paths otherwise).
//!
//! Embedder selection:
//! * built with `--features fastembed` (default when available): real
//!   local ONNX inference via `FastEmbedder::default_model()`
//!   (BGESmallENV15, 384-dim). The model downloads from the HF hub on
//!   FIRST RUN (~100 MB) and is cached; force it off with OSS_EMBEDDER=stub.
//! * otherwise: deterministic `StubEmbedder` (token-hasher, semantically
//!   meaningless) so the pipeline mechanics still run — real-embedding
//!   quality requires the fastembed feature.
//!
//! Finishes with a live natural-language query through the dense store
//! (the fastembed-backed query when the feature is on) and prints routing
//! decisions from `classify_query`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use oss_semantic::chunk_store::ChunkStore;
use oss_semantic::chunking::{by_extension, chunk_source};
use oss_semantic::embedding::{Embedder, StubEmbedder};
use oss_semantic::hybrid::classify_query;
use oss_semantic::vector_store::{
    FlatVectorStore, MetadataFilter, SearchHit, VectorMeta, VectorStore,
};

const BATCH: usize = 256;
const LANG_EXTS: &[&str] = &["rs", "py", "pyi", "js", "mjs", "cjs", "jsx"];
const META_JSON: &str = "meta.json";
const VECTORS_FILE: &str = "vectors.ossv";
const CHUNKS_FILE: &str = "chunks.db";

fn mib(bytes: u64) -> String {
    format!("{:.2} MiB", bytes as f64 / 1_048_576.0)
}

fn boxed_embedder() -> (String, Box<dyn Embedder>) {
    let forced_stub = std::env::var("OSS_EMBEDDER").as_deref() == Ok("stub");
    #[cfg(feature = "fastembed")]
    if !forced_stub {
        match oss_semantic::fast_embedder::FastEmbedder::default_model() {
            Ok(e) => {
                let dim = e.dim();
                println!("embedder: fastembed BGESmallENV15 (dim {dim}, local ONNX CPU)");
                return ("fastembed".to_string(), Box::new(e));
            }
            Err(e) => {
                eprintln!("fastembed init failed ({e}); falling back to StubEmbedder");
            }
        }
    }
    #[cfg(not(feature = "fastembed"))]
    if forced_stub {
        println!("OSS_EMBEDDER=stub requested");
    }
    println!(
        "embedder: StubEmbedder (dim 256) — token-hasher, NOT semantic; \
         rebuild with --features fastembed for real embeddings"
    );
    ("stub".to_string(), Box::new(StubEmbedder::new(256)))
}

/// All (repo, rel_path, source) source files under `work/`, sorted for
/// deterministic chunk ids.
fn collect_sources(work_root: &Path) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    let mut repo_dirs: Vec<PathBuf> = std::fs::read_dir(work_root)
        .unwrap_or_else(|e| panic!("read {}: {e}", work_root.display()))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    repo_dirs.sort();
    for repo_dir in repo_dirs {
        let repo = repo_dir.file_name().unwrap().to_string_lossy().to_string();
        let mut files = Vec::new();
        let mut stack = vec![repo_dir.clone()];
        while let Some(dir) = stack.pop() {
            let entries = match std::fs::read_dir(&dir) {
                Ok(e) => e,
                Err(_) => continue,
            };
            for entry in entries.filter_map(|e| e.ok()) {
                let p = entry.path();
                if p.is_dir() {
                    let name = p
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    if name == ".git" || name == "node_modules" {
                        continue;
                    }
                    stack.push(p);
                } else if p.is_file() {
                    let ext = p
                        .extension()
                        .map(|e| e.to_string_lossy().to_string())
                        .unwrap_or_default();
                    if LANG_EXTS.contains(&ext.as_str()) {
                        files.push(p);
                    }
                }
            }
        }
        files.sort();
        for f in files {
            let rel = f.strip_prefix(&repo_dir).unwrap();
            let path = rel.to_string_lossy().replace('\\', "/");
            if let Ok(source) = std::fs::read_to_string(&f) {
                out.push((repo.clone(), path, source));
            }
        }
    }
    out
}

fn main() {
    let t0 = Instant::now();
    let root: PathBuf = std::env::var("OSS_CORPUS_ROOT")
        .unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../data/corpus").to_string())
        .into();
    let work_root = root.join("work");
    let sem_dir = root.join("semantic");
    std::fs::create_dir_all(&sem_dir).expect("create semantic dir");
    println!("corpus root: {}", root.display());

    // ---- chunk (tree-sitter) ----
    let t_chunk = Instant::now();
    let sources = collect_sources(&work_root);
    let mut chunks = Vec::new();
    let mut per_lang_files: BTreeMap<String, u64> = BTreeMap::new();
    for (repo, path, source) in &sources {
        let ext = path.rsplit('.').next().unwrap_or("");
        let lang = by_extension(ext)
            .map(|s| s.name.to_string())
            .unwrap_or_else(|| "window".into());
        *per_lang_files.entry(lang.clone()).or_default() += 1;
        // repo-prefixed path keeps chunk ids unique across the 100 repos.
        let prefixed = format!("{repo}/{path}");
        for c in chunk_source(source, &lang, &prefixed) {
            chunks.push((repo.clone(), lang.clone(), c));
        }
    }
    let chunk_secs = t_chunk.elapsed().as_secs_f64();
    println!(
        "chunked {} source files -> {} chunks in {chunk_secs:.1}s ({:?})",
        sources.len(),
        chunks.len(),
        per_lang_files
    );

    // ---- embed ----
    let (embedder_name, embedder) = boxed_embedder();
    let t_embed = Instant::now();
    let mut vectors = FlatVectorStore::new();
    let mut batches = 0u64;
    for batch in chunks.chunks(BATCH) {
        let texts: Vec<String> = batch.iter().map(|(_, _, c)| c.text.clone()).collect();
        let embeddings = embedder.embed(&texts).expect("embed batch");
        assert_eq!(embeddings.len(), batch.len());
        for ((repo, lang, c), vector) in batch.iter().zip(&embeddings) {
            vectors
                .add(
                    c.id.clone(),
                    vector.clone(),
                    VectorMeta {
                        repo: repo.clone(),
                        path: c.path.clone(),
                        kind: c.kind.as_str().to_string(),
                        lang: lang.clone(),
                        start_line: c.start_line,
                        end_line: c.end_line,
                    },
                )
                .expect("add vector");
        }
        batches += 1;
        if batches.is_multiple_of(20) {
            println!(
                "  embedded {}/{} chunks ({:.1}s)...",
                batches as usize * BATCH,
                chunks.len(),
                t_embed.elapsed().as_secs_f64()
            );
        }
    }
    let embed_secs = t_embed.elapsed().as_secs_f64();
    println!(
        "embedded {} chunks in {} batches ({embed_secs:.1}s), dim {:?}",
        vectors.len(),
        batches,
        vectors.dim()
    );

    // ---- persist ----
    let t_store = Instant::now();
    vectors
        .save(sem_dir.join(VECTORS_FILE))
        .expect("save vectors");
    {
        let mut store = ChunkStore::open(sem_dir.join(CHUNKS_FILE)).expect("open chunk store");
        // add_chunks is per-(repo, lang); group to keep each call uniform.
        let mut groups: BTreeMap<(String, String), Vec<oss_semantic::Chunk>> = BTreeMap::new();
        for (repo, lang, c) in &chunks {
            groups
                .entry((repo.clone(), lang.clone()))
                .or_default()
                .push(c.clone());
        }
        let mut total = 0usize;
        for ((repo, lang), items) in &groups {
            let ids: Vec<String> = items.iter().map(|c| c.id.clone()).collect();
            total += store
                .add_chunks(repo, lang, items.iter().cloned(), Some(&ids))
                .expect("add chunks");
        }
        println!("chunk store rows: {total}");
    }
    let store_secs = t_store.elapsed().as_secs_f64();
    let on_disk: u64 = [META_JSON, VECTORS_FILE, CHUNKS_FILE]
        .iter()
        .map(|f| sem_dir.join(f).metadata().map(|m| m.len()).unwrap_or(0))
        .sum();
    let meta = serde_json::json!({
        "embedder": embedder_name,
        "dim": vectors.dim(),
        "files": sources.len(),
        "chunks": vectors.len(),
        "batch_size": BATCH,
        "wall_secs": {
            "chunk": chunk_secs,
            "embed": embed_secs,
            "store": store_secs,
            "total": t0.elapsed().as_secs_f64(),
        },
        "on_disk_bytes": on_disk,
    });
    std::fs::write(
        sem_dir.join(META_JSON),
        serde_json::to_vec_pretty(&meta).unwrap(),
    )
    .expect("write meta.json");
    println!(
        "stored: {} + {} + {} = {} ({}) in {store_secs:.1}s -> {}",
        VECTORS_FILE,
        CHUNKS_FILE,
        META_JSON,
        on_disk,
        mib(on_disk),
        sem_dir.display()
    );

    // ---- live query through routing ----
    let identifier_q = "deserialize_struct";
    let nl_q = "retry a failed request with exponential backoff";
    for q in [identifier_q, nl_q] {
        println!("\nrouting: {q:?} -> {:?}", classify_query(q));
    }
    println!("\nlive dense top-10 for {nl_q:?} (embedder: {embedder_name}):");
    let t_q = Instant::now();
    let qv = &embedder.embed(&[nl_q.to_string()]).expect("embed query")[0];
    let hits: Vec<SearchHit> = vectors.search(qv, 10, &MetadataFilter::none());
    let q_ms = t_q.elapsed().as_secs_f64() * 1000.0;
    let store = ChunkStore::open(sem_dir.join(CHUNKS_FILE)).expect("reopen chunk store");
    for (i, h) in hits.iter().enumerate() {
        let loc = store
            .resolve_embedding_id(&h.id)
            .ok()
            .flatten()
            .map(|c| {
                format!(
                    "{}:{} [{}] {}",
                    c.repo,
                    c.path,
                    c.kind,
                    c.text.lines().next().unwrap_or("").trim()
                )
            })
            .unwrap_or_else(|| h.meta.location());
        println!("{:>2}. {:.4}  {}", i + 1, h.score, loc);
    }
    println!("query wall: {q_ms:.1}ms");
}
