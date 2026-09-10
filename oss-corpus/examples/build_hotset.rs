//! Build the real hot-set corpus under data/corpus and emit REPORT.md.
//!
//! Additive example: drives the implemented pipeline (CloneCoordinator ->
//! CorpusIngestor -> CasStore) over ~100 curated small/medium GitHub repos
//! across rust/python/javascript/go, plus deliberate dedup-control fixtures.

use oss_corpus::admission::Rejection;
use oss_corpus::{
    Blake3Hash, CasStats, CasStore, CloneCoordinator, CorpusIngestor, DupCounts, NearDupPair,
    SyncAction,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// (ecosystem, "owner/name", clone URL)
const REPOS: &[(&str, &str, &str)] = &[
    // rust (32)
    ("rust", "serde-rs/serde", "https://github.com/serde-rs/serde"),
    ("rust", "serde-rs/json", "https://github.com/serde-rs/json"),
    ("rust", "dtolnay/anyhow", "https://github.com/dtolnay/anyhow"),
    ("rust", "dtolnay/thiserror", "https://github.com/dtolnay/thiserror"),
    ("rust", "rust-itertools/itertools", "https://github.com/rust-itertools/itertools"),
    ("rust", "servo/rust-smallvec", "https://github.com/servo/rust-smallvec"),
    ("rust", "Amanieu/parking_lot", "https://github.com/Amanieu/parking_lot"),
    ("rust", "rayon-rs/rayon", "https://github.com/rayon-rs/rayon"),
    ("rust", "BurntSushi/globset", "https://github.com/BurntSushi/globset"),
    ("rust", "rayon-rs/either", "https://github.com/rayon-rs/either"),
    ("rust", "crossbeam-rs/crossbeam", "https://github.com/crossbeam-rs/crossbeam"),
    ("rust", "rust-lang/hashbrown", "https://github.com/rust-lang/hashbrown"),
    ("rust", "tokio-rs/bytes", "https://github.com/tokio-rs/bytes"),
    ("rust", "rust-lang-nursery/lazy-static.rs", "https://github.com/rust-lang-nursery/lazy-static.rs"),
    ("rust", "matklad/once_cell", "https://github.com/matklad/once_cell"),
    ("rust", "rust-lang/regex", "https://github.com/rust-lang/regex"),
    ("rust", "chronotope/chrono", "https://github.com/chronotope/chrono"),
    ("rust", "servo/rust-url", "https://github.com/servo/rust-url"),
    ("rust", "hyperium/mime", "https://github.com/hyperium/mime"),
    ("rust", "dtolnay/semver", "https://github.com/dtolnay/semver"),
    ("rust", "indexmap-rs/indexmap", "https://github.com/indexmap-rs/indexmap"),
    ("rust", "bitflags/bitflags", "https://github.com/bitflags/bitflags"),
    ("rust", "rust-lang/log", "https://github.com/rust-lang/log"),
    ("rust", "rust-cli/env_logger", "https://github.com/rust-cli/env_logger"),
    ("rust", "tokio-rs/tracing", "https://github.com/tokio-rs/tracing"),
    ("rust", "clap-rs/clap", "https://github.com/clap-rs/clap"),
    ("rust", "rust-lang/glob", "https://github.com/rust-lang/glob"),
    ("rust", "BurntSushi/walkdir", "https://github.com/BurntSushi/walkdir"),
    ("rust", "Stebalien/tempfile", "https://github.com/Stebalien/tempfile"),
    ("rust", "dtolnay/ryu", "https://github.com/dtolnay/ryu"),
    ("rust", "dtolnay/itoa", "https://github.com/dtolnay/itoa"),
    ("rust", "dtolnay/syn", "https://github.com/dtolnay/syn"),
    // python (26)
    ("python", "pallets/flask", "https://github.com/pallets/flask"),
    ("python", "psf/requests", "https://github.com/psf/requests"),
    ("python", "pallets/click", "https://github.com/pallets/click"),
    ("python", "Textualize/rich", "https://github.com/Textualize/rich"),
    ("python", "python-attrs/attrs", "https://github.com/python-attrs/attrs"),
    ("python", "encode/httpx", "https://github.com/encode/httpx"),
    ("python", "pallets/jinja", "https://github.com/pallets/jinja"),
    ("python", "pallets/markupsafe", "https://github.com/pallets/markupsafe"),
    ("python", "pallets/werkzeug", "https://github.com/pallets/werkzeug"),
    ("python", "pallets/itsdangerous", "https://github.com/pallets/itsdangerous"),
    ("python", "theskumar/python-dotenv", "https://github.com/theskumar/python-dotenv"),
    ("python", "marshmallow-code/marshmallow", "https://github.com/marshmallow-code/marshmallow"),
    ("python", "coleifer/peewee", "https://github.com/coleifer/peewee"),
    ("python", "arrow-py/arrow", "https://github.com/arrow-py/arrow"),
    ("python", "spulec/freezegun", "https://github.com/spulec/freezegun"),
    ("python", "mahmoud/boltons", "https://github.com/mahmoud/boltons"),
    ("python", "pypa/pip", "https://github.com/pypa/pip"),
    ("python", "pypa/wheel", "https://github.com/pypa/wheel"),
    ("python", "tqdm/tqdm", "https://github.com/tqdm/tqdm"),
    ("python", "kjd/idna", "https://github.com/kjd/idna"),
    ("python", "chardet/chardet", "https://github.com/chardet/chardet"),
    ("python", "urllib3/urllib3", "https://github.com/urllib3/urllib3"),
    ("python", "dateutil/dateutil", "https://github.com/dateutil/dateutil"),
    ("python", "yaml/pyyaml", "https://github.com/yaml/pyyaml"),
    ("python", "certifi/python-certifi", "https://github.com/certifi/python-certifi"),
    ("python", "pydantic/pydantic", "https://github.com/pydantic/pydantic"),
    // javascript (24)
    ("javascript", "lodash/lodash", "https://github.com/lodash/lodash"),
    ("javascript", "chalk/chalk", "https://github.com/chalk/chalk"),
    ("javascript", "tj/commander.js", "https://github.com/tj/commander.js"),
    ("javascript", "minimistjs/minimist", "https://github.com/minimistjs/minimist"),
    ("javascript", "colinhacks/zod", "https://github.com/colinhacks/zod"),
    ("javascript", "vercel/ms", "https://github.com/vercel/ms"),
    ("javascript", "debug-js/debug", "https://github.com/debug-js/debug"),
    ("javascript", "axios/axios", "https://github.com/axios/axios"),
    ("javascript", "left-pad/left-pad", "https://github.com/left-pad/left-pad"),
    ("javascript", "isaacs/node-mkdirp", "https://github.com/isaacs/node-mkdirp"),
    ("javascript", "yargs/yargs", "https://github.com/yargs/yargs"),
    ("javascript", "iamkun/dayjs", "https://github.com/iamkun/dayjs"),
    ("javascript", "preactjs/preact", "https://github.com/preactjs/preact"),
    ("javascript", "Marak/colors.js", "https://github.com/Marak/colors.js"),
    ("javascript", "inspect-js/node-deep-equal", "https://github.com/inspect-js/node-deep-equal"),
    ("javascript", "epoberezkin/fast-deep-equal", "https://github.com/epoberezkin/fast-deep-equal"),
    ("javascript", "npm/node-semver", "https://github.com/npm/node-semver"),
    ("javascript", "js-cookie/js-cookie", "https://github.com/js-cookie/js-cookie"),
    ("javascript", "ljharb/qs", "https://github.com/ljharb/qs"),
    ("javascript", "json5/json5", "https://github.com/json5/json5"),
    ("javascript", "auth0/node-jsonwebtoken", "https://github.com/auth0/node-jsonwebtoken"),
    ("javascript", "JedWatson/classnames", "https://github.com/JedWatson/classnames"),
    ("javascript", "ai/nanoid", "https://github.com/ai/nanoid"),
    ("javascript", "alexeyraspopov/picocolors", "https://github.com/alexeyraspopov/picocolors"),
    // go (18)
    ("go", "gorilla/mux", "https://github.com/gorilla/mux"),
    ("go", "spf13/cobra", "https://github.com/spf13/cobra"),
    ("go", "urfave/cli", "https://github.com/urfave/cli"),
    ("go", "google/uuid", "https://github.com/google/uuid"),
    ("go", "satori/go.uuid", "https://github.com/satori/go.uuid"),
    ("go", "fatih/color", "https://github.com/fatih/color"),
    ("go", "sirupsen/logrus", "https://github.com/sirupsen/logrus"),
    ("go", "pkg/errors", "https://github.com/pkg/errors"),
    ("go", "gin-gonic/gin", "https://github.com/gin-gonic/gin"),
    ("go", "go-chi/chi", "https://github.com/go-chi/chi"),
    ("go", "julienschmidt/httprouter", "https://github.com/julienschmidt/httprouter"),
    ("go", "rs/cors", "https://github.com/rs/cors"),
    ("go", "golang-jwt/jwt", "https://github.com/golang-jwt/jwt"),
    ("go", "BurntSushi/toml", "https://github.com/BurntSushi/toml"),
    ("go", "pelletier/go-toml", "https://github.com/pelletier/go-toml"),
    ("go", "asaskevich/govalidator", "https://github.com/asaskevich/govalidator"),
    ("go", "go-playground/validator", "https://github.com/go-playground/validator"),
    ("go", "mitchellh/mapstructure", "https://github.com/mitchellh/mapstructure"),
];

const CONCURRENCY: usize = 8;
const FIXTURE_TARGET: &str = "vendored/tiny-lru.js";

const VENDORED_TINY_LRU: &str = r#"// SPDX-License-Identifier: MIT
// Vendored tiny LRU used by the dedup-control fixture pair.
"use strict";

class TinyLru {
  constructor(capacity) {
    if (!Number.isInteger(capacity) || capacity <= 0) {
      throw new RangeError("capacity must be a positive integer");
    }
    this.capacity = capacity;
    this.size = 0;
    this.head = null;
    this.tail = null;
    this.map = new Map();
  }

  get(key) {
    const node = this.map.get(key);
    if (!node) return undefined;
    this.detach(node);
    this.pushFront(node);
    return node.value;
  }

  set(key, value) {
    let node = this.map.get(key);
    if (node) {
      node.value = value;
      this.detach(node);
      this.pushFront(node);
      return this;
    }
    node = { key, value, prev: null, next: null };
    this.map.set(key, node);
    this.pushFront(node);
    this.size += 1;
    if (this.size > this.capacity) {
      this.evict();
    }
    return this;
  }

  delete(key) {
    const node = this.map.get(key);
    if (!node) return false;
    this.detach(node);
    this.map.delete(key);
    this.size -= 1;
    return true;
  }

  evict() {
    const victim = this.tail;
    if (!victim) return;
    this.detach(victim);
    this.map.delete(victim.key);
    this.size -= 1;
  }

  detach(node) {
    if (node.prev) node.prev.next = node.next;
    else this.head = node.next;
    if (node.next) node.next.prev = node.prev;
    else this.tail = node.prev;
    node.prev = null;
    node.next = null;
  }

  pushFront(node) {
    node.next = this.head;
    node.prev = null;
    if (this.head) this.head.prev = node;
    this.head = node;
    if (!this.tail) this.tail = node;
  }

  keys() {
    const out = [];
    let cur = this.head;
    while (cur) {
      out.push(cur.key);
      cur = cur.next;
    }
    return out;
  }

  clear() {
    this.map.clear();
    this.head = null;
    this.tail = null;
    this.size = 0;
  }
}

module.exports = TinyLru;
"#;

// Near-dup control: same vendored file with one small method added.
const VENDORED_TINY_LRU_VARIANT: &str = r#"// SPDX-License-Identifier: MIT
// Vendored tiny LRU used by the dedup-control fixture pair.
"use strict";

class TinyLru {
  constructor(capacity) {
    if (!Number.isInteger(capacity) || capacity <= 0) {
      throw new RangeError("capacity must be a positive integer");
    }
    this.capacity = capacity;
    this.size = 0;
    this.head = null;
    this.tail = null;
    this.map = new Map();
  }

  get(key) {
    const node = this.map.get(key);
    if (!node) return undefined;
    this.detach(node);
    this.pushFront(node);
    return node.value;
  }

  has(key) {
    return this.map.has(key);
  }

  set(key, value) {
    let node = this.map.get(key);
    if (node) {
      node.value = value;
      this.detach(node);
      this.pushFront(node);
      return this;
    }
    node = { key, value, prev: null, next: null };
    this.map.set(key, node);
    this.pushFront(node);
    this.size += 1;
    if (this.size > this.capacity) {
      this.evict();
    }
    return this;
  }

  delete(key) {
    const node = this.map.get(key);
    if (!node) return false;
    this.detach(node);
    this.map.delete(key);
    this.size -= 1;
    return true;
  }

  evict() {
    const victim = this.tail;
    if (!victim) return;
    this.detach(victim);
    this.map.delete(victim.key);
    this.size -= 1;
  }

  detach(node) {
    if (node.prev) node.prev.next = node.next;
    else this.head = node.next;
    if (node.next) node.next.prev = node.prev;
    else this.tail = node.prev;
    node.prev = null;
    node.next = null;
  }

  pushFront(node) {
    node.next = this.head;
    node.prev = null;
    if (this.head) this.head.prev = node;
    this.head = node;
    if (!this.tail) this.tail = node;
  }

  keys() {
    const out = [];
    let cur = this.head;
    while (cur) {
      out.push(cur.key);
      cur = cur.next;
    }
    return out;
  }

  clear() {
    this.map.clear();
    this.head = null;
    this.tail = null;
    this.size = 0;
  }
}

module.exports = TinyLru;
"#;

fn repo_name_from_url(url: &str) -> String {
    let trimmed = url.trim_end_matches('/');
    let last = trimmed.rsplit('/').next().unwrap_or(trimmed);
    last.strip_suffix(".git").unwrap_or(last).to_string()
}

fn mib(bytes: u64) -> String {
    format!("{:.2} MiB", bytes as f64 / 1_048_576.0)
}

fn iso_utc(epoch: u64) -> String {
    let days = (epoch / 86400) as i64;
    let secs = epoch % 86400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

fn write_fixtures(root: &Path) {
    let specs = [
        ("fixture-alpha", "exact-dup control", VENDORED_TINY_LRU),
        ("fixture-beta", "exact-dup control", VENDORED_TINY_LRU),
        ("fixture-gamma", "near-dup control", VENDORED_TINY_LRU_VARIANT),
    ];
    for (repo, role, vendored) in specs {
        let dir = root.join("fixtures").join(repo);
        let vdir = dir.join("vendored");
        std::fs::create_dir_all(&vdir).expect("create fixture dir");
        std::fs::write(vdir.join("tiny-lru.js"), vendored).expect("write vendored file");
        std::fs::write(
            dir.join("README.md"),
            format!("# {repo}\n\nDedup-control fixture ({role}); vendors tiny-lru.js.\n"),
        )
        .expect("write fixture readme");
    }
}

#[tokio::main]
async fn main() {
    let t0 = Instant::now();
    let root: PathBuf = std::env::var("OSS_CORPUS_ROOT")
        .unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../data/corpus").to_string())
        .into();
    std::fs::create_dir_all(&root).expect("create corpus root");
    println!("corpus root: {}", root.display());

    let mut names = HashSet::new();
    for (_, _, url) in REPOS {
        let name = repo_name_from_url(url);
        assert!(names.insert(name.clone()), "duplicate repo name: {name}");
    }

    write_fixtures(&root);

    // Phase 1: clone (8-way, unauthenticated https, blobless depth-1).
    let urls: Vec<&str> = REPOS.iter().map(|(_, _, u)| *u).collect();
    let coordinator = CloneCoordinator::new(root.clone()).with_concurrency(CONCURRENCY);
    println!("cloning {} repos at {CONCURRENCY}-way parallelism...", urls.len());
    let t_clone = Instant::now();
    let clone_reports = coordinator.clone_all(&urls).await;
    let clone_secs = t_clone.elapsed().as_secs_f64();
    let ok_count = clone_reports.iter().filter(|r| r.error.is_none()).count();
    println!("clone phase done in {clone_secs:.1}s: {ok_count}/{} ok", urls.len());

    // Phase 2: ingest fixtures first (so they are dedup origins), then repos.
    let t_ing = Instant::now();
    let mut ingestor = CorpusIngestor::open(root.join("cas")).expect("open CasStore");
    let mut repo_reports = Vec::new();
    for repo in ["fixture-alpha", "fixture-beta", "fixture-gamma"] {
        let dir = root.join("fixtures").join(repo);
        match ingestor.ingest_worktree(repo, &dir) {
            Ok(r) => repo_reports.push(r),
            Err(e) => eprintln!("fixture {repo} ingest failed: {e}"),
        }
    }
    let mut ok_reports: Vec<_> = clone_reports.iter().filter(|r| r.error.is_none()).collect();
    ok_reports.sort_by(|a, b| a.repo.cmp(&b.repo));
    let mut ingest_failures: Vec<(String, String)> = Vec::new();
    for cr in &ok_reports {
        let work = root.join("work").join(&cr.repo);
        match ingestor.ingest_worktree(&cr.repo, &work) {
            Ok(r) => {
                println!("ingested {:<24} files={:<5} accepted={:<5}", r.repo, r.files_total, r.files_accepted);
                repo_reports.push(r);
            }
            Err(e) => {
                eprintln!("ingest {} failed: {e}", cr.repo);
                ingest_failures.push((cr.repo.clone(), e.to_string()));
            }
        }
    }
    ingestor.cas.seal();
    let stats: CasStats = ingestor.cas.stats().expect("cas stats");
    let ingest_secs = t_ing.elapsed().as_secs_f64();
    let total_secs = t0.elapsed().as_secs_f64();

    // Dedup control evidence: exact pair from the manifest, near verdict from
    // the MinHash pipeline's own report for fixture-gamma.
    let gamma_near = repo_reports
        .iter()
        .find(|rr| rr.repo == "fixture-gamma")
        .and_then(|rr| rr.near_dups.iter().find(|p| p.path == FIXTURE_TARGET))
        .map(|p| format!("jaccard={:.3} vs `{}`", p.jaccard, p.duplicate_of));
    let control = dedup_control(&ingestor.cas, gamma_near.as_deref());

    // Aggregates.
    let eco_of: HashMap<String, &str> = REPOS
        .iter()
        .map(|(eco, _, u)| (repo_name_from_url(u), *eco))
        .collect();
    let label_of: HashMap<String, &str> = REPOS
        .iter()
        .map(|(_, label, u)| (repo_name_from_url(u), *label))
        .collect();

    let mut eco_rows: BTreeMap<&str, (u64, u64, u64, u64)> = BTreeMap::new();
    for (eco, _, _) in REPOS {
        eco_rows.entry(eco).or_insert((0, 0, 0, 0)).0 += 1;
    }
    let mut failures: Vec<(String, String, String)> = Vec::new();
    for cr in &clone_reports {
        let eco = eco_of.get(&cr.repo).copied().unwrap_or("?");
        let label = label_of.get(&cr.repo).copied().unwrap_or("?");
        match &cr.error {
            None => eco_rows.entry(eco).or_insert((0, 0, 0, 0)).1 += 1,
            Some(e) => failures.push((eco.to_string(), label.to_string(), e.clone())),
        }
    }
    for rr in &repo_reports {
        if rr.repo.starts_with("fixture-") {
            continue;
        }
        if let Some(eco) = eco_of.get(&rr.repo).copied() {
            let row = eco_rows.entry(eco).or_insert((0, 0, 0, 0));
            row.2 += rr.files_accepted;
            row.3 += rr.bytes_offered;
        }
    }

    let mut dup_total = DupCounts::default();
    let mut files_total = 0u64;
    let mut files_accepted = 0u64;
    let mut bytes_offered = 0u64;
    for rr in &repo_reports {
        if rr.repo.starts_with("fixture-") {
            continue;
        }
        dup_total.unique += rr.dup.unique;
        dup_total.exact += rr.dup.exact;
        dup_total.token += rr.dup.token;
        dup_total.near += rr.dup.near;
        files_total += rr.files_total;
        files_accepted += rr.files_accepted;
        bytes_offered += rr.bytes_offered;
    }

    let mut near: Vec<(String, NearDupPair)> = Vec::new();
    for rr in &repo_reports {
        for p in &rr.near_dups {
            near.push((rr.repo.clone(), p.clone()));
        }
    }
    near.sort_by(|a, b| b.1.jaccard.total_cmp(&a.1.jaccard));

    let mut rej_counts: BTreeMap<&str, u64> = BTreeMap::new();
    let mut clone_rej_total = 0u64;
    for cr in &clone_reports {
        for fr in &cr.rejected {
            let key = match &fr.reason {
                Rejection::TooLarge { .. } => "too_large",
                Rejection::BinaryNul { .. } => "binary_nul",
                Rejection::HighEntropy { .. } => "high_entropy",
            };
            *rej_counts.entry(key).or_default() += 1;
            clone_rej_total += 1;
        }
    }
    let mut ingest_rej_total = 0u64;
    for rr in &repo_reports {
        ingest_rej_total += rr.rejections.len() as u64;
    }

    let mut lic_dist: BTreeMap<String, u64> = BTreeMap::new();
    for rr in &repo_reports {
        let k = rr
            .license
            .as_ref()
            .map(|l| l.spdx.clone())
            .unwrap_or_else(|| "unidentified".to_string());
        *lic_dist.entry(k).or_default() += 1;
    }

    // Report.
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let attempted = REPOS.len() as u64;
    let n_cloned = clone_reports
        .iter()
        .filter(|r| matches!(r.action, Some(SyncAction::Cloned)))
        .count();
    let n_updated = clone_reports
        .iter()
        .filter(|r| matches!(r.action, Some(SyncAction::Updated { .. })))
        .count();
    let n_unchanged = clone_reports
        .iter()
        .filter(|r| matches!(r.action, Some(SyncAction::Unchanged { .. })))
        .count();
    let mut rep = String::new();
    let _ = writeln!(
        rep,
        "# Hot-set corpus build report\n\n\
         Generated {gen} by `oss-corpus/examples/build_hotset.rs` \
         (CloneCoordinator -> CorpusIngestor -> CasStore).\n\
         Clone policy: unauthenticated `git clone --depth 1 --single-branch --no-checkout \
         --filter=blob:none` over https, {CONCURRENCY}-way parallel.\n\n\
         - Corpus root: `{root}`\n\
         - Repos attempted: {attempted}\n\
         - Repos succeeded: {ok}\n\
         - Repos failed: {failed}\n\
         - Repos ingested (incl. 3 fixtures): {ingested}\n\
         - Clone actions this pass: {n_cloned} cloned, {n_updated} updated, {n_unchanged} unchanged (pipeline is idempotent; a warm repos/ cache reports unchanged)\n\
         - Wall clock: {total_secs:.1}s total ({clone_secs:.1}s clone+materialize + {ingest_secs:.1}s ingest)\n",
        gen = iso_utc(now),
        root = root.display(),
        ok = ok_count,
        failed = attempted - ok_count as u64,
        ingested = repo_reports.len(),
    );

    let _ = writeln!(rep, "## Per-ecosystem\n");
    let _ = writeln!(
        rep,
        "| ecosystem | attempted | succeeded | failed | files accepted | bytes offered |"
    );
    let _ = writeln!(rep, "|---|---|---|---|---|---|");
    let mut tot = (0u64, 0u64, 0u64, 0u64);
    for (eco, (att, okc, files, bytes)) in &eco_rows {
        let failed = att - okc;
        let _ = writeln!(
            rep,
            "| {eco} | {att} | {okc} | {failed} | {files} | {} |",
            mib(*bytes)
        );
        tot.0 += att;
        tot.1 += okc;
        tot.2 += files;
        tot.3 += bytes;
    }
    let _ = writeln!(
        rep,
        "| **total** | {} | {} | {} | {} | {} |",
        tot.0,
        tot.1,
        tot.0 - tot.1,
        tot.2,
        mib(tot.3)
    );

    if !failures.is_empty() {
        let _ = writeln!(rep, "\n### Clone failures\n");
        for (eco, label, err) in &failures {
            let msg: String = err.chars().take(220).collect();
            let _ = writeln!(rep, "- [{eco}] {label}: {msg}");
        }
    }
    if !ingest_failures.is_empty() {
        let _ = writeln!(rep, "\n### Ingest failures\n");
        for (repo, err) in &ingest_failures {
            let _ = writeln!(rep, "- {repo}: {err}");
        }
    }

    let _ = writeln!(rep, "\n## CasStore stats\n");
    let _ = writeln!(rep, "| metric | value |");
    let _ = writeln!(rep, "|---|---|");
    let _ = writeln!(rep, "| blobs (distinct) | {} |", stats.blobs);
    let _ = writeln!(rep, "| packs | {} |", stats.packs);
    let _ = writeln!(rep, "| bytes_raw | {} ({}) |", stats.bytes_raw, mib(stats.bytes_raw));
    let _ = writeln!(
        rep,
        "| bytes_stored (zstd-3) | {} ({}) |",
        stats.bytes_stored,
        mib(stats.bytes_stored)
    );
    let _ = writeln!(rep, "| bytes_offered (all puts) | {} ({}) |", stats.bytes_offered, mib(stats.bytes_offered));
    let _ = writeln!(rep, "| dedup_ratio (offered/raw) | {:.3} |", stats.dedup_ratio);
    let _ = writeln!(
        rep,
        "| compression (raw/stored) | {:.2}x |",
        stats.bytes_raw as f64 / stats.bytes_stored.max(1) as f64
    );

    let _ = writeln!(rep, "\n## Ingestion totals (repos only, fixtures excluded)\n");
    let _ = writeln!(rep, "- files seen: {files_total}");
    let _ = writeln!(rep, "- files accepted into manifest: {files_accepted}");
    let _ = writeln!(rep, "- bytes offered: {} ({})", bytes_offered, mib(bytes_offered));
    let _ = writeln!(
        rep,
        "- dedup verdicts: unique={} exact={} token={} near={}",
        dup_total.unique, dup_total.exact, dup_total.token, dup_total.near
    );

    let _ = writeln!(rep, "\n## Top near-dup pairs (MinHash/LSH, threshold 0.85)\n");
    if near.is_empty() {
        let _ = writeln!(rep, "(none found)");
    } else {
        let _ = writeln!(rep, "| file | duplicate_of | jaccard | estimated |");
        let _ = writeln!(rep, "|---|---|---|---|");
        for (repo, p) in near.iter().take(20) {
            let _ = writeln!(
                rep,
                "| {}:{} | `{}` | {:.3} | {:.3} |",
                repo, p.path, p.duplicate_of, p.jaccard, p.estimated
            );
        }
        let _ = writeln!(rep, "\n({} near-dup pairs total)", near.len());
    }

    let _ = writeln!(rep, "\n## License distribution (repo-level detection)\n");
    let _ = writeln!(rep, "| license | repos |");
    let _ = writeln!(rep, "|---|---|");
    let mut lic_sorted: Vec<(&String, &u64)> = lic_dist.iter().collect();
    lic_sorted.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
    for (lic, count) in lic_sorted {
        let _ = writeln!(rep, "| {lic} | {count} |");
    }

    let _ = writeln!(rep, "\n## Admission-gate rejections\n");
    let _ = writeln!(rep, "| reason | count (clone phase) |");
    let _ = writeln!(rep, "|---|---|");
    for key in ["too_large", "binary_nul", "high_entropy"] {
        let _ = writeln!(rep, "| {key} | {} |", rej_counts.get(key).copied().unwrap_or(0));
    }
    let _ = writeln!(rep, "| **total** | {clone_rej_total} |");
    let _ = writeln!(
        rep,
        "\nIngest-phase rejections (work trees are pre-gated, expect 0): {ingest_rej_total}."
    );

    let _ = writeln!(rep, "\n## Deliberate dedup control\n");
    let _ = writeln!(
        rep,
        "Fixtures `fixture-alpha` and `fixture-beta` both vendor the identical file \
         `{FIXTURE_TARGET}` (plus distinct READMEs); `fixture-gamma` vendors a near-identical \
         variant (one small `has()` method added) as a MinHash near-dup control.\n"
    );
    let _ = writeln!(rep, "- vendored-file blake3: `b3:{}`", control.hash_hex);
    let _ = writeln!(rep, "- distinct blob hashes for that file: {} (expected 1)", control.distinct_hashes);
    let _ = writeln!(
        rep,
        "- `repo_blobs` manifest entries for that hash: {} (expected 2: fixture-alpha + fixture-beta)",
        control.manifest_entries
    );
    let _ = writeln!(
        rep,
        "- near-dup control: {} (expected: flagged Near vs the alpha/beta blob)",
        control.gamma_verdict
    );
    let _ = writeln!(
        rep,
        "\nVerdict: {} — one canonical blob, two repo manifest entries;{}",
        if control.distinct_hashes == 1 && control.manifest_entries == 2 {
            "PASS"
        } else {
            "FAIL"
        },
        if control.gamma_is_near {
            " gamma variant flagged near-dup as designed."
        } else {
            " gamma variant NOT flagged near-dup."
        }
    );

    let _ = std::fs::write(root.join("REPORT.md"), &rep);
    let _ = std::fs::write(
        root.join("clone-reports.json"),
        serde_json::to_string_pretty(&clone_reports).unwrap(),
    );
    let _ = std::fs::write(
        root.join("ingest-reports.json"),
        serde_json::to_string_pretty(&repo_reports).unwrap(),
    );
    let _ = std::fs::write(
        root.join("cas-stats.json"),
        serde_json::to_string_pretty(&stats).unwrap(),
    );
    println!("\nreport: {}", root.join("REPORT.md").display());
    println!("blobs={} packs={} raw={} stored={} dedup={:.3}", stats.blobs, stats.packs, stats.bytes_raw, stats.bytes_stored, stats.dedup_ratio);
    println!("done in {total_secs:.1}s");
}

struct Control {
    hash_hex: String,
    distinct_hashes: u64,
    manifest_entries: u64,
    gamma_verdict: String,
    gamma_is_near: bool,
}

fn dedup_control(cas: &CasStore, gamma_near: Option<&str>) -> Control {
    let mut hashes: HashSet<Blake3Hash> = HashSet::new();
    let mut entries = 0u64;
    let mut hash_hex = String::from("-");
    for repo in ["fixture-alpha", "fixture-beta"] {
        for row in cas.repo_files(repo).unwrap_or_default() {
            if row.path == FIXTURE_TARGET {
                hashes.insert(row.hash);
                entries += 1;
                hash_hex = row.hash.hex();
            }
        }
    }
    // gamma near-dup evidence comes from the pipeline's own MinHash verdict.
    let mut gamma_verdict = "no gamma row found".to_string();
    let mut gamma_is_near = false;
    let gamma_rows = cas.repo_files("fixture-gamma").unwrap_or_default();
    if gamma_rows.iter().any(|r| r.path == FIXTURE_TARGET) {
        match gamma_near {
            Some(detail) => {
                gamma_is_near = true;
                gamma_verdict = format!("flagged Near by MinHash/LSH ({detail})");
            }
            None => gamma_verdict = "distinct hash present but NOT flagged Near by MinHash".to_string(),
        }
    }
    Control {
        hash_hex,
        distinct_hashes: hashes.len() as u64,
        manifest_entries: entries,
        gamma_verdict,
        gamma_is_near,
    }
}
