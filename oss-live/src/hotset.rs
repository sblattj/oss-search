//! The hot-set engine: serves the 7-tool surface from a locally built
//! [`TrigramIndex`] hot-set (created by `oss-cli hotset build`), plus a
//! merged hot-set + live federation engine for `oss-mcp --live` when a
//! hot-set is present.
//!
//! Path convention (shared with oss-cli): the home directory is
//! `OSS_SEARCH_HOME` when set (non-empty), else `~/.cache/oss-search`, and
//! the built index lives at `<home>/index` (manifest version 1).
//!
//! Truthfulness rules mirror the live engine's:
//! * local hits are attributed `backend=local_index` in both the result
//!   rows and `backend_status`;
//! * `oss_repo_tree` / `oss_fetch_file` serve the indexed snapshot — a miss
//!   is a typed `NotFound`, never a faked negative;
//! * metadata the snapshot cannot know (stars, license, activity) is
//!   omitted, not invented — pass `--live` for it.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use oss_core::{
    BackendStatus, CodeSearchMode, EngineOutput, SearchEngine, ToolError, DEFAULT_CONTEXT_LINES,
    FetchDocsParams, FetchFileParams, GuideParams, RepoProfileParams, RepoTreeParams,
    SearchCodeParams, SearchReposParams, format_snippet,
};
use oss_index::{DocMeta, Filters, LineMatch, TrigramIndex};
use oss_query::Query;
use serde_json::{json, Value};

use crate::LiveEngine;
use crate::code_search::{cursor_offset, query_error};
use crate::status;
use crate::util::strip_probe_delimiters;

pub const LOCAL_INDEX_BACKEND: &str = "local_index";
/// Per-probe fetch cap for local searches; page windows are applied after
/// the merge, so this bounds cost without hiding page-sized results.
const MAX_PROBE_RESULTS: usize = 500;

/// The hot-set home directory: `OSS_SEARCH_HOME` (when non-empty), else
/// `~/.cache/oss-search`.
pub fn default_home() -> PathBuf {
    home_from(std::env::var_os("OSS_SEARCH_HOME").as_deref())
}

fn home_from(env_home: Option<&std::ffi::OsStr>) -> PathBuf {
    if let Some(h) = env_home
        && !h.is_empty()
    {
        return PathBuf::from(h);
    }
    match std::env::home_dir() {
        Some(home) => home.join(".cache").join("oss-search"),
        None => PathBuf::from(".cache").join("oss-search"),
    }
}

/// Where the built hot-set index lives: `<home>/index`.
pub fn default_index_dir() -> PathBuf {
    default_home().join("index")
}

/// [`SearchEngine`] over a locally built hot-set index.
///
/// Search runs on the loaded [`TrigramIndex`]; repo/file/docs/profile tools
/// read the saved snapshot (the `docs.json` manifest plus lazy `doc_N.bin`
/// content reads) so `oss_repo_tree` and `oss_fetch_file` degrade
/// truthfully to typed `NotFound` errors when a repo or path is not in the
/// snapshot.
pub struct HotsetEngine {
    index: TrigramIndex,
    index_dir: PathBuf,
    /// repo -> docs sorted by path (the manifest view of the snapshot).
    repos: BTreeMap<String, Vec<DocMeta>>,
}

impl std::fmt::Debug for HotsetEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // TrigramIndex has no Debug; report the load-bearing facts.
        f.debug_struct("HotsetEngine")
            .field("index_dir", &self.index_dir)
            .field("docs", &self.doc_count())
            .field("repos", &self.repos.len())
            .finish()
    }
}

impl HotsetEngine {
    /// Open a hot-set index from `index_dir`. Fails with a human-readable
    /// reason when the directory is missing, unreadable, or a manifest
    /// version this engine does not understand.
    pub fn open(index_dir: &Path) -> Result<Self, String> {
        let manifest_path = index_dir.join("manifest.json");
        let manifest: Value = serde_json::from_slice(
            &std::fs::read(&manifest_path)
                .map_err(|e| format!("read {}: {e}", manifest_path.display()))?,
        )
        .map_err(|e| format!("parse {}: {e}", manifest_path.display()))?;
        let version = manifest["version"].as_u64().unwrap_or(0);
        if version != 1 {
            return Err(format!(
                "unsupported hot-set manifest version {version} (expected 1)"
            ));
        }
        let metas: Vec<DocMeta> = serde_json::from_slice(
            &std::fs::read(index_dir.join("docs.json"))
                .map_err(|e| format!("read docs.json: {e}"))?,
        )
        .map_err(|e| format!("parse docs.json: {e}"))?;
        let index = TrigramIndex::load(index_dir).map_err(|e| format!("load index: {e}"))?;
        let mut repos: BTreeMap<String, Vec<DocMeta>> = BTreeMap::new();
        for meta in metas {
            repos.entry(meta.repo.clone()).or_default().push(meta);
        }
        for docs in repos.values_mut() {
            docs.sort_by(|a, b| a.path.cmp(&b.path));
        }
        Ok(Self {
            index,
            index_dir: index_dir.to_path_buf(),
            repos,
        })
    }

    /// Open the hot-set from the default location
    /// (`<OSS_SEARCH_HOME ?? ~/.cache/oss-search>/index`).
    pub fn open_default() -> Result<Self, String> {
        Self::open(&default_index_dir())
    }

    pub fn doc_count(&self) -> usize {
        self.index.doc_count()
    }

    pub fn index_dir(&self) -> &Path {
        &self.index_dir
    }

    fn status_ok(&self) -> BackendStatus {
        status::ok_with(
            LOCAL_INDEX_BACKEND,
            format!(
                "hot-set: {} docs from {}",
                self.doc_count(),
                self.index_dir.display()
            ),
        )
    }

