//! `oss-cli hotset build`: the staged, resumable local hot-set builder.
//!
//! Stages: clone (blobless depth-1 git clones via `oss_corpus::CloneCoordinator`)
//! -> ingest (CasStore + dedup + license via `oss_corpus::CorpusIngestor`)
//! -> index (trigram index via `oss_index::TrigramIndex`)
//! -> semantic (chunk + embed via `oss_semantic`, real vectors only when
//! built with the `semantic-fastembed` feature).
//!
//! A JSON state file under `<home>/hotset-state.json` records completed
//! stages plus per-repo status; a second run skips completed work. The
//! state file is only rewritten when its content actually changes, so an
//! unchanged re-run leaves its mtime alone.
//!
//! Progress goes to stderr; the final summary goes to stdout.

use clap::{Args, Subcommand};
use oss_corpus::{CasStore, CloneCoordinator, CorpusIngestor};
use oss_index::{admitted, TrigramIndex};
use oss_semantic::chunk_store::ChunkStore;
use oss_semantic::chunking::{by_extension, chunk_source};
use oss_semantic::embedding::{Embedder, StubEmbedder};
use oss_semantic::vector_store::{FlatVectorStore, VectorMeta, VectorStore};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Instant;
use walkdir::WalkDir;

const STATE_FILE: &str = "hotset-state.json";
const STATE_VERSION: u32 = 1;
const SEMANTIC_BATCH: usize = 256;
const LANG_EXTS: &[&str] = &["rs", "py", "pyi", "js", "mjs", "cjs", "jsx"];
const VECTORS_FILE: &str = "vectors.ossv";
const CHUNKS_FILE: &str = "chunks.db";
const SEM_META_FILE: &str = "meta.json";

