//! Build the trigram index over the REAL hot-set corpus (data/corpus/work)
//! and save it to data/corpus/index.
//!
//! Walks every materialized work tree, admission-gates each file through
//! `oss_index::admitted`, builds with `TrigramIndex::build_parallel`, and
//! prints build wall time, doc count, and on-disk index size.
//!
//! Corpus layout (see data/corpus/REPORT.md): `repos/` holds the blobless
//! git clones, `work/<repo>/` holds the materialized working trees — the
//! work trees are what gets indexed. Repo names come from the work-tree
//! directory name; paths are repo-relative.
//!
//! Override the corpus root with OSS_CORPUS_ROOT (defaults to
//! `<crate>/../data/corpus`), matching oss-corpus/examples/build_hotset.rs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use oss_index::{admitted, TrigramIndex};

fn mib(bytes: u64) -> String {
    format!("{:.2} MiB", bytes as f64 / 1_048_576.0)
}

/// One admissible corpus file ready for the index builder.
type CorpusDoc = (String, String, Vec<u8>);

/// (repo, path, bytes) for every admissible file under `work/`, sorted for
/// deterministic doc ids. Files failing the admission gate are counted and
/// reported, not indexed.
fn collect_docs(work_root: &Path) -> (Vec<CorpusDoc>, BTreeMap<String, u64>) {
    let mut rejections: BTreeMap<String, u64> = BTreeMap::new();
    let mut docs = Vec::new();
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
                let name = p
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                if p.is_dir() {
                    if name == ".git" || name == "node_modules" {
                        continue;
                    }
                    stack.push(p);
                } else if p.is_file() && name != ".DS_Store" {
                    files.push(p);
                }
            }
        }
        files.sort();
        for f in files {
            let rel = f.strip_prefix(&repo_dir).unwrap();
            let path = rel.to_string_lossy().replace('\\', "/");
            let bytes = match std::fs::read(&f) {
                Ok(b) => b,
                Err(_) => {
                    *rejections.entry("unreadable".into()).or_default() += 1;
                    continue;
                }
            };
            match admitted(&path, &bytes) {
                Ok(()) => docs.push((repo.clone(), path, bytes)),
                Err(reason) => {
                    let key = if reason.contains("too large") {
                        "too_large"
                    } else if reason.contains("binary") {
                        "binary_nul"
                    } else {
                        "other"
                    };
                    *rejections.entry(key.to_string()).or_default() += 1;
                }
            }
        }
    }
    (docs, rejections)
}

fn dir_size(dir: &Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.filter_map(|e| e.ok()) {
            total += e.metadata().map(|m| m.len()).unwrap_or(0);
        }
    }
    total
}

fn main() {
    let t0 = Instant::now();
    let root: PathBuf = std::env::var("OSS_CORPUS_ROOT")
        .unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../data/corpus").to_string())
        .into();
    let work_root = root.join("work");
    let index_dir = root.join("index");
    println!("corpus root: {}", root.display());
    println!("work trees : {}", work_root.display());

    let t_walk = Instant::now();
    let (docs, rejections) = collect_docs(&work_root);
    let walk_secs = t_walk.elapsed().as_secs_f64();
    let bytes: u64 = docs.iter().map(|(_, _, b)| b.len() as u64).sum();
    let repos = docs
        .iter()
        .map(|(r, _, _)| r.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    println!(
        "walk: {} files from {} repos ({}), gated out: {:?} — {:.1}s",
        docs.len(),
        repos,
        mib(bytes),
        rejections,
        walk_secs
    );

    let t_build = Instant::now();
    let idx = TrigramIndex::build_parallel(docs).expect("build_parallel");
    let build_secs = t_build.elapsed().as_secs_f64();
    assert!(
        idx.doc_count() > 10_000,
        "expected the full hot-set, got {}",
        idx.doc_count()
    );

    let t_save = Instant::now();
    idx.save(&index_dir).expect("save index");
    let save_secs = t_save.elapsed().as_secs_f64();

    let on_disk = dir_size(&index_dir);
    println!("docs indexed : {}", idx.doc_count());
    println!("build wall   : {build_secs:.1}s (parallel extraction, release semantics)");
    println!("save wall    : {save_secs:.1}s -> {}", index_dir.display());
    println!("index on disk: {} ({})", on_disk, mib(on_disk));
    println!("total wall   : {:.1}s", t0.elapsed().as_secs_f64());
}