    fn local_status_vec(&self) -> Vec<BackendStatus> {
        vec![self.status_ok()]
    }

    /// Exact match first, then case-insensitive (mirrors the stub engine).
    fn find_repo(&self, repo: &str) -> Option<(&str, &Vec<DocMeta>)> {
        if let Some((key, docs)) = self.repos.get_key_value(repo) {
            return Some((key.as_str(), docs));
        }
        self.repos
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(repo))
            .map(|(key, docs)| (key.as_str(), docs))
    }

    fn find_doc(&self, repo: &str, path: &str) -> Option<&DocMeta> {
        self.repos.get(repo)?.iter().find(|d| d.path == path)
    }

    /// Read one snapshot file. The trigram admission gate already rejected
    /// NUL-containing bytes; lossy decoding is a final display safety net.
    fn read_doc(&self, meta: &DocMeta) -> Result<String, ToolError> {
        let bytes =
            std::fs::read(self.index_dir.join(format!("doc_{}.bin", meta.id))).map_err(|e| {
                ToolError::Engine {
                    message: format!("read snapshot blob for {}:{}: {e}", meta.repo, meta.path),
                }
            })?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    fn hotset_guide_section(&self, note: &str) -> Value {
        json!({
            "status": "served",
            "docs": self.doc_count(),
            "index_dir": self.index_dir.display().to_string(),
            "note": note,
        })
    }
}

struct FileHit {
    repo: String,
    path: String,
    matches: Vec<LineMatch>,
    probes: BTreeSet<String>,
}

fn not_in_hotset(kind: &'static str, name: &str, input: Value, correction: &str) -> ToolError {
    ToolError::NotFound {
        kind,
        name: name.to_string(),
        input,
        correction: format!("{correction} (not in the local hot-set snapshot)"),
    }
}

fn readme_digest(content: &str) -> String {
    let digest: Vec<&str> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(6)
        .collect();
    let joined = digest.join(" ");
    if joined.chars().count() > 400 {
        let kept: String = joined.chars().take(400).collect();
        format!("{kept}…")
    } else {
        joined
    }
}