/// Curated hot-set table (ecosystem, owner/name), best-first within each
/// ecosystem — the same list `oss-corpus/examples/build_hotset.rs` drives.
const CURATED: &[(&str, &str)] = &[
    // rust
    ("rust", "serde-rs/serde"), ("rust", "serde-rs/json"), ("rust", "dtolnay/anyhow"),
    ("rust", "dtolnay/thiserror"), ("rust", "rust-itertools/itertools"),
    ("rust", "servo/rust-smallvec"), ("rust", "Amanieu/parking_lot"),
    ("rust", "rayon-rs/rayon"), ("rust", "BurntSushi/globset"),
    ("rust", "rayon-rs/either"), ("rust", "crossbeam-rs/crossbeam"),
    ("rust", "rust-lang/hashbrown"), ("rust", "tokio-rs/bytes"),
    ("rust", "rust-lang-nursery/lazy-static.rs"), ("rust", "matklad/once_cell"),
    ("rust", "rust-lang/regex"), ("rust", "chronotope/chrono"),
    ("rust", "servo/rust-url"), ("rust", "hyperium/mime"), ("rust", "dtolnay/semver"),
    ("rust", "indexmap-rs/indexmap"), ("rust", "bitflags/bitflags"),
    ("rust", "rust-lang/log"), ("rust", "rust-cli/env_logger"),
    ("rust", "tokio-rs/tracing"), ("rust", "clap-rs/clap"), ("rust", "rust-lang/glob"),
    ("rust", "BurntSushi/walkdir"), ("rust", "Stebalien/tempfile"),
    ("rust", "dtolnay/ryu"), ("rust", "dtolnay/itoa"), ("rust", "dtolnay/syn"),
    // python
    ("python", "pallets/flask"), ("python", "psf/requests"), ("python", "pallets/click"),
    ("python", "Textualize/rich"), ("python", "python-attrs/attrs"),
    ("python", "encode/httpx"), ("python", "pallets/jinja"),
    ("python", "pallets/markupsafe"), ("python", "pallets/werkzeug"),
    ("python", "pallets/itsdangerous"), ("python", "theskumar/python-dotenv"),
    ("python", "marshmallow-code/marshmallow"), ("python", "coleifer/peewee"),
    ("python", "arrow-py/arrow"), ("python", "spulec/freezegun"),
    ("python", "mahmoud/boltons"), ("python", "pypa/pip"), ("python", "pypa/wheel"),
    ("python", "tqdm/tqdm"), ("python", "kjd/idna"), ("python", "chardet/chardet"),
    ("python", "urllib3/urllib3"), ("python", "dateutil/dateutil"),
    ("python", "yaml/pyyaml"), ("python", "certifi/python-certifi"),
    ("python", "pydantic/pydantic"),
    // javascript
    ("javascript", "lodash/lodash"), ("javascript", "chalk/chalk"),
    ("javascript", "tj/commander.js"), ("javascript", "minimistjs/minimist"),
    ("javascript", "colinhacks/zod"), ("javascript", "vercel/ms"),
    ("javascript", "debug-js/debug"), ("javascript", "axios/axios"),
    ("javascript", "left-pad/left-pad"), ("javascript", "isaacs/node-mkdirp"),
    ("javascript", "yargs/yargs"), ("javascript", "iamkun/dayjs"),
    ("javascript", "preactjs/preact"), ("javascript", "Marak/colors.js"),
    ("javascript", "inspect-js/node-deep-equal"),
    ("javascript", "epoberezkin/fast-deep-equal"),
    ("javascript", "npm/node-semver"), ("javascript", "js-cookie/js-cookie"),
    ("javascript", "ljharb/qs"), ("javascript", "json5/json5"),
    ("javascript", "auth0/node-jsonwebtoken"), ("javascript", "JedWatson/classnames"),
    ("javascript", "ai/nanoid"), ("javascript", "alexeyraspopov/picocolors"),
    // go
    ("go", "gorilla/mux"), ("go", "spf13/cobra"), ("go", "urfave/cli"),
    ("go", "google/uuid"), ("go", "satori/go.uuid"), ("go", "fatih/color"),
    ("go", "sirupsen/logrus"), ("go", "pkg/errors"), ("go", "gin-gonic/gin"),
    ("go", "go-chi/chi"), ("go", "julienschmidt/httprouter"), ("go", "rs/cors"),
    ("go", "golang-jwt/jwt"), ("go", "BurntSushi/toml"), ("go", "pelletier/go-toml"),
    ("go", "asaskevich/govalidator"), ("go", "go-playground/validator"),
    ("go", "mitchellh/mapstructure"),
];

#[derive(Debug, Subcommand)]
pub enum HotsetAction {
    /// Build the staged, resumable hot-set pipeline under <home>.
    Build(HotsetBuildArgs),
}

#[derive(Debug, Args)]
pub struct HotsetBuildArgs {
    /// Comma-separated repos as owner/name, full git URLs, or file:// paths
    /// (repeatable; combines with --ecosystems)
    #[arg(long, value_delimiter = ',', required_unless_present = "ecosystems")]
    pub repos: Vec<String>,
    /// Comma-separated ecosystems drawn from the curated hot-set table
    /// (rust, python, javascript, go)
    #[arg(long, value_delimiter = ',')]
    pub ecosystems: Vec<String>,
    /// Repos per ecosystem to take from the curated table
    #[arg(long, default_value_t = 50)]
    pub top: usize,
    /// Hot-set home directory (default: $OSS_SEARCH_HOME or ~/.cache/oss-search)
    #[arg(long)]
    pub home: Option<PathBuf>,
    /// Force the semantic (embedding) stage on even without fastembed
    /// (falls back to a deterministic stub embedder with a loud warning)
    #[arg(long)]
    pub semantic: bool,
    /// Skip the semantic (embedding) stage entirely
    #[arg(long, conflicts_with = "semantic")]
    pub no_semantic: bool,
    /// Parallel clone workers
    #[arg(long, default_value_t = 8)]
    pub concurrency: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct IngestInfo {
    files_total: u64,
    files_accepted: u64,
    bytes_offered: u64,
    unique: u64,
    exact: u64,
    token: u64,
    near: u64,
    license: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RepoState {
    clone_done: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    clone_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ingest: Option<IngestInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IndexInfo {
    repos: Vec<String>,
    docs: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SemanticInfo {
    /// "done" | "skipped" | "failed"
    status: String,
    note: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    embedder: Option<String>,
    #[serde(default)]
    repos: Vec<String>,
    #[serde(default)]
    chunks: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct HotsetState {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    repos: BTreeMap<String, RepoState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    index: Option<IndexInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    semantic: Option<SemanticInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SemanticIntent {
    Auto,
    Force,
    Off,
}

struct RepoSpec {
    name: String,
    url: String,
}

pub fn run(args: &HotsetBuildArgs) -> i32 {
    let t0 = Instant::now();
    let home = match resolve_home(args.home.as_deref()) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    if let Err(e) = std::fs::create_dir_all(&home) {
        eprintln!("error: cannot create home {}: {e}", home.display());
        return 1;
    }
    let state_path = home.join(STATE_FILE);
    let mut state = match load_state(&state_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {}: {e}", state_path.display());
            return 1;
        }
    };
    state.version = STATE_VERSION;

    let specs = match resolve_specs(args) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let any_https = specs.iter().any(|s| s.url.starts_with("https://"));
    if any_https && std::env::var("GITHUB_TOKEN").map_or(true, |t| t.is_empty()) {
        eprintln!(
            "note: GITHUB_TOKEN is not set; cloning github.com unauthenticated \
             (fine for a hot-set build, but rate-limited if you rebuild often)"
        );
    }

    eprintln!("[hotset] home : {}", home.display());
    eprintln!("[hotset] repos: {} requested", specs.len());

    // ---- stage 1: clone ----
    let work_root = home.join("work");
    let mut to_sync: Vec<&RepoSpec> = Vec::new();
    for spec in &specs {
        let done = state
            .repos
            .get(&spec.name)
            .is_some_and(|r| r.clone_done)
            && dir_nonempty(&work_root.join(&spec.name));
        if done {
            continue;
        }
        to_sync.push(spec);
    }
    if to_sync.is_empty() {
        eprintln!("[clone] {}/{} up to date, skipping", specs.len(), specs.len());
    } else {
        eprintln!(
            "[clone] {} up to date, syncing {} at {}-way concurrency...",
            specs.len() - to_sync.len(),
            to_sync.len(),
            args.concurrency
        );
        let coordinator = CloneCoordinator::new(&home).with_concurrency(args.concurrency);
        let urls: Vec<&str> = to_sync.iter().map(|s| s.url.as_str()).collect();
        let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("error: tokio runtime: {e}");
                return 1;
            }
        };
        let reports = rt.block_on(coordinator.clone_all(&urls));
        for report in &reports {
            let entry = state.repos.entry(report.repo.clone()).or_default();
            match &report.error {
                None => {
                    entry.clone_done = true;
                    entry.clone_error = None;
                    eprintln!("[clone] {}: ok", report.repo);
                }
                Some(e) => {
                    entry.clone_done = false;
                    let msg: String = e.chars().take(200).collect();
                    entry.clone_error = Some(msg.clone());
                    eprintln!("[clone] {}: FAILED: {msg}", report.repo);
                }
            }
        }
    }
    let cloned_ok: Vec<String> = specs
        .iter()
        .filter(|s| state.repos.get(&s.name).is_some_and(|r| r.clone_done))
        .map(|s| s.name.clone())
        .collect();
    let clone_failed = specs.len() - cloned_ok.len();
    if cloned_ok.is_empty() {
        eprintln!("error: no repos cloned successfully; aborting");
        let _ = save_if_changed(&state_path, &state);
        return 1;
    }

    // ---- stage 2: ingest (cas + dedup + license) ----
    let mut to_ingest: Vec<&String> = cloned_ok
        .iter()
        .filter(|name| state.repos[*name].ingest.is_none())
        .collect();
    to_ingest.sort();
    if to_ingest.is_empty() {
        eprintln!("[ingest] {}/{} up to date, skipping", cloned_ok.len(), cloned_ok.len());
    } else {
        eprintln!(
            "[ingest] {} up to date, ingesting {}...",
            cloned_ok.len() - to_ingest.len(),
            to_ingest.len()
        );
        let mut ingestor = match CorpusIngestor::open(home.join("cas")) {
            Ok(i) => i,
            Err(e) => {
                eprintln!("error: open CasStore: {e}");
                let _ = save_if_changed(&state_path, &state);
                return 1;
            }
        };
        for name in &to_ingest {
            let work = work_root.join(name.as_str());
            match ingestor.ingest_worktree(name, &work) {
                Ok(r) => {
                    state.repos.entry(name.to_string()).or_default().ingest =
                        Some(IngestInfo {
                            files_total: r.files_total,
                            files_accepted: r.files_accepted,
                            bytes_offered: r.bytes_offered,
                            unique: r.dup.unique,
                            exact: r.dup.exact,
                            token: r.dup.token,
                            near: r.dup.near,
                            license: r.license.as_ref().map(|l| l.spdx.clone()),
                        });
                    eprintln!(
                        "[ingest] {name}: files={} accepted={} license={}",
                        r.files_total,
                        r.files_accepted,
                        r.license.as_ref().map(|l| l.spdx.as_str()).unwrap_or("?")
                    );
                }
                Err(e) => {
                    eprintln!("[ingest] {name}: FAILED: {e}");
                }
            }
        }
        ingestor.cas.seal();
    }
    let ingested: Vec<String> = cloned_ok
        .into_iter()
        .filter(|name| state.repos[name].ingest.is_some())
        .collect();

    // ---- stage 3: trigram index ----
    let index_dir = home.join("index");
    let index_fresh = state.index.as_ref().is_some_and(|ix| {
        ix.repos == ingested && index_dir.join("manifest.json").is_file()
    });
    let index_docs;
    if index_fresh {
        index_docs = state.index.as_ref().map(|ix| ix.docs).unwrap_or(0);
        eprintln!("[index] up to date ({index_docs} docs), skipping");
    } else {
        let t = Instant::now();
        let (docs, gated) = collect_index_docs(&work_root, &ingested);
        let idx = match TrigramIndex::build_parallel(docs) {
            Ok(i) => i,
            Err(e) => {
                eprintln!("error: build index: {e}");
                let _ = save_if_changed(&state_path, &state);
                return 1;
            }
        };
        index_docs = idx.doc_count();
        if let Err(e) = idx.save(&index_dir) {
            eprintln!("error: save index: {e}");
            let _ = save_if_changed(&state_path, &state);
            return 1;
        }
        state.index = Some(IndexInfo { repos: ingested.clone(), docs: index_docs });
        eprintln!(
            "[index] {} docs ({gated} files gated out) -> {} in {:.1}s",
            index_docs,
            index_dir.display(),
            t.elapsed().as_secs_f64()
        );
    }

    // ---- stage 4: semantic ----
    let sem_dir = home.join("semantic");
    let intent = if args.no_semantic {
        SemanticIntent::Off
    } else if args.semantic {
        SemanticIntent::Force
    } else {
        SemanticIntent::Auto
    };
    let sem_fresh = state.semantic.as_ref().is_some_and(|s| {
        s.status == "done" && s.repos == ingested && sem_files_present(&sem_dir)
    });
    let (sem_chunks, sem_summary);
    if sem_fresh {
        sem_chunks = state.semantic.as_ref().map(|s| s.chunks).unwrap_or(0);
        sem_summary = format!("done ({sem_chunks} chunks, up to date)");
        eprintln!("[semantic] up to date ({sem_chunks} chunks), skipping");
    } else {
        // (status, note, chunks, embedder)
        let outcome = match intent {
            SemanticIntent::Off => {
                ("skipped", "disabled by --no-semantic".to_string(), None, None)
            }
            SemanticIntent::Auto => auto_semantic(&sem_dir, &ingested, &work_root),
            SemanticIntent::Force => force_semantic(&sem_dir, &ingested, &work_root),
        };
        let (status, note, chunks, embedder) = outcome;
        sem_chunks = chunks.unwrap_or(0);
        sem_summary = match status {
            "done" => format!("done ({sem_chunks} chunks)"),
            _ => format!("{status} ({note})"),
        };
        if status != "done" {
            eprintln!("[semantic] {status}: {note}");
        }
        state.semantic = Some(SemanticInfo {
            status: status.to_string(),
            note,
            embedder,
            repos: if status == "done" { ingested.clone() } else { Vec::new() },
            chunks: sem_chunks,
        });
    }

    // ---- summary ----
    let cas = CasStore::open(home.join("cas")).ok().and_then(|c| c.stats().ok());
    let (files_total, files_accepted) = state
        .repos
        .values()
        .filter_map(|r| r.ingest.as_ref())
        .fold((0u64, 0u64), |(t, a), i| (t + i.files_total, a + i.files_accepted));
    let disk = dir_size(&home);
    let wall = t0.elapsed().as_secs_f64();
    let _ = save_if_changed(&state_path, &state);
    println!("hotset build complete in {wall:.1}s");
    println!("  repos        : {} attempted / {} failed", specs.len(), clone_failed);
    println!("  files        : {files_total} seen / {files_accepted} accepted");
    match &cas {
        Some(s) => println!(
            "  blobs        : {} distinct ({} raw / {} stored, {:.2}x compressed)",
            s.blobs,
            mib(s.bytes_raw),
            mib(s.bytes_stored),
            s.bytes_raw as f64 / s.bytes_stored.max(1) as f64
        ),
        None => println!("  blobs        : (cas stats unavailable)"),
    }
    println!(
        "  dedup ratio  : {:.3}",
        cas.as_ref().map(|s| s.dedup_ratio).unwrap_or(0.0)
    );
    println!("  index docs   : {index_docs} in index/");
    println!("  semantic     : {sem_summary}");
    println!("  disk usage   : {} under {}", mib(disk), home.display());
    println!("  wall time    : {wall:.1}s");
    0
}

fn resolve_home(flag: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(p) = flag {
        return Ok(p.to_path_buf());
    }
    if let Ok(p) = std::env::var("OSS_SEARCH_HOME")
        && !p.is_empty()
    {
        return Ok(p.into());
    }
    if let Ok(home) = std::env::var("HOME")
        && !home.is_empty()
    {
        return Ok(Path::new(&home).join(".cache").join("oss-search"));
    }
    Err(
        "no home: pass --home, set OSS_SEARCH_HOME, or set HOME (default ~/.cache/oss-search)"
            .to_string(),
    )
}

fn resolve_specs(args: &HotsetBuildArgs) -> Result<Vec<RepoSpec>, String> {
    let mut specs: Vec<RepoSpec> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let add = |url: String, specs: &mut Vec<RepoSpec>, seen: &mut BTreeSet<String>| -> Result<(), String> {
        let name = repo_name_from_url(&url);
        if name.is_empty() {
            return Err(format!("cannot derive a repo name from {url:?}"));
        }
        if seen.insert(name.clone()) {
            specs.push(RepoSpec { name, url });
        }
        Ok(())
    };
    for raw in &args.repos {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        let url = if raw.contains("://") {
            raw.to_string()
        } else if raw.starts_with('/') {
            format!("file://{raw}")
        } else {
            format!("https://github.com/{raw}")
        };
        add(url, &mut specs, &mut seen)?;
    }
    if !args.ecosystems.is_empty() {
        let valid: Vec<&str> = CURATED.iter().map(|(e, _)| *e).collect::<BTreeSet<_>>().into_iter().collect();
        for eco in &args.ecosystems {
            let eco = eco.trim().to_lowercase();
            if !valid.contains(&eco.as_str()) {
                return Err(format!(
                    "unknown ecosystem {eco:?}; valid: {}",
                    valid.join(", ")
                ));
            }
            let picks: Vec<&str> = CURATED
                .iter()
                .filter(|(e, _)| *e == eco)
                .map(|(_, r)| *r)
                .take(args.top)
                .collect();
            if picks.is_empty() {
                return Err(format!("no curated repos for ecosystem {eco}"));
            }
            let available = CURATED.iter().filter(|(e, _)| *e == eco).count();
            if args.top > available {
                eprintln!(
                    "[hotset] --top {} exceeds the {available} curated {eco} repos; using all {available}"
                , args.top);
            }
            for r in picks {
                add(format!("https://github.com/{r}"), &mut specs, &mut seen)?;
            }
        }
    }
    if specs.is_empty() {
        return Err("no repos resolved: pass --repos and/or --ecosystems".to_string());
    }
    Ok(specs)
}

fn repo_name_from_url(url: &str) -> String {
    let trimmed = url.trim_end_matches('/');
    let last = trimmed.rsplit('/').next().unwrap_or(trimmed);
    last.strip_suffix(".git").unwrap_or(last).to_string()
}

fn load_state(path: &Path) -> Result<HotsetState, String> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| format!("corrupt state file ({e}); delete it to rebuild from scratch",)),
        Err(_) => Ok(HotsetState::default()),
    }
}