fn cursor_base_offset(cursor: &Option<String>) -> u64 {
    cursor
        .as_deref()
        .and_then(|c| c.strip_prefix("off:"))
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

fn repo_rollup_rows(
    merged: &BTreeMap<(String, String), FileHit>,
    total_probes: usize,
) -> Vec<Value> {
    let mut per_repo: BTreeMap<String, Vec<&FileHit>> = BTreeMap::new();
    for hit in merged.values() {
        per_repo.entry(hit.repo.clone()).or_default().push(hit);
    }
    per_repo
        .into_iter()
        .map(|(repo, hits)| {
            let probes_hit: BTreeSet<&str> = hits
                .iter()
                .flat_map(|h| h.probes.iter().map(|s| s.as_str()))
                .collect();
            let sample = hits.iter().find_map(|h| {
                h.matches.first().map(|m| {
                    format!(
                        "{}:{} {} <- probe '{}'",
                        h.path,
                        m.line_no,
                        m.line_text,
                        h.probes.iter().next().cloned().unwrap_or_default()
                    )
                })
            });
            let probes_vec: Vec<&str> = probes_hit.iter().copied().collect();
            json!({
                "repo": repo,
                "probes_hit": probes_vec,
                "probe_breadth": probes_hit.len(),
                "total_probes": total_probes,
                "hit_files": hits.len(),
                "backends": [LOCAL_INDEX_BACKEND],
                "sample_evidence": sample,
            })
        })
        .collect()
}

impl HotsetEngine {
    fn snippet_rows(&self, merged: &BTreeMap<(String, String), FileHit>) -> Vec<Value> {
        let mut rows = Vec::new();
        for hit in merged.values() {
            let Some(first) = hit.matches.first() else {
                continue;
            };
            let (lines, locator, start_line, end_line) =
                match self.find_doc(&hit.repo, &hit.path) {
                    Some(meta) => match self.read_doc(meta) {
                        Ok(content) => {
                            let file_lines: Vec<String> =
                                content.lines().map(|l| l.to_string()).collect();
                            let snip = format_snippet(
                                &hit.path,
                                &file_lines,
                                first.line_no as u64,
                                DEFAULT_CONTEXT_LINES,
                            );
                            (snip.lines, snip.locator, snip.start_line, snip.end_line)
                        }
                        Err(_) => (Vec::new(), hit.path.clone(), 0, 0),
                    },
                    None => (Vec::new(), hit.path.clone(), 0, 0),
                };
            rows.push(json!({
                "repo": hit.repo,
                "path": hit.path,
                "locator": locator,
                "start_line": start_line,
                "end_line": end_line,
                "match_line": first.line_no,
                "probe": hit.probes.iter().next().cloned().unwrap_or_default(),
                "lines": lines,
                "backends": [LOCAL_INDEX_BACKEND],
                "url": format!("https://github.com/{}/blob/main/{}", hit.repo, hit.path),
            }));
        }
        rows
    }
}

impl SearchEngine for HotsetEngine {
    fn backend_status(&self, _tool: &str) -> Vec<BackendStatus> {
        self.local_status_vec()
    }

    fn search_code(&self, params: &SearchCodeParams) -> Result<EngineOutput, ToolError> {
        let input = serde_json::to_value(params).unwrap_or(Value::Null);
        let offset = cursor_offset(&params.cursor, &input)?;

        // Probe parsing mirrors the live engine: oss-query validates the
        // pattern/kind/filters so typed errors match across engines.
        let mut probes: Vec<(String, bool)> = Vec::new();
        for probe in &params.probes {
            let (pattern, is_regex) = strip_probe_delimiters(probe);
            let mut obj = json!({ "pattern": pattern });
            if is_regex {
                obj["kind"] = json!("regex");
            }
            if let Some(lang) = &params.language {
                obj["langs"] = json!([lang]);
            }
            if let Some(path) = &params.path {
                obj["paths"] = json!([path]);
            }
            if let Some(filter) = &params.repo_filter
                && filter.contains('/')
            {
                obj["repos"] = json!([filter]);
            }
            let q = Query::from_json(&obj.to_string()).map_err(|e| query_error(e, &input))?;
            q.plan().map_err(|e| query_error(e, &input))?;
            probes.push((pattern.to_string(), is_regex));
        }

        let mut filters = Filters::default();
        if let Some(lang) = &params.language {
            filters.langs = vec![lang.to_lowercase()];
        }
        if let Some(path) = &params.path {
            filters.path_contains = vec![path.clone()];
        }
        if let Some(filter) = &params.repo_filter {
            filters.repos = vec![filter.clone()];
        }

        // Per-file merge across probes, local_index attribution. Literal
        // probes are lowercased for the index call: the tool surface has no
        // case flag and the live-engine default is case-insensitive, and
        // the trigram case expansion only grows lowercase grams. Regex
        // probes stay case-sensitive (lowercasing a pattern would rewrite
        // character classes).
        let fetch_max = (offset as usize + params.limit as usize).min(MAX_PROBE_RESULTS);
        let mut merged: BTreeMap<(String, String), FileHit> = BTreeMap::new();
        for (pattern, is_regex) in &probes {
            let outcome = if *is_regex {
                self.index.search_regex(pattern, &filters, fetch_max)
            } else {
                self.index.search_literal(&pattern.to_lowercase(), &filters, fetch_max)
            }
            .map_err(|e| ToolError::Engine {
                message: format!("local index search failed: {e}"),
            })?;
            for r in outcome.results {
                let entry = merged
                    .entry((r.repo.clone(), r.path.clone()))
                    .or_insert_with(|| FileHit {
                        repo: r.repo.clone(),
                        path: r.path.clone(),
                        matches: Vec::new(),
                        probes: BTreeSet::new(),
                    });
                entry.probes.insert(pattern.clone());
                if entry.matches.is_empty() {
                    entry.matches = r.matches.clone();
                }
            }
        }

        let mut all_rows: Vec<Value> = match params.mode {
            CodeSearchMode::Repos => {
                let mut rows = repo_rollup_rows(&merged, params.probes.len());
                rows.sort_by(|a, b| {
                    let breadth_a = a["probe_breadth"].as_u64().unwrap_or(0);
                    let breadth_b = b["probe_breadth"].as_u64().unwrap_or(0);
                    breadth_b.cmp(&breadth_a).then_with(|| {
                        let files_a = a["hit_files"].as_u64().unwrap_or(0);
                        let files_b = b["hit_files"].as_u64().unwrap_or(0);
                        files_b.cmp(&files_a)
                    })
                });
                rows
            }
            CodeSearchMode::Snippets => self.snippet_rows(&merged),
        };
        if matches!(params.mode, CodeSearchMode::Repos) {
            all_rows.sort_by(|a, b| {
                a["repo"].as_str().unwrap_or("").cmp(b["repo"].as_str().unwrap_or(""))
            });
        }

        let page_start = (offset as usize).min(all_rows.len());
        let page_end = (page_start + params.limit as usize).min(all_rows.len());
        let page_len = page_end - page_start;
        let rows: Vec<Value> = all_rows[page_start..page_end].to_vec();
        let has_more = page_end < all_rows.len();
        Ok(EngineOutput {
            results: rows,
            total: offset + page_len as u64,
            has_more,
            partial: false,
            next_cursor: has_more.then(|| format!("off:{}", offset + page_len as u64)),
        })
    }

    fn search_repos(&self, params: &SearchReposParams) -> Result<EngineOutput, ToolError> {
        let input = serde_json::to_value(params).unwrap_or(Value::Null);
        let offset = cursor_offset(&params.cursor, &input)?;
        let phrasings: Vec<String> = params
            .query
            .split(';')
            .map(|p| p.trim().to_lowercase())
            .filter(|p| !p.is_empty())
            .collect();
        let mut matched: Vec<(&String, &Vec<DocMeta>)> = self
            .repos
            .iter()
            .filter(|(repo, _)| {
                let hay = repo.to_lowercase();
                phrasings.is_empty() || phrasings.iter().any(|p| hay.contains(p))
            })
            .collect();
        // All sort modes fall back to snapshot size: stars and activity are
        // metadata the local hot-set does not know.
        matched.sort_by(|a, b| {
            b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(b.0))
        });
        let total = matched.len() as u64;
        let start = (offset as usize).min(matched.len());
        let end = (start + params.limit as usize).min(matched.len());
        let rows: Vec<Value> = matched[start..end]
            .iter()
            .map(|(repo, docs)| {
                let langs: BTreeSet<&str> =
                    docs.iter().map(|d| d.lang.as_str()).collect();
                let mut row = json!({
                    "repo": repo,
                    "hotset": true,
                    "hotset_file_count": docs.len(),
                    "hotset_langs": langs.into_iter().collect::<Vec<_>>(),
                    "url": format!("https://github.com/{repo}"),
                });
                if let Some(fields) = &params.fields {
                    let keep_urls = fields.iter().any(|f| f == "urls");
                    row.as_object_mut().unwrap().retain(|k, _| {
                        k == "repo" || k == "hotset_file_count" || (keep_urls && k == "url")
                    });
                    row["fields_returned"] = json!(fields);
                }
                row
            })
            .collect();
        let has_more = end < matched.len();
        Ok(EngineOutput {
            results: rows,
            total,
            has_more,
            partial: false,
            next_cursor: has_more.then(|| format!("off:{end}")),
        })
    }

    fn repo_profile(&self, params: &RepoProfileParams) -> Result<EngineOutput, ToolError> {
        let input = serde_json::to_value(params).unwrap_or(Value::Null);
        let (repo, docs) = self.find_repo(&params.repo).ok_or_else(|| {
            not_in_hotset(
                "repo",
                &params.repo,
                input,
                "call oss_search_repos to list hot-set repos, or run oss-mcp --live for full metadata",
            )
        })?;
        let mut langs: BTreeSet<&str> = BTreeSet::new();
        let mut top_dirs: BTreeSet<String> = BTreeSet::new();
        for d in docs {
            langs.insert(d.lang.as_str());
            if let Some(seg) = d.path.split('/').next()
                && !seg.contains('.')
            {
                top_dirs.insert(seg.to_string());
            }
        }
        let lang_list = langs.into_iter().collect::<Vec<_>>().join(", ");
        let mut row = json!({
            "repo": repo,
            "what": format!("local hot-set snapshot: {} indexed files ({lang_list})", docs.len()),
            "catch": "stars, license and activity metadata require --live",
            "hotset": {
                "file_count": docs.len(),
                "index_dir": self.index_dir.display().to_string(),
            },
            "architecture_top_level": top_dirs.into_iter().collect::<Vec<_>>(),
        });
        if params.deep_docs {
            let readme = docs
                .iter()
                .find(|d| d.path.eq_ignore_ascii_case("readme.md"));
            row["deep_docs"] = match readme {
                Some(d) => match self.read_doc(d) {
                    Ok(content) => json!({
                        "source": format!("{repo}/{}", d.path),
                        "digest": readme_digest(&content),
                    }),
                    Err(_) => json!("README.md is in the snapshot but could not be read"),
                },
                None => json!("no README.md in the hot-set snapshot"),
            };
        }
        Ok(EngineOutput::complete(vec![row]))
    }

    fn repo_tree(&self, params: &RepoTreeParams) -> Result<EngineOutput, ToolError> {
        let input = serde_json::to_value(params).unwrap_or(Value::Null);
        let (_, docs) = self.find_repo(&params.repo).ok_or_else(|| {
            not_in_hotset(
                "repo",
                &params.repo,
                input,
                "call oss_search_repos to list hot-set repos, or run oss-mcp --live for the GitHub tree",
            )
        })?;
        let mut paths: Vec<&str> = docs.iter().map(|d| d.path.as_str()).collect();
        if let Some(prefix) = &params.path_prefix {
            paths.retain(|p| p.starts_with(prefix.as_str()));
        }
        if let Some(glob) = &params.glob {
            let pattern = glob.trim_start_matches('*');
            paths.retain(|p| {
                p.rsplit('/')
                    .next()
                    .is_some_and(|name| name.ends_with(pattern))
            });
        }
        let total = paths.len() as u64;
        let end = (params.limit as usize).min(paths.len());
        let rows: Vec<Value> = paths[..end].iter().map(|p| json!({ "path": p })).collect();
        let has_more = end < paths.len();
        Ok(EngineOutput {
            results: rows,
            total,
            has_more,
            partial: false,
            next_cursor: has_more.then(|| format!("off:{end}")),
        })
    }

    fn fetch_file(&self, params: &FetchFileParams) -> Result<EngineOutput, ToolError> {
        let input = serde_json::to_value(params).unwrap_or(Value::Null);
        let meta =
            self.find_doc(&params.repo, &params.path)
                .ok_or_else(|| {
                    not_in_hotset(
                        "file",
                        &format!("{}/{}", params.repo, params.path),
                        input,
                        "call oss_repo_tree for the indexed paths; a path outside the hot-set needs --live",
                    )
                })?;
        let content = self.read_doc(meta)?;
        let lines: Vec<&str> = content.lines().collect();
        let total_lines = lines.len() as u64;
        let (start, end) = match &params.line_range {
            Some(range) => (
                range.start.min(total_lines.max(1)),
                range.end.min(total_lines),
            ),
            None => (1, total_lines.min(100)),
        };
        let slice = if total_lines == 0 {
            String::new()
        } else {
            lines[(start as usize - 1)..(end as usize)].join("\n")
        };
        let row = json!({
            "repo": meta.repo,
            "path": meta.path,
            "ref": "hotset",
            "start_line": start,
            "end_line": end,
            "total_lines": total_lines,
            "content": slice,
            "source": LOCAL_INDEX_BACKEND,
        });
        Ok(EngineOutput::complete(vec![row]))
    }

    fn fetch_docs(&self, params: &FetchDocsParams) -> Result<EngineOutput, ToolError> {
        let input = serde_json::to_value(params).unwrap_or(Value::Null);
        let (repo, docs) = self
            .find_repo(&params.repo)
            .ok_or_else(|| {
                not_in_hotset(
                    "repo",
                    &params.repo,
                    input.clone(),
                    "call oss_search_repos to list hot-set repos, or run oss-mcp --live for hosted docs",
                )
            })?;
        let (pattern, is_regex) = strip_probe_delimiters(&params.pattern);
        let needle = pattern.to_lowercase();
        let re = if is_regex {
            Some(regex::Regex::new(pattern).map_err(|e| ToolError::InvalidValue {
                field: "pattern".into(),
                message: format!("invalid regex `{pattern}`: {e}"),
                input: input.clone(),
                correction:
                    "use RE2-style syntax with a literal run of 3+ characters; see oss_guide"
                        .into(),
            })?)
        } else {
            None
        };
        let mut rows: Vec<Value> = Vec::new();
        for doc in docs {
            if !matches!(
                doc.path.rsplit('.').next().unwrap_or(""),
                "md" | "mdx" | "markdown" | "txt"
            ) {
                continue;
            }
            let content = self.read_doc(doc)?;
            let url = format!("https://github.com/{repo}/blob/main/{}", doc.path);
            let hits: Vec<Value> = content
                .lines()
                .enumerate()
                .filter(|(_, line)| match &re {
                    Some(re) => re.is_match(line),
                    None => line.to_lowercase().contains(&needle),
                })
                .map(|(i, line)| {
                    json!({
                        "line": i + 1,
                        "text": line,
                        "breadcrumb": format!("Source: {url}"),
                    })
                })
                .collect();
            if !hits.is_empty() {
                rows.push(json!({
                    "repo": repo,
                    "page": doc.path,
                    "source_url": url,
                    "hits": hits,
                }));
            }
        }
        Ok(EngineOutput::complete(rows))
    }

    fn guide(&self, _params: &GuideParams) -> Result<EngineOutput, ToolError> {
        let row = json!({
            "engine": "hotset",
            "hotset": self.hotset_guide_section(
                "all seven tools serve the local snapshot; pass --live to federate remote backends"
            ),
            "which_tool_when": [
                "oss_search_repos: discovery over the local hot-set only (repo names and snapshot file counts; stars/license need --live)",
                "oss_search_code: vetting over the local hot-set — fast literal and /regex/ probes, results attributed backend=local_index",
                "oss_repo_profile: one-repo brief computed from the snapshot (files, languages, top-level layout)",
                "oss_repo_tree: file map of the indexed snapshot",
                "oss_fetch_file: byte-exact read from the snapshot, prefer line_range",
                "oss_fetch_docs: markdown search inside the snapshot with Source: breadcrumbs",
            ],
            "probe_writing": {
                "good": ["spawn_worker", "special_closes", "x-ratelimit-remaining", "/early[Cc]loses/"],
                "bad": ["market calendars", "rust http library", "error handling"],
                "rule": "search for code, not concepts; uniqueness beats correctness; 2-5 orthogonal probes per call",
            },
            "regex_rules": [
                "RE2-style syntax only: no backreferences, no look-around",
                "a regex must contain a literal run of >= 3 characters to be plannable (trigram index requirement) — 'retry.*backoff' is fine, '[a-z]+' is rejected",
                "in oss_search_code probes, wrap regexes in slashes: '/early[Cc]loses/'; regex probes are case-sensitive against the local index, literal probes are case-insensitive",
            ],
            "local_limits": "the hot-set only knows what was indexed: stars, license, release cadence and anything outside the snapshot require --live",
            "rate_limit_traps": "no network is consulted; an empty result here is a true negative for the snapshot, not a fetch failure",
            "license_classes": {
                "permissive": "MIT/Apache-2.0/BSD — safe to vendor",
                "copyleft": "GPL family — pattern-only study, linking has obligations",
                "AGPL": "network copyleft — treat as toxic for SaaS embedding",
            },
            "response_budget": "default calls stay under ~25KB; truncated=true means results were dropped to fit — narrow the query or page with next_cursor",
        });
        Ok(EngineOutput::complete(vec![row]))
    }
}