/// Write the state only when its serialized bytes actually changed, so a
/// fully-cached re-run leaves the file (and its mtime) untouched.
fn save_if_changed(path: &Path, state: &HotsetState) -> std::io::Result<bool> {
    let bytes = serde_json::to_vec_pretty(state).map_err(std::io::Error::other)?;
    if std::fs::read(path).is_ok_and(|old| old == bytes) {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, bytes)?;
    Ok(true)
}

fn dir_nonempty(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|mut d| d.next().is_some())
}

/// (repo, rel path, bytes) for every admitted file in the given repos'
/// work trees, sorted for deterministic doc ids.
fn collect_index_docs(work_root: &Path, repos: &[String]) -> (Vec<(String, String, Vec<u8>)>, u64) {
    let mut docs = Vec::new();
    let mut gated = 0u64;
    for repo in repos {
        let repo_dir = work_root.join(repo);
        let mut files = Vec::new();
        for entry in WalkDir::new(&repo_dir)
            .min_depth(1)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|e| {
                let n = e.file_name().to_string_lossy();
                n != ".git" && n != "node_modules" && n != ".DS_Store"
            })
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_file() {
                files.push(entry.into_path());
            }
        }
        for f in files {
            let Ok(rel) = f.strip_prefix(&repo_dir) else { continue };
            let path = rel.to_string_lossy().replace('\\', "/");
            let Ok(bytes) = std::fs::read(&f) else { continue };
            match admitted(&path, &bytes) {
                Ok(()) => docs.push((repo.clone(), path, bytes)),
                Err(_) => gated += 1,
            }
        }
    }
    (docs, gated)
}

/// (repo, rel path, source) for every source file the semantic stage
/// chunks, sorted for deterministic chunk ids.
fn collect_sources(work_root: &Path, repos: &[String]) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for repo in repos {
        let repo_dir = work_root.join(repo);
        let mut files = Vec::new();
        for entry in WalkDir::new(&repo_dir)
            .min_depth(1)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|e| {
                let n = e.file_name().to_string_lossy();
                n != ".git" && n != "node_modules"
            })
            .filter_map(|e| e.ok())
        {
            if !entry.file_type().is_file() {
                continue;
            }
            let ext = entry
                .path()
                .extension()
                .map(|e| e.to_string_lossy().to_string())
                .unwrap_or_default();
            if LANG_EXTS.contains(&ext.as_str()) {
                files.push(entry.into_path());
            }
        }
        for f in files {
            let Ok(rel) = f.strip_prefix(&repo_dir) else { continue };
            let path = rel.to_string_lossy().replace('\\', "/");
            if let Ok(source) = std::fs::read_to_string(&f) {
                out.push((repo.clone(), path, source));
            }
        }
    }
    out
}

/// (status, note, chunks, embedder) for the auto semantic decision: run
/// real embeddings when fastembed is compiled in and initializes;
/// otherwise skip with a loud note (never silently produce junk vectors).
fn auto_semantic(
    sem_dir: &Path,
    repos: &[String],
    work_root: &Path,
) -> (&'static str, String, Option<usize>, Option<String>) {
    if !cfg!(feature = "semantic-fastembed") {
        return (
            "skipped",
            "oss-cli not built with the `semantic-fastembed` feature; rebuild with \
             `cargo build --features semantic-fastembed` for real embeddings (the \
             lexical index is complete without this stage)"
                .to_string(),
            None,
            None,
        );
    }
    match fastembed_boxed() {
        Ok((name, embedder)) => match run_semantic(sem_dir, repos, work_root, embedder, &name) {
            Ok(chunks) => (
                "done",
                format!("embedder: {name}"),
                Some(chunks),
                Some(name),
            ),
            Err(e) => ("failed", format!("semantic stage failed: {e}"), None, None),
        },
        Err(e) => ("skipped", format!("fastembed init failed: {e}"), None, None),
    }
}