/// Hot-set + live federation: local hot-set results first (attributed
/// `local_index`), merged and deduped with remote backend results. Repo
/// tree/file reads prefer the snapshot and fall back to the remote backend
/// on a local miss; discovery, profiles and hosted docs go to the remote
/// engine, which is the only place that metadata exists.
pub struct MergedEngine {
    hotset: HotsetEngine,
    live: LiveEngine,
}

impl MergedEngine {
    pub fn new(hotset: HotsetEngine, live: LiveEngine) -> Self {
        Self { hotset, live }
    }

    pub fn hotset(&self) -> &HotsetEngine {
        &self.hotset
    }
}

fn row_key(row: &Value, mode: CodeSearchMode) -> String {
    let repo = row["repo"].as_str().unwrap_or("");
    match mode {
        CodeSearchMode::Repos => repo.to_string(),
        CodeSearchMode::Snippets => {
            let path = row["path"].as_str().unwrap_or("");
            format!("{repo}\u{0}{path}")
        }
    }
}

/// Merge local-first rows with remote rows, deduping by row key and
/// unioning backend attribution into the local row.
fn merge_local_remote(mut rows: Vec<Value>, remote: Vec<Value>, mode: CodeSearchMode) -> Vec<Value> {
    let mut seen: HashSet<String> = rows.iter().map(|r| row_key(r, mode)).collect();
    for r in remote {
        let key = row_key(&r, mode);
        if seen.contains(&key) {
            if let Some(existing) = rows.iter_mut().find(|e| row_key(e, mode) == key) {
                union_into(existing, &r);
            }
        } else {
            seen.insert(key);
            rows.push(r);
        }
    }
    rows
}