/// (status, note, chunks, embedder) for an explicit --semantic: real
/// vectors when possible, deterministic stub vectors otherwise (with a
/// loud warning).
fn force_semantic(
    sem_dir: &Path,
    repos: &[String],
    work_root: &Path,
) -> (&'static str, String, Option<usize>, Option<String>) {
    if cfg!(feature = "semantic-fastembed")
        && let Ok((name, embedder)) = fastembed_boxed()
    {
        return match run_semantic(sem_dir, repos, work_root, embedder, &name) {
            Ok(chunks) => (
                "done",
                format!("embedder: {name}"),
                Some(chunks),
                Some(name),
            ),
            Err(e) => ("failed", format!("semantic stage failed: {e}"), None, None),
        };
    }
    eprintln!(
        "[semantic] WARNING: --semantic forced but fastembed is unavailable; \
         using StubEmbedder (deterministic token-hasher, NOT semantic vectors)"
    );
    match run_semantic(sem_dir, repos, work_root, Box::new(StubEmbedder::new(256)), "stub") {
        Ok(chunks) => (
            "done",
            "embedder: stub (forced fallback)".to_string(),
            Some(chunks),
            Some("stub".to_string()),
        ),
        Err(e) => ("failed", format!("semantic stage failed: {e}"), None, None),
    }
}