fn union_into(local: &mut Value, remote: &Value) {
    let mut backends: Vec<Value> = local["backends"].as_array().cloned().unwrap_or_default();
    for b in remote["backends"].as_array().unwrap_or(&Vec::new()) {
        if !backends.contains(b) {
            backends.push(b.clone());
        }
    }
    if !backends.is_empty() {
        local["backends"] = Value::Array(backends);
    }
    let mut probes: Vec<Value> = local["probes_hit"].as_array().cloned().unwrap_or_default();
    for p in remote["probes_hit"].as_array().unwrap_or(&Vec::new()) {
        if !probes.contains(p) {
            probes.push(p.clone());
        }
    }
    if !probes.is_empty() {
        local["probes_hit"] = Value::Array(probes.clone());
        local["probe_breadth"] = json!(probes.len());
    }
    if local["sample_evidence"].is_null() && !remote["sample_evidence"].is_null() {
        local["sample_evidence"] = remote["sample_evidence"].clone();
    }
    if local["hit_files"].is_u64() && remote["hit_files"].is_u64() {
        local["hit_files"] =
            json!(local["hit_files"].as_u64().unwrap_or(0) + remote["hit_files"].as_u64().unwrap_or(0));
    }
}

impl SearchEngine for MergedEngine {
    fn backend_status(&self, tool: &str) -> Vec<BackendStatus> {
        let mut statuses = self.hotset.backend_status(tool);
        statuses.extend(self.live.backend_status(tool));
        statuses
    }

    fn search_code(&self, params: &SearchCodeParams) -> Result<EngineOutput, ToolError> {
        // The same cursor offset is forwarded to both engines; the merged
        // page is local-first, deduped, then windowed to the limit.
        let local = self.hotset.search_code(params)?;
        let remote = self.live.search_code(params)?;
        let offset = cursor_base_offset(&params.cursor);
        let mut rows = merge_local_remote(local.results, remote.results, params.mode);
        let page_len = (params.limit as usize).min(rows.len());
        rows.truncate(page_len);
        let has_more = local.has_more || remote.has_more;
        Ok(EngineOutput {
            total: offset + page_len as u64,
            results: rows,
            has_more,
            partial: local.partial || remote.partial,
            next_cursor: has_more.then(|| format!("off:{}", offset + page_len as u64)),
        })
    }

    fn search_repos(&self, params: &SearchReposParams) -> Result<EngineOutput, ToolError> {
        self.live.search_repos(params)
    }

    fn repo_profile(&self, params: &RepoProfileParams) -> Result<EngineOutput, ToolError> {
        let mut out = self.live.repo_profile(params)?;
        if let Some(row) = out.results.first_mut()
            && let Some((repo, docs)) = self.hotset.find_repo(&params.repo)
        {
            row["hotset"] = json!({
                "repo": repo,
                "file_count": docs.len(),
                "index_dir": self.hotset.index_dir.display().to_string(),
            });
        }
        Ok(out)
    }

    fn repo_tree(&self, params: &RepoTreeParams) -> Result<EngineOutput, ToolError> {
        match self.hotset.repo_tree(params) {
            Ok(out) => Ok(out),
            Err(ToolError::NotFound { .. }) => self.live.repo_tree(params),
            Err(e) => Err(e),
        }
    }

    fn fetch_file(&self, params: &FetchFileParams) -> Result<EngineOutput, ToolError> {
        match self.hotset.fetch_file(params) {
            Ok(out) => Ok(out),
            Err(ToolError::NotFound { .. }) => self.live.fetch_file(params),
            Err(e) => Err(e),
        }
    }

    fn fetch_docs(&self, params: &FetchDocsParams) -> Result<EngineOutput, ToolError> {
        self.live.fetch_docs(params)
    }