#[cfg(feature = "semantic-fastembed")]
fn fastembed_boxed() -> Result<(String, Box<dyn Embedder>), String> {
    oss_semantic::fast_embedder::FastEmbedder::default_model()
        .map(|e| ("fastembed".to_string(), Box::new(e) as Box<dyn Embedder>))
        .map_err(|e| e.to_string())
}

#[cfg(not(feature = "semantic-fastembed"))]
fn fastembed_boxed() -> Result<(String, Box<dyn Embedder>), String> {
    Err("oss-cli not built with --features semantic-fastembed".to_string())
}

/// Chunk + embed + persist the semantic layer; returns the chunk count.
fn run_semantic(
    sem_dir: &Path,
    repos: &[String],
    work_root: &Path,
    embedder: Box<dyn Embedder>,
    embedder_name: &str,
) -> Result<usize, String> {
    let t = Instant::now();
    std::fs::create_dir_all(sem_dir).map_err(|e| e.to_string())?;
    let sources = collect_sources(work_root, repos);
    let mut chunks: Vec<(String, String, oss_semantic::Chunk)> = Vec::new();
    for (repo, path, source) in &sources {
        let ext = path.rsplit('.').next().unwrap_or("");
        let lang = by_extension(ext).map(|s| s.name.to_string()).unwrap_or_else(|| "window".into());
        let prefixed = format!("{repo}/{path}");
        for c in chunk_source(source, &lang, &prefixed) {
            chunks.push((repo.clone(), lang.clone(), c));
        }
    }
    eprintln!(
        "[semantic] chunked {} source files -> {} chunks (embedder: {embedder_name})",
        sources.len(),
        chunks.len()
    );
    let mut vectors = FlatVectorStore::new();
    for batch in chunks.chunks(SEMANTIC_BATCH) {
        let texts: Vec<String> = batch.iter().map(|(_, _, c)| c.text.clone()).collect();
        let embeddings = embedder.embed(&texts).map_err(|e| e.to_string())?;
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
                .map_err(|e| e.to_string())?;
        }
    }
    vectors.save(sem_dir.join(VECTORS_FILE)).map_err(|e| e.to_string())?;
    let mut store = ChunkStore::open(sem_dir.join(CHUNKS_FILE)).map_err(|e| e.to_string())?;
    let mut groups: BTreeMap<(String, String), Vec<oss_semantic::Chunk>> = BTreeMap::new();
    for (repo, lang, c) in &chunks {
        groups.entry((repo.clone(), lang.clone())).or_default().push(c.clone());
    }
    let mut rows = 0usize;
    for ((repo, lang), items) in &groups {
        let ids: Vec<String> = items.iter().map(|c| c.id.clone()).collect();
        rows += store
            .add_chunks(repo, lang, items.iter().cloned(), Some(&ids))
            .map_err(|e| e.to_string())?;
    }
    let meta = serde_json::json!({
        "embedder": embedder_name,
        "dim": vectors.dim(),
        "files": sources.len(),
        "chunks": vectors.len(),
        "repos": repos,
        "batch_size": SEMANTIC_BATCH,
        "wall_secs": t.elapsed().as_secs_f64(),
    });
    std::fs::write(sem_dir.join(SEM_META_FILE), serde_json::to_vec_pretty(&meta).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    eprintln!(
        "[semantic] {} vectors + {rows} chunk rows -> {} in {:.1}s",
        vectors.len(),
        sem_dir.display(),
        t.elapsed().as_secs_f64()
    );
    Ok(vectors.len())
}