    fn guide(&self, params: &GuideParams) -> Result<EngineOutput, ToolError> {
        let mut out = self.live.guide(params)?;
        if let Some(row) = out.results.first_mut() {
            row["engine"] = json!("hotset+live");
            row["hotset"] = self.hotset.hotset_guide_section(
                "local hot-set results are merged first (backend local_index), then live backends; repo tree/file reads prefer the snapshot and fall back to remote on a miss"
            );
            if let Some(when) = row["which_tool_when"].as_array_mut() {
                for entry in when.iter_mut() {
                    if let Some(s) = entry.as_str() {
                        if s.starts_with("oss_search_code:") {
                            *entry = json!("oss_search_code: vetting — local hot-set first (backend local_index), federated with github + grep.app, merged with per-backend attribution");
                        } else if s.starts_with("oss_search_repos:") {
                            *entry = json!("oss_search_repos: discovery via live backends (npms.io + ecosyste.ms), annotated with hot-set file counts when the repo is indexed locally");
                        }
                    }
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oss_core::call_tool;

    const NEEDLE: &str = "hotset_fixture_needle";

    fn fixture_dir() -> (tempfile::TempDir, HotsetEngine) {
        let tmp = tempfile::tempdir().unwrap();
        let index = TrigramIndex::build(vec![
            (
                "fixture/hotset-demo".into(),
                "src/lib.rs".into(),
                format!("pub fn {NEEDLE}() -> u32 {{ 7 }}\nfn other() {{}}\n").into_bytes(),
            ),
            (
                "fixture/hotset-demo".into(),
                "README.md".into(),
                format!("# hotset demo\nDocs mention {NEEDLE} usage.\n").into_bytes(),
            ),
            (
                "fixture/plain".into(),
                "src/other.rs".into(),
                b"fn unrelated_thing() {}\n".to_vec(),
            ),
        ])
        .unwrap();
        index.save(&tmp.path().join("index")).unwrap();
        let engine = HotsetEngine::open(&tmp.path().join("index")).unwrap();
        assert_eq!(engine.doc_count(), 3);
        (tmp, engine)
    }

    #[test]
    fn open_missing_dir_is_error() {
        let tmp = tempfile::tempdir().unwrap();
        let err = HotsetEngine::open(&tmp.path().join("nope")).unwrap_err();
        assert!(err.contains("manifest"), "error was: {err}");
    }

    #[test]
    fn open_rejects_unknown_manifest_version() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("index");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest.json"), br#"{"version": 99, "docs": 0}"#).unwrap();
        let err = HotsetEngine::open(&dir).unwrap_err();
        assert!(err.contains("version 99"), "error was: {err}");
    }

    #[test]
    fn home_convention_env_overrides_cache_default() {
        let env = PathBuf::from("/tmp/custom-home");
        assert_eq!(home_from(Some(env.as_os_str())), env);
        let unset = home_from(None);
        assert!(unset.ends_with(".cache/oss-search"), "was {unset:?}");
        let empty = home_from(Some(std::ffi::OsStr::new("")));
        assert!(empty.ends_with(".cache/oss-search"), "was {empty:?}");
        assert_eq!(default_index_dir(), default_home().join("index"));
    }

    #[test]
    fn search_code_attributed_local_index() {
        let (_tmp, engine) = fixture_dir();
        let out = call_tool(
            &engine,
            "oss_search_code",
            Some(json!({"probes": [NEEDLE], "response_format": "detailed"})),
        )
        .unwrap();
        assert_eq!(out["total"], 1);
        let row = &out["results"][0];
        assert_eq!(row["repo"], "fixture/hotset-demo");
        assert_eq!(row["probe_breadth"], 1);
        assert_eq!(row["hit_files"], 2);
        let backends: Vec<&str> = row["backends"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|b| b.as_str())
            .collect();
        assert_eq!(backends, vec!["local_index"]);
        assert!(row["sample_evidence"]
            .as_str()
            .unwrap()
            .contains("README.md:2"));
        let status = &out["backend_status"][0];
        assert_eq!(status["backend"], "local_index");
        assert_eq!(status["status"], "ok");
        assert!(status["detail"].as_str().unwrap().contains("3 docs"));
    }

    #[test]
    fn search_code_negative_is_true_negative() {
        let (_tmp, engine) = fixture_dir();
        let out = call_tool(
            &engine,
            "oss_search_code",
            Some(json!({"probes": ["definitely_absent_probe"]})),
        )
        .unwrap();
        assert_eq!(out["total"], 0);
        assert!(out["results"].as_array().unwrap().is_empty());
        assert_eq!(out["partial"], false);
    }

    #[test]
    fn search_code_case_insensitive_literal_default() {
        let (_tmp, engine) = fixture_dir();
        let out = call_tool(
            &engine,
            "oss_search_code",
            Some(json!({"probes": ["HOTSET_FIXTURE_NEEDLE"]})),
        )
        .unwrap();
        assert_eq!(out["total"], 1);
    }

    #[test]
    fn snippets_mode_line_locators() {
        let (_tmp, engine) = fixture_dir();
        let out = call_tool(
            &engine,
            "oss_search_code",
            Some(json!({"probes": [NEEDLE], "mode": "snippets", "response_format": "detailed"})),
        )
        .unwrap();
        let rows = out["results"].as_array().unwrap();
        assert!(!rows.is_empty());
        let row = rows
            .iter()
            .find(|r| r["path"] == "src/lib.rs")
            .unwrap_or_else(|| panic!("no src/lib.rs snippet row: {rows:?}"));
        assert!(
            row["locator"].as_str().unwrap().starts_with("src/lib.rs:L"),
            "locator: {}",
            row["locator"]
        );
        assert_eq!(row["probe"], NEEDLE);
        assert!(!row["lines"].as_array().unwrap().is_empty());
        assert_eq!(row["backends"][0], "local_index");
    }

    #[test]
    fn regex_probe_via_slashes() {
        let (_tmp, engine) = fixture_dir();
        let out = call_tool(
            &engine,
            "oss_search_code",
            Some(json!({"probes": ["/hotset_[a-z_]+_needle/"], "response_format": "detailed"})),
        )
        .unwrap();
        assert_eq!(out["total"], 1);
        assert_eq!(out["results"][0]["repo"], "fixture/hotset-demo");
    }

    #[test]
    fn non_indexable_regex_is_typed_error() {
        let (_tmp, engine) = fixture_dir();
        let err = call_tool(
            &engine,
            "oss_search_code",
            Some(json!({"probes": ["/[a-z]+/"]})),
        )
        .unwrap_err();
        assert_eq!(err.code(), "invalid_value");
        assert!(err.to_json()["error"]["correction"]
            .as_str()
            .unwrap()
            .contains("oss_guide"));
    }

    #[test]
    fn filters_repo_and_lang() {
        let (_tmp, engine) = fixture_dir();
        let out = call_tool(
            &engine,
            "oss_search_code",
            Some(json!({"probes": [NEEDLE], "repo_filter": "fixture/plain"})),
        )
        .unwrap();
        assert_eq!(out["total"], 0);
        let out = call_tool(
            &engine,
            "oss_search_code",
            Some(json!({"probes": [NEEDLE], "language": "python"})),
        )
        .unwrap();
        assert_eq!(out["total"], 0);
        let out = call_tool(
            &engine,
            "oss_search_code",
            Some(json!({"probes": [NEEDLE], "language": "rust"})),
        )
        .unwrap();
        assert_eq!(out["total"], 1);
    }

    #[test]
    fn repo_tree_lists_snapshot() {
        let (_tmp, engine) = fixture_dir();
        let out = call_tool(
            &engine,
            "oss_repo_tree",
            Some(json!({"repo": "fixture/hotset-demo"})),
        )
        .unwrap();
        let paths: Vec<&str> = out["results"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|p| p["path"].as_str())
            .collect();
        assert_eq!(paths, vec!["README.md", "src/lib.rs"]);
        let out = call_tool(
            &engine,
            "oss_repo_tree",
            Some(json!({"repo": "fixture/hotset-demo", "path_prefix": "src/"})),
        )
        .unwrap();
        let paths: Vec<&str> = out["results"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|p| p["path"].as_str())
            .collect();
        assert_eq!(paths, vec!["src/lib.rs"]);
    }

    #[test]
    fn repo_tree_unknown_repo_is_typed_not_found() {
        let (_tmp, engine) = fixture_dir();
        let err = call_tool(
            &engine,
            "oss_repo_tree",
            Some(json!({"repo": "nope/missing"})),
        )
        .unwrap_err();
        assert_eq!(err.code(), "not_found");
        assert!(err.to_json()["error"]["correction"]
            .as_str()
            .unwrap()
            .contains("hot-set"));
    }

    #[test]
    fn fetch_file_serves_snapshot_lines() {
        let (_tmp, engine) = fixture_dir();
        let out = call_tool(
            &engine,
            "oss_fetch_file",
            Some(json!({"repo": "fixture/hotset-demo", "path": "src/lib.rs", "line_range": {"start": 1, "end": 1}})),
        )
        .unwrap();
        let row = &out["results"][0];
        assert_eq!(row["ref"], "hotset");
        assert_eq!(row["total_lines"], 2);
        assert!(row["content"].as_str().unwrap().contains(NEEDLE));
        assert_eq!(row["source"], "local_index");
    }

    #[test]
    fn fetch_file_missing_is_typed_not_found() {
        let (_tmp, engine) = fixture_dir();
        let err = call_tool(
            &engine,
            "oss_fetch_file",
            Some(json!({"repo": "fixture/hotset-demo", "path": "src/guessed.rs"})),
        )
        .unwrap_err();
        assert_eq!(err.code(), "not_found");
        assert!(err.to_json()["error"]["correction"]
            .as_str()
            .unwrap()
            .contains("oss_repo_tree"));
    }

    #[test]
    fn fetch_docs_markdown_with_breadcrumbs() {
        let (_tmp, engine) = fixture_dir();
        let out = call_tool(
            &engine,
            "oss_fetch_docs",
            Some(json!({"repo": "fixture/hotset-demo", "pattern": "usage"})),
        )
        .unwrap();
        let row = &out["results"][0];
        assert_eq!(row["page"], "README.md");
        assert!(row["hits"][0]["breadcrumb"]
            .as_str()
            .unwrap()
            .starts_with("Source: https://github.com/"));
    }

    #[test]
    fn search_repos_local_rollup() {
        let (_tmp, engine) = fixture_dir();
        let out = call_tool(
            &engine,
            "oss_search_repos",
            Some(json!({"query": "hotset-demo"})),
        )
        .unwrap();
        let row = &out["results"][0];
        assert_eq!(row["repo"], "fixture/hotset-demo");
        assert_eq!(row["hotset_file_count"], 2);
        assert_eq!(row["hotset"], true);
        let out = call_tool(
            &engine,
            "oss_search_repos",
            Some(json!({"query": "does-not-exist"})),
        )
        .unwrap();
        assert_eq!(out["total"], 0);
    }

    #[test]
    fn repo_profile_from_snapshot() {
        let (_tmp, engine) = fixture_dir();
        let out = call_tool(
            &engine,
            "oss_repo_profile",
            Some(json!({"repo": "fixture/hotset-demo", "deep_docs": true})),
        )
        .unwrap();
        let row = &out["results"][0];
        assert!(row["what"].as_str().unwrap().contains("2 indexed files"));
        let dirs: Vec<&str> = row["architecture_top_level"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|d| d.as_str())
            .collect();
        assert_eq!(dirs, vec!["src"]);
        assert!(row["deep_docs"]["digest"]
            .as_str()
            .unwrap()
            .contains("hotset demo"));
        assert!(row["catch"].as_str().unwrap().contains("--live"));
    }

    #[test]
    fn guide_reports_index_stats() {
        let (_tmp, engine) = fixture_dir();
        let out = call_tool(&engine, "oss_guide", None).unwrap();
        let row = &out["results"][0];
        assert_eq!(row["engine"], "hotset");
        assert_eq!(row["hotset"]["status"], "served");
        assert_eq!(row["hotset"]["docs"], 3);
        assert!(row["hotset"]["index_dir"].as_str().unwrap().contains("index"));
    }

    #[test]
    fn merged_engine_local_first_when_remote_down() {
        let (_tmp, engine) = fixture_dir();
        // Point every remote backend at a port that refuses connections:
        // remote answers Unavailable, local results must still be served
        // first with local_index attribution and a truthful partial flag.
        let live = crate::LiveEngine::with_config(crate::LiveEngineConfig::all_at(
            "http://127.0.0.1:9",
        ));
        let merged = MergedEngine::new(engine, live);
        let out = call_tool(
            &merged,
            "oss_search_code",
            Some(json!({"probes": [NEEDLE], "response_format": "detailed"})),
        )
        .unwrap();
        assert_eq!(out["partial"], true, "remote down must mark partial");
        let row = &out["results"][0];
        assert_eq!(row["repo"], "fixture/hotset-demo");
        let backends: Vec<&str> = row["backends"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|b| b.as_str())
            .collect();
        assert_eq!(backends, vec!["local_index"]);
        let names: Vec<&str> = out["backend_status"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|s| s["backend"].as_str())
            .collect();
        assert_eq!(names.first(), Some(&"local_index"));
        let guide = call_tool(&merged, "oss_guide", None).unwrap();
        assert_eq!(guide["results"][0]["engine"], "hotset+live");
        assert_eq!(guide["results"][0]["hotset"]["docs"], 3);
    }

    #[test]
    fn merged_tree_prefers_snapshot_then_remote() {
        let (_tmp, engine) = fixture_dir();
        let live = crate::LiveEngine::with_config(crate::LiveEngineConfig::all_at(
            "http://127.0.0.1:9",
        ));
        let merged = MergedEngine::new(engine, live);
        // Snapshot hit: served from disk even though the remote is down.
        let out = call_tool(
            &merged,
            "oss_repo_tree",
            Some(json!({"repo": "fixture/hotset-demo"})),
        )
        .unwrap();
        assert_eq!(out["results"].as_array().unwrap().len(), 2);
        // Snapshot miss falls back to remote, which is down -> degraded
        // empty result, not a faked negative and not a crash.
        let out = call_tool(
            &merged,
            "oss_repo_tree",
            Some(json!({"repo": "octocat/spoon-knife"})),
        )
        .unwrap();
        assert_eq!(out["partial"], true);
        assert!(out["results"].as_array().unwrap().is_empty());
    }
}