fn sem_files_present(sem_dir: &Path) -> bool {
    [VECTORS_FILE, CHUNKS_FILE, SEM_META_FILE]
        .iter()
        .all(|f| sem_dir.join(f).is_file())
}

fn dir_size(dir: &Path) -> u64 {
    WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.metadata().map(|m| m.len()).unwrap_or(0))
        .sum()
}

fn mib(bytes: u64) -> String {
    format!("{:.2} MiB", bytes as f64 / 1_048_576.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_name_from_common_url_shapes() {
        assert_eq!(repo_name_from_url("https://github.com/a/b"), "b");
        assert_eq!(repo_name_from_url("https://github.com/a/b.git"), "b");
        assert_eq!(repo_name_from_url("file:///tmp/fixtures/alpha"), "alpha");
        assert_eq!(repo_name_from_url("/tmp/x/last"), "last");
    }

    #[test]
    fn curated_table_has_unique_repo_names() {
        let mut seen = BTreeSet::new();
        for (_, name) in CURATED {
            assert!(seen.insert(*name), "duplicate curated repo {name}");
        }
    }

    #[test]
    fn state_roundtrip_and_unchanged_save_leaves_file_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STATE_FILE);
        let mut state = HotsetState {
            version: STATE_VERSION,
            ..Default::default()
        };
        state.repos.insert(
            "alpha".into(),
            RepoState { clone_done: true, clone_error: None, ingest: Some(IngestInfo::default()) },
        );
        assert!(save_if_changed(&path, &state).unwrap());
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert!(!save_if_changed(&path, &state).unwrap());
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), mtime);
        let reloaded: HotsetState = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(reloaded.repos["alpha"].clone_done);
    }
}
