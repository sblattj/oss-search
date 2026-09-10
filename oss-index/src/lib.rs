use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum IndexError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("regex error: {0}")]
    Regex(#[from] regex::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("pattern is not indexable: {0}")]
    NotIndexable(String),
    #[error("index error: {0}")]
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocMeta {
    pub id: u32,
    pub repo: String,
    pub path: String,
    pub lang: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LineMatch {
    pub line_no: u32,
    pub line_text: String,
    pub spans: Vec<(u32, u32)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScoreComponents {
    pub whole_word: f32,
    pub case_exact: f32,
    pub definition_path: f32,
    pub test_path: f32,
    pub filename_match: f32,
    pub path_length: f32,
    pub density: f32,
}

impl ScoreComponents {
    pub fn total(&self) -> f32 {
        self.whole_word
            + self.case_exact
            + self.definition_path
            + self.test_path
            + self.filename_match
            + self.path_length
            + self.density
    }

    pub fn as_map(&self) -> BTreeMap<String, f32> {
        let mut m = BTreeMap::new();
        m.insert("whole_word".into(), self.whole_word);
        m.insert("case_exact".into(), self.case_exact);
        m.insert("definition_path".into(), self.definition_path);
        m.insert("test_path".into(), self.test_path);
        m.insert("filename_match".into(), self.filename_match);
        m.insert("path_length".into(), self.path_length);
        m.insert("density".into(), self.density);
        m
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchResult {
    pub repo: String,
    pub path: String,
    pub matches: Vec<LineMatch>,
    pub score: f32,
    pub score_components: ScoreComponents,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SearchOutcome {
    pub results: Vec<SearchResult>,
    pub truncated: bool,
    pub candidates_scanned: u64,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Default)]
pub struct UpdateStats {
    pub docs_reindexed: u64,
    pub postings_touched: u64,
}

pub const DEFAULT_MAX_RESULTS: usize = 50;
pub const MAX_FILE_BYTES: usize = 1_048_576;
const MAX_LINE_LEN: usize = 500;

#[derive(Debug, Clone, Default)]
pub struct Filters {
    pub repos: Vec<String>,
    pub exclude_repos: Vec<String>,
    pub langs: Vec<String>,
    pub path_contains: Vec<String>,
    pub exclude_paths: Vec<String>,
}

impl Filters {
    pub fn allows(&self, doc: &DocMeta) -> bool {
        if !self.repos.is_empty() && !self.repos.iter().any(|r| doc.repo.contains(r)) {
            return false;
        }
        if self.exclude_repos.iter().any(|r| doc.repo.contains(r)) {
            return false;
        }
        if !self.langs.is_empty() && !self.langs.iter().any(|l| l == &doc.lang) {
            return false;
        }
        if !self.path_contains.is_empty()
            && !self.path_contains.iter().any(|p| doc.path.contains(p))
        {
            return false;
        }
        if self.exclude_paths.iter().any(|p| doc.path.contains(p)) {
            return false;
        }
        true
    }
}

pub fn detect_lang(path: &str) -> String {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "rs" => "rust",
        "py" => "python",
        "js" | "mjs" | "cjs" => "javascript",
        "ts" | "tsx" => "typescript",
        "go" => "go",
        "c" | "h" => "c",
        "cpp" | "cc" | "hpp" => "cpp",
        "java" => "java",
        "rb" => "ruby",
        "md" => "markdown",
        "toml" => "toml",
        "json" => "json",
        "yaml" | "yml" => "yaml",
        "sh" => "shell",
        _ => "text",
    }
    .to_string()
}

pub fn admitted(path: &str, bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err(format!(
            "file too large: {} bytes > {} cap",
            bytes.len(),
            MAX_FILE_BYTES
        ));
    }
    let head = &bytes[..bytes.len().min(8192)];
    if head.contains(&0) {
        return Err(format!("binary content (NUL byte) in {path}"));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct Trigram(u32);

fn trigrams_of(bytes: &[u8]) -> Vec<Trigram> {
    let mut out = Vec::with_capacity(bytes.len().saturating_sub(2));
    for w in bytes.windows(3) {
        let v = (w[0] as u32) << 16 | (w[1] as u32) << 8 | w[2] as u32;
        out.push(Trigram(v));
    }
    out
}

fn push_uvarint(buf: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            buf.push(b);
            return;
        }
        buf.push(b | 0x80);
    }
}

fn read_uvarint(buf: &[u8], pos: &mut usize) -> Option<u64> {
    let mut v: u64 = 0;
    let mut shift = 0;
    loop {
        let b = *buf.get(*pos)?;
        *pos += 1;
        v |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Some(v);
        }
        shift += 7;
        if shift > 63 {
            return None;
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
struct PostingList {
    entries: BTreeMap<u32, Vec<u32>>,
}

impl PostingList {
    fn add(&mut self, doc: u32, offset: u32) {
        self.entries.entry(doc).or_default().push(offset);
    }

    fn remove_doc(&mut self, doc: u32) -> bool {
        self.entries.remove(&doc).is_some()
    }

    fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        push_uvarint(&mut buf, self.entries.len() as u64);
        let mut last_doc = 0u32;
        for (doc, offsets) in &self.entries {
            push_uvarint(&mut buf, (*doc - last_doc) as u64);
            last_doc = *doc;
            push_uvarint(&mut buf, offsets.len() as u64);
            let mut last_off = 0u32;
            for off in offsets {
                push_uvarint(&mut buf, (*off - last_off) as u64);
                last_off = *off;
            }
        }
        buf
    }

    fn decode(buf: &[u8]) -> Option<PostingList> {
        let mut pos = 0usize;
        let ndocs = read_uvarint(buf, &mut pos)?;
        let mut entries = BTreeMap::new();
        let mut last_doc = 0u32;
        for _ in 0..ndocs {
            let delta = read_uvarint(buf, &mut pos)?;
            let doc = last_doc.checked_add(delta as u32)?;
            last_doc = doc;
            let noffs = read_uvarint(buf, &mut pos)?;
            let mut offsets = Vec::with_capacity(noffs as usize);
            let mut last_off = 0u32;
            for _ in 0..noffs {
                let d = read_uvarint(buf, &mut pos)?;
                let off = last_off.checked_add(d as u32)?;
                last_off = off;
                offsets.push(off);
            }
            entries.insert(doc, offsets);
        }
        Some(PostingList { entries })
    }
}

#[derive(Debug, Clone)]
struct StoredDoc {
    meta: DocMeta,
    content: Vec<u8>,
    ngrams: u32,
}

type ExtractedDoc = (usize, (String, String), Vec<u8>, Vec<(u32, u32)>);

pub struct TrigramIndex {
    docs: HashMap<u32, StoredDoc>,
    key_to_id: HashMap<(String, String), u32>,
    postings: HashMap<u32, PostingList>,
    next_id: u32,
}

impl TrigramIndex {
    pub fn in_memory() -> Self {
        TrigramIndex {
            docs: HashMap::new(),
            key_to_id: HashMap::new(),
            postings: HashMap::new(),
            next_id: 0,
        }
    }

    pub fn build<I>(docs: I) -> Result<Self, IndexError>
    where
        I: IntoIterator<Item = (String, String, Vec<u8>)>,
    {
        let mut idx = Self::in_memory();
        for (repo, path, bytes) in docs {
            idx.add_doc(&repo, &path, bytes)?;
        }
        Ok(idx)
    }

    pub fn build_parallel<I>(docs: I) -> Result<Self, IndexError>
    where
        I: IntoIterator<Item = (String, String, Vec<u8>)>,
    {
        let docs: Vec<(String, String, Vec<u8>)> = docs.into_iter().collect();
        let extracted: Vec<ExtractedDoc> = docs
            .into_par_iter()
            .enumerate()
            .map(|(i, (repo, path, bytes))| {
                let grams = trigrams_of(&bytes);
                let positioned: Vec<(u32, u32)> = grams
                    .iter()
                    .enumerate()
                    .map(|(off, t)| (t.0, off as u32))
                    .collect();
                (i, (repo, path), bytes, positioned)
            })
            .collect();
        let mut idx = Self::in_memory();
        for (_, (repo, path), bytes, positioned) in extracted {
            if admitted(&path, &bytes).is_err() {
                continue;
            }
            let id = idx.alloc_id(&repo, &path, bytes, 0);
            let ngrams = positioned.len() as u32;
            idx.docs.get_mut(&id).unwrap().ngrams = ngrams;
            for (gram, off) in positioned {
                idx.postings.entry(gram).or_default().add(id, off);
            }
        }
        Ok(idx)
    }

    fn alloc_id(&mut self, repo: &str, path: &str, content: Vec<u8>, ngrams: u32) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        let meta = DocMeta {
            id,
            repo: repo.to_string(),
            path: path.to_string(),
            lang: detect_lang(path),
        };
        self.key_to_id
            .insert((repo.to_string(), path.to_string()), id);
        self.docs.insert(
            id,
            StoredDoc {
                meta,
                content,
                ngrams,
            },
        );
        id
    }

    pub fn add_doc(
        &mut self,
        repo: &str,
        path: &str,
        bytes: Vec<u8>,
    ) -> Result<Option<u32>, IndexError> {
        if admitted(path, &bytes).is_err() {
            return Ok(None);
        }
        let grams = trigrams_of(&bytes);
        let id = self.alloc_id(repo, path, bytes, grams.len() as u32);
        for (off, t) in grams.iter().enumerate() {
            self.postings.entry(t.0).or_default().add(id, off as u32);
        }
        Ok(Some(id))
    }

    pub fn update_doc(
        &mut self,
        repo: &str,
        path: &str,
        bytes: Vec<u8>,
    ) -> Result<UpdateStats, IndexError> {
        let mut stats = UpdateStats::default();
        let key = (repo.to_string(), path.to_string());
        if let Some(&old_id) = self.key_to_id.get(&key) {
            stats.postings_touched += self.remove_doc_postings(old_id) as u64;
            self.docs.remove(&old_id);
            self.key_to_id.remove(&key);
            stats.docs_reindexed += 1;
        }
        if admitted(path, &bytes).is_ok() {
            let grams = trigrams_of(&bytes);
            let id = self.alloc_id(repo, path, bytes, grams.len() as u32);
            for (off, t) in grams.iter().enumerate() {
                self.postings.entry(t.0).or_default().add(id, off as u32);
                stats.postings_touched += 1;
            }
        }
        Ok(stats)
    }

    fn remove_doc_postings(&mut self, id: u32) -> usize {
        let mut touched = 0;
        let doc = match self.docs.get(&id) {
            Some(d) => d,
            None => return 0,
        };
        let grams: HashSet<u32> = trigrams_of(&doc.content).iter().map(|t| t.0).collect();
        for gram in grams {
            if let Some(pl) = self.postings.get_mut(&gram) {
                if pl.remove_doc(id) {
                    touched += 1;
                }
            }
        }
        touched
    }

    pub fn remove_repo(&mut self, repo: &str) -> UpdateStats {
        let ids: Vec<u32> = self
            .docs
            .values()
            .filter(|d| d.meta.repo == repo)
            .map(|d| d.meta.id)
            .collect();
        let mut stats = UpdateStats::default();
        for id in ids {
            stats.postings_touched += self.remove_doc_postings(id) as u64;
            if let Some(d) = self.docs.remove(&id) {
                self.key_to_id
                    .remove(&(d.meta.repo.clone(), d.meta.path.clone()));
                stats.docs_reindexed += 1;
            }
        }
        stats
    }

    pub fn doc_count(&self) -> usize {
        self.docs.len()
    }

    pub fn search_literal(
        &self,
        pattern: &str,
        filters: &Filters,
        max: usize,
    ) -> Result<SearchOutcome, IndexError> {
        let start = std::time::Instant::now();
        let escaped = regex::escape(pattern);
        self.run_regex(&escaped, pattern, true, filters, max, start)
    }

    pub fn search_literal_case_sensitive(
        &self,
        pattern: &str,
        filters: &Filters,
        max: usize,
    ) -> Result<SearchOutcome, IndexError> {
        let start = std::time::Instant::now();
        let escaped = regex::escape(pattern);
        self.run_regex(&escaped, pattern, false, filters, max, start)
    }

    pub fn search_regex(
        &self,
        pattern: &str,
        filters: &Filters,
        max: usize,
    ) -> Result<SearchOutcome, IndexError> {
        let start = std::time::Instant::now();
        self.run_regex(pattern, pattern, false, filters, max, start)
    }

    fn run_regex(
        &self,
        regex_src: &str,
        display_pattern: &str,
        case_insensitive: bool,
        filters: &Filters,
        max: usize,
        start: std::time::Instant,
    ) -> Result<SearchOutcome, IndexError> {
        let required = extract_required_trigrams(regex_src);
        if required.is_none() && !self.docs.is_empty() {
            return Err(IndexError::NotIndexable(format!(
                "pattern `{display_pattern}` yields no required trigram set"
            )));
        }
        let re_src = if case_insensitive {
            format!("(?i){regex_src}")
        } else {
            regex_src.to_string()
        };
        let re = regex::Regex::new(&re_src)?;

        let candidate_ids: Vec<u32> = match &required {
            Some(sets) => self.intersect_postings(&expand_case_variants(sets, case_insensitive)),
            None => self.docs.keys().copied().collect(),
        };

        let mut results = Vec::new();
        let mut scanned = 0u64;
        let mut truncated = false;
        for id in candidate_ids {
            let doc = match self.docs.get(&id) {
                Some(d) => d,
                None => continue,
            };
            if !filters.allows(&doc.meta) {
                continue;
            }
            scanned += 1;
            let content = match std::str::from_utf8(&doc.content) {
                Ok(s) => s,
                Err(_) => continue,
            };
            let matches = find_line_matches(&re, content);
            if matches.is_empty() {
                continue;
            }
            let score_components =
                score_doc(&doc.meta, &matches, display_pattern, case_insensitive);
            let score = score_components.total();
            results.push(SearchResult {
                repo: doc.meta.repo.clone(),
                path: doc.meta.path.clone(),
                matches,
                score,
                score_components,
            });
        }
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.path.cmp(&b.path))
        });
        if results.len() > max {
            results.truncate(max);
            truncated = true;
        }
        Ok(SearchOutcome {
            results,
            truncated,
            candidates_scanned: scanned,
            elapsed_ms: start.elapsed().as_millis() as u64,
        })
    }

    fn intersect_postings(&self, sets: &[Vec<u32>]) -> Vec<u32> {
        let mut acc: Option<HashSet<u32>> = None;
        for alternatives in sets {
            let mut ids: HashSet<u32> = HashSet::new();
            for gram in alternatives {
                if let Some(pl) = self.postings.get(gram) {
                    ids.extend(pl.entries.keys().copied());
                }
            }
            acc = Some(match acc {
                None => ids,
                Some(prev) => prev.intersection(&ids).copied().collect(),
            });
        }
        let mut v: Vec<u32> = acc.unwrap_or_default().into_iter().collect();
        v.sort_unstable();
        v
    }

    pub fn save(&self, dir: &Path) -> Result<(), IndexError> {
        std::fs::create_dir_all(dir)?;
        let manifest = serde_json::json!({ "version": 1, "docs": self.docs.len() });
        std::fs::write(dir.join("manifest.json"), serde_json::to_vec(&manifest)?)?;
        let mut meta_rows = Vec::new();
        for doc in self.docs.values() {
            meta_rows.push(doc.meta.clone());
            let doc_path = dir.join(format!("doc_{}.bin", doc.meta.id));
            std::fs::write(doc_path, &doc.content)?;
        }
        std::fs::write(dir.join("docs.json"), serde_json::to_vec(&meta_rows)?)?;
        let mut posting_files = HashMap::new();
        for (gram, pl) in &self.postings {
            posting_files.insert(gram.to_string(), pl.encode());
        }
        let mut postings_bin = Vec::new();
        push_uvarint(&mut postings_bin, posting_files.len() as u64);
        for (gram, bytes) in &posting_files {
            let g: u32 = gram
                .parse()
                .map_err(|e| IndexError::Other(format!("bad gram: {e}")))?;
            push_uvarint(&mut postings_bin, g as u64);
            push_uvarint(&mut postings_bin, bytes.len() as u64);
            postings_bin.extend_from_slice(bytes);
        }
        std::fs::write(dir.join("postings.bin"), postings_bin)?;
        Ok(())
    }

    pub fn load(dir: &Path) -> Result<Self, IndexError> {
        let metas: Vec<DocMeta> = serde_json::from_slice(&std::fs::read(dir.join("docs.json"))?)?;
        let mut docs = HashMap::new();
        let mut key_to_id = HashMap::new();
        let mut next_id = 0u32;
        for meta in metas {
            let content = std::fs::read(dir.join(format!("doc_{}.bin", meta.id)))?;
            let ngrams = content.len().saturating_sub(2) as u32;
            key_to_id.insert((meta.repo.clone(), meta.path.clone()), meta.id);
            next_id = next_id.max(meta.id + 1);
            docs.insert(
                meta.id,
                StoredDoc {
                    meta,
                    content,
                    ngrams,
                },
            );
        }
        let postings_bin = std::fs::read(dir.join("postings.bin"))?;
        let mut pos = 0usize;
        let ngrams = read_uvarint(&postings_bin, &mut pos)
            .ok_or_else(|| IndexError::Other("corrupt postings".into()))?;
        let mut postings = HashMap::new();
        for _ in 0..ngrams {
            let gram = read_uvarint(&postings_bin, &mut pos)
                .ok_or_else(|| IndexError::Other("corrupt postings".into()))?;
            let len = read_uvarint(&postings_bin, &mut pos)
                .ok_or_else(|| IndexError::Other("corrupt postings".into()))?;
            let bytes = postings_bin
                .get(pos..pos + len as usize)
                .ok_or_else(|| IndexError::Other("corrupt postings".into()))?;
            let pl = PostingList::decode(bytes)
                .ok_or_else(|| IndexError::Other("corrupt posting list".into()))?;
            postings.insert(gram as u32, pl);
            pos += len as usize;
        }
        Ok(TrigramIndex {
            docs,
            key_to_id,
            postings,
            next_id,
        })
    }
}

fn extract_required_trigrams(regex_src: &str) -> Option<Vec<Vec<u32>>> {
    let lits = extract_literal_runs(regex_src);
    if lits.is_empty() {
        return None;
    }
    let mut sets = Vec::new();
    for run in lits {
        let grams: Vec<u32> = trigrams_of(run.as_bytes()).iter().map(|t| t.0).collect();
        if let Some(first) = grams.first() {
            sets.push(vec![*first]);
        }
    }
    if sets.is_empty() {
        None
    } else {
        Some(sets)
    }
}

fn extract_literal_runs(pattern: &str) -> Vec<String> {
    let mut runs = Vec::new();
    let mut cur = String::new();
    let mut chars = pattern.chars().peekable();
    let mut depth = 0i32;
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(n) = chars.next() {
                    match n {
                        'd' | 's' | 'w' | 'b' | 'D' | 'S' | 'W' | 'B' | 'n' | 't' | 'r' | '0'
                        | 'x' | 'u' | 'p' => {
                            if cur.len() >= 3 && depth == 0 {
                                runs.push(std::mem::take(&mut cur));
                            } else {
                                cur.clear();
                            }
                        }
                        other => {
                            cur.push(other);
                        }
                    }
                }
            }
            '(' => depth += 1,
            ')' => depth = (depth - 1).max(0),
            '[' => {
                let mut d = 1;
                while let Some(c2) = chars.next() {
                    match c2 {
                        '[' => d += 1,
                        ']' => {
                            d -= 1;
                            if d == 0 {
                                break;
                            }
                        }
                        '\\' => {
                            chars.next();
                        }
                        _ => {}
                    }
                }
                if cur.len() >= 3 && depth == 0 {
                    runs.push(std::mem::take(&mut cur));
                } else {
                    cur.clear();
                }
            }
            '.' | '*' | '+' | '?' | '{' | '}' | '|' | '^' | '$' => {
                if cur.len() >= 3 && depth == 0 {
                    runs.push(std::mem::take(&mut cur));
                } else {
                    cur.clear();
                }
            }
            other => cur.push(other),
        }
    }
    if cur.len() >= 3 && depth == 0 {
        runs.push(cur);
    }
    runs
}

fn expand_case_variants(sets: &[Vec<u32>], case_insensitive: bool) -> Vec<Vec<u32>> {
    if !case_insensitive {
        return sets.to_vec();
    }
    sets.iter()
        .map(|alts| {
            let mut out: Vec<u32> = Vec::new();
            for &g in alts {
                let b0 = (g >> 16) as u8;
                let b1 = (g >> 8) as u8;
                let b2 = g as u8;
                if b0.is_ascii_lowercase() && b1.is_ascii_lowercase() && b2.is_ascii_lowercase() {
                    for m0 in [b0, b0.to_ascii_uppercase()] {
                        for m1 in [b1, b1.to_ascii_uppercase()] {
                            for m2 in [b2, b2.to_ascii_uppercase()] {
                                out.push((m0 as u32) << 16 | (m1 as u32) << 8 | m2 as u32);
                            }
                        }
                    }
                } else {
                    out.push(g);
                }
            }
            out.sort_unstable();
            out.dedup();
            out
        })
        .collect()
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn find_line_matches(re: &regex::Regex, content: &str) -> Vec<LineMatch> {
    let mut out = Vec::new();
    let mut line_start = 0usize;
    for (idx, line) in content.lines().enumerate() {
        let spans: Vec<(u32, u32)> = re
            .find_iter(line)
            .map(|m| (m.start() as u32, m.end() as u32))
            .collect();
        if !spans.is_empty() {
            let text = if line.len() > MAX_LINE_LEN {
                let mut end = MAX_LINE_LEN;
                while !line.is_char_boundary(end) {
                    end -= 1;
                }
                format!("{}…", &line[..end])
            } else {
                line.to_string()
            };
            out.push(LineMatch {
                line_no: (idx + 1) as u32,
                line_text: text,
                spans,
            });
        }
        line_start += line.len() + 1;
    }
    let _ = line_start;
    out
}

fn word_boundary_bonus(content_line: &str, spans: &[(u32, u32)]) -> bool {
    spans.iter().all(|&(s, e)| {
        let before_ok = if s == 0 {
            true
        } else {
            !content_line
                .as_bytes()
                .get((s - 1) as usize)
                .map(|b| is_word_byte(*b))
                .unwrap_or(true)
        };
        let after_ok = !content_line
            .as_bytes()
            .get(e as usize)
            .map(|b| is_word_byte(*b))
            .unwrap_or(true);
        before_ok && after_ok
    })
}

fn score_doc(
    meta: &DocMeta,
    matches: &[LineMatch],
    pattern: &str,
    case_insensitive: bool,
) -> ScoreComponents {
    let whole_word = if matches
        .iter()
        .any(|m| word_boundary_bonus(&m.line_text, &m.spans))
    {
        2.0
    } else {
        0.0
    };
    let case_exact = if case_insensitive {
        0.0
    } else if matches.iter().any(|m| {
        m.spans.iter().any(|&(s, e)| {
            // Spans index the original line; `line_text` may be truncated
            // for display (MAX_LINE_LEN), so a span can start past its end.
            let end = (e as usize).min(m.line_text.len());
            m.line_text
                .get(s as usize..end)
                .is_some_and(|t| t == pattern)
        })
    }) {
        1.5
    } else {
        0.0
    };
    let p = meta.path.to_lowercase();
    let definition_path = if p.contains("/src/") || p.starts_with("src/") {
        1.0
    } else {
        0.0
    };
    let test_path = if p.contains("test") || p.contains("spec") {
        -1.0
    } else {
        0.0
    };
    let filename = meta.path.rsplit('/').next().unwrap_or("");
    let filename_stem = filename.split('.').next().unwrap_or("");
    let filename_match = if !pattern.is_empty()
        && filename_stem
            .to_lowercase()
            .contains(&pattern.to_lowercase())
    {
        2.0
    } else {
        0.0
    };
    let path_length = (100.0 / (meta.path.len() as f32 + 20.0)).min(1.0);
    let density = (matches.len() as f32).ln_1p();
    ScoreComponents {
        whole_word,
        case_exact,
        definition_path,
        test_path,
        filename_match,
        path_length,
        density,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(repo: &str, path: &str, content: &str) -> (String, String, Vec<u8>) {
        (repo.into(), path.into(), content.as_bytes().to_vec())
    }

    #[test]
    fn trigram_extraction() {
        let grams = trigrams_of(b"abcd");
        assert_eq!(grams.len(), 2);
        assert_eq!(
            grams[0].0,
            (b'a' as u32) << 16 | (b'b' as u32) << 8 | b'c' as u32
        );
    }

    #[test]
    fn trigram_short_input() {
        assert!(trigrams_of(b"ab").is_empty());
        assert!(trigrams_of(b"").is_empty());
    }

    #[test]
    fn posting_roundtrip() {
        let mut pl = PostingList::default();
        pl.add(1, 10);
        pl.add(1, 20);
        pl.add(5, 0);
        pl.add(1000, 65535);
        let bytes = pl.encode();
        let back = PostingList::decode(&bytes).unwrap();
        assert_eq!(pl, back);
    }

    #[test]
    fn literal_search_finds_match() {
        let idx = TrigramIndex::build(vec![doc("a/b", "src/main.rs", "fn retry() {}\n")]).unwrap();
        let out = idx
            .search_literal("retry", &Filters::default(), 10)
            .unwrap();
        assert_eq!(out.results.len(), 1);
        assert_eq!(out.results[0].matches[0].line_no, 1);
    }

    #[test]
    fn literal_case_insensitive_default() {
        let idx = TrigramIndex::build(vec![doc("a/b", "src/main.rs", "fn RETRY() {}\n")]).unwrap();
        let out = idx
            .search_literal("retry", &Filters::default(), 10)
            .unwrap();
        assert_eq!(out.results.len(), 1);
    }

    #[test]
    fn literal_case_sensitive_misses() {
        let idx = TrigramIndex::build(vec![doc("a/b", "src/main.rs", "fn RETRY() {}\n")]).unwrap();
        let out = idx
            .search_literal_case_sensitive("retry", &Filters::default(), 10)
            .unwrap();
        assert!(out.results.is_empty());
    }

    #[test]
    fn regex_search_with_literal_anchor() {
        let idx = TrigramIndex::build(vec![
            doc("a/b", "src/a.rs", "fn retry_with_backoff() {}\n"),
            doc("a/b", "src/b.rs", "fn reticulate() {}\n"),
        ])
        .unwrap();
        let out = idx
            .search_regex("retry.*backoff", &Filters::default(), 10)
            .unwrap();
        assert_eq!(out.results.len(), 1);
        assert!(out.results[0].path.ends_with("a.rs"));
    }

    #[test]
    fn regex_without_literal_is_not_indexable() {
        let idx = TrigramIndex::build(vec![doc("a/b", "src/a.rs", "hello world\n")]).unwrap();
        let err = idx.search_regex("[a-z]+", &Filters::default(), 10);
        assert!(matches!(err, Err(IndexError::NotIndexable(_))));
    }

    #[test]
    fn filter_by_repo() {
        let idx = TrigramIndex::build(vec![
            doc("org/one", "src/a.rs", "retry logic here\n"),
            doc("org/two", "src/b.rs", "retry logic here\n"),
        ])
        .unwrap();
        let f = Filters {
            repos: vec!["org/one".into()],
            ..Default::default()
        };
        let out = idx.search_literal("retry", &f, 10).unwrap();
        assert_eq!(out.results.len(), 1);
        assert_eq!(out.results[0].repo, "org/one");
    }

    #[test]
    fn filter_exclude_repo() {
        let idx = TrigramIndex::build(vec![
            doc("org/one", "src/a.rs", "retry logic here\n"),
            doc("org/two", "src/b.rs", "retry logic here\n"),
        ])
        .unwrap();
        let f = Filters {
            exclude_repos: vec!["org/one".into()],
            ..Default::default()
        };
        let out = idx.search_literal("retry", &f, 10).unwrap();
        assert_eq!(out.results.len(), 1);
        assert_eq!(out.results[0].repo, "org/two");
    }

    #[test]
    fn filter_by_lang() {
        let idx = TrigramIndex::build(vec![
            doc("a/b", "src/a.rs", "retry logic here\n"),
            doc("a/b", "src/a.py", "retry logic here\n"),
        ])
        .unwrap();
        let f = Filters {
            langs: vec!["python".into()],
            ..Default::default()
        };
        let out = idx.search_literal("retry", &f, 10).unwrap();
        assert_eq!(out.results.len(), 1);
        assert!(out.results[0].path.ends_with(".py"));
    }

    #[test]
    fn filter_path_contains_and_excludes() {
        let idx = TrigramIndex::build(vec![
            doc("a/b", "src/lib.rs", "retry logic here\n"),
            doc("a/b", "tests/t.rs", "retry logic here\n"),
        ])
        .unwrap();
        let f = Filters {
            exclude_paths: vec!["tests/".into()],
            ..Default::default()
        };
        let out = idx.search_literal("retry", &f, 10).unwrap();
        assert_eq!(out.results.len(), 1);
        assert!(out.results[0].path.contains("src/"));
    }

    #[test]
    fn ranking_whole_word_beats_partial() {
        let idx = TrigramIndex::build(vec![
            doc("a/b", "src/x.rs", "let thread = 1;\n"),
            doc("a/b", "src/y.rs", "let pthread_getname = 1;\n"),
        ])
        .unwrap();
        let out = idx
            .search_literal("thread", &Filters::default(), 10)
            .unwrap();
        assert_eq!(out.results.len(), 2);
        assert!(out.results[0].path.ends_with("x.rs"));
        assert!(out.results[0].score > out.results[1].score);
    }

    #[test]
    fn ranking_test_path_penalized() {
        let idx = TrigramIndex::build(vec![
            doc("a/b", "src/impl.rs", "fn backoff() {}\n"),
            doc("a/b", "tests/impl_test.rs", "fn backoff() {}\n"),
        ])
        .unwrap();
        let out = idx
            .search_literal("backoff", &Filters::default(), 10)
            .unwrap();
        assert!(out.results[0].path.contains("src/"));
    }

    #[test]
    fn ranking_filename_match_boosted() {
        let idx = TrigramIndex::build(vec![
            doc("a/b", "src/retry.rs", "// retry docs\n"),
            doc("a/b", "src/otherthing.rs", "// retry docs\n"),
        ])
        .unwrap();
        let out = idx
            .search_literal("retry", &Filters::default(), 10)
            .unwrap();
        assert!(out.results[0].path.ends_with("retry.rs"));
    }

    #[test]
    fn score_components_inspectable() {
        let idx = TrigramIndex::build(vec![doc("a/b", "src/main.rs", "fn retry() {}\n")]).unwrap();
        let out = idx
            .search_literal("retry", &Filters::default(), 10)
            .unwrap();
        let map = out.results[0].score_components.as_map();
        let sum: f32 = map.values().sum();
        assert!((sum - out.results[0].score).abs() < 1e-4);
    }

    #[test]
    fn update_doc_replaces_content() {
        let mut idx =
            TrigramIndex::build(vec![doc("a/b", "src/m.rs", "alpha beta gamma\n")]).unwrap();
        let stats = idx
            .update_doc("a/b", "src/m.rs", b"delta epsilon zeta\n".to_vec())
            .unwrap();
        assert_eq!(stats.docs_reindexed, 1);
        assert!(stats.postings_touched > 0);
        assert!(idx
            .search_literal("alpha", &Filters::default(), 10)
            .unwrap()
            .results
            .is_empty());
        assert_eq!(
            idx.search_literal("delta", &Filters::default(), 10)
                .unwrap()
                .results
                .len(),
            1
        );
    }

    #[test]
    fn update_touches_only_changed_doc() {
        let mut docs = Vec::new();
        for i in 0..100 {
            docs.push(doc(
                "a/b",
                &format!("src/f{i}.rs"),
                &format!("unique_content_word_{i} filler text here\n"),
            ));
        }
        let mut idx = TrigramIndex::build(docs).unwrap();
        let stats = idx
            .update_doc(
                "a/b",
                "src/f7.rs",
                b"unique_content_word_7 changed filler\n".to_vec(),
            )
            .unwrap();
        assert_eq!(stats.docs_reindexed, 1);
        let old_grams: HashSet<u32> = trigrams_of(b"unique_content_word_7 filler text here\n")
            .iter()
            .map(|t| t.0)
            .collect();
        let new_grams: HashSet<u32> = trigrams_of(b"unique_content_word_7 changed filler\n")
            .iter()
            .map(|t| t.0)
            .collect();
        let union: HashSet<_> = old_grams.union(&new_grams).collect();
        assert!(stats.postings_touched <= (union.len() + new_grams.len()) as u64);
    }

    #[test]
    fn remove_repo_clears_everything() {
        let mut idx = TrigramIndex::build(vec![
            doc("a/keep", "src/k.rs", "shared needle text\n"),
            doc("a/drop", "src/d.rs", "shared needle text\n"),
        ])
        .unwrap();
        let stats = idx.remove_repo("a/drop");
        assert_eq!(stats.docs_reindexed, 1);
        let out = idx
            .search_literal("needle", &Filters::default(), 10)
            .unwrap();
        assert_eq!(out.results.len(), 1);
        assert_eq!(out.results[0].repo, "a/keep");
    }

    #[test]
    fn binary_rejected_by_admission() {
        let bytes = b"abc\0def".to_vec();
        assert!(admitted("x.bin", &bytes).is_err());
    }

    #[test]
    fn oversized_rejected_by_admission() {
        let bytes = vec![b'a'; MAX_FILE_BYTES + 1];
        assert!(admitted("big.rs", &bytes).is_err());
    }

    #[test]
    fn oversized_not_indexed() {
        let mut idx = TrigramIndex::in_memory();
        let big = vec![b'x'; MAX_FILE_BYTES + 1];
        let r = idx.add_doc("a/b", "big.rs", big).unwrap();
        assert!(r.is_none());
        assert_eq!(idx.doc_count(), 0);
    }

    #[test]
    fn truncation_signaled() {
        let mut docs = Vec::new();
        for i in 0..20 {
            docs.push(doc(
                "a/b",
                &format!("src/f{i}.rs"),
                "common search needle\n",
            ));
        }
        let idx = TrigramIndex::build(docs).unwrap();
        let out = idx
            .search_literal("needle", &Filters::default(), 5)
            .unwrap();
        assert_eq!(out.results.len(), 5);
        assert!(out.truncated);
    }

    #[test]
    fn no_truncation_flag_when_under_cap() {
        let idx = TrigramIndex::build(vec![doc("a/b", "src/a.rs", "needle here\n")]).unwrap();
        let out = idx
            .search_literal("needle", &Filters::default(), 50)
            .unwrap();
        assert!(!out.truncated);
    }

    #[test]
    fn save_load_roundtrip() {
        let idx = TrigramIndex::build(vec![
            doc("a/b", "src/a.rs", "fn retry() {}\n"),
            doc("c/d", "lib/b.py", "def retry(): pass\n"),
        ])
        .unwrap();
        let tmp = tempfile::tempdir().unwrap();
        idx.save(tmp.path()).unwrap();
        let loaded = TrigramIndex::load(tmp.path()).unwrap();
        let a = idx
            .search_literal("retry", &Filters::default(), 10)
            .unwrap();
        let b = loaded
            .search_literal("retry", &Filters::default(), 10)
            .unwrap();
        assert_eq!(a.results.len(), b.results.len());
        assert_eq!(loaded.doc_count(), 2);
    }

    #[test]
    fn index_matches_brute_force_regex() {
        let mut rng: u64 = 0x12345678;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        let alphabet = b"abcdefg .*()_";
        let mut docs = Vec::new();
        for i in 0..200 {
            let len = 20 + (next() % 80) as usize;
            let mut s = String::new();
            for _ in 0..len {
                let c = alphabet[(next() as usize) % alphabet.len()] as char;
                s.push(c);
            }
            docs.push(doc("t/t", &format!("f{i}.txt"), &s));
        }
        let idx = TrigramIndex::build(docs.clone()).unwrap();
        let pattern = "abc.*def";
        let via_index = idx
            .search_regex(pattern, &Filters::default(), 1000)
            .unwrap();
        let re = regex::Regex::new(pattern).unwrap();
        let mut brute: HashSet<String> = HashSet::new();
        for (repo, path, bytes) in &docs {
            let s = String::from_utf8_lossy(bytes);
            if re.is_match(&s) {
                brute.insert(format!("{repo}/{path}"));
            }
        }
        let indexed: HashSet<String> = via_index
            .results
            .iter()
            .map(|r| format!("{}/{}", r.repo, r.path))
            .collect();
        assert_eq!(brute, indexed);
    }

    #[test]
    fn literal_property_against_brute_force() {
        let mut rng: u64 = 0xDEADBEEF;
        let mut next = move || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        let mut docs = Vec::new();
        for i in 0..100 {
            let mut s = String::new();
            for _ in 0..40 {
                s.push_str(match next() % 5 {
                    0 => "needle",
                    1 => "hay ",
                    2 => "stack ",
                    3 => "Needle",
                    _ => "pin ",
                });
            }
            docs.push(doc("t/t", &format!("f{i}.txt"), &s));
        }
        let idx = TrigramIndex::build(docs.clone()).unwrap();
        let out = idx
            .search_literal("needle", &Filters::default(), 1000)
            .unwrap();
        let re = regex::Regex::new("(?i)needle").unwrap();
        let expected: HashSet<String> = docs
            .iter()
            .filter(|(_, _, b)| re.is_match(&String::from_utf8_lossy(b)))
            .map(|(r, p, _)| format!("{r}/{p}"))
            .collect();
        let got: HashSet<String> = out
            .results
            .iter()
            .map(|r| format!("{}/{}", r.repo, r.path))
            .collect();
        assert_eq!(expected, got);
    }

    #[test]
    fn unicode_content_searchable() {
        let idx =
            TrigramIndex::build(vec![doc("a/b", "src/u.rs", "let π_value = 3.14;\n")]).unwrap();
        let out = idx
            .search_literal("π_value", &Filters::default(), 10)
            .unwrap();
        assert_eq!(out.results.len(), 1);
    }

    #[test]
    fn crlf_handled() {
        let idx = TrigramIndex::build(vec![doc(
            "a/b",
            "src/w.rs",
            "fn retry() {}\r\nfn other() {}\r\n",
        )])
        .unwrap();
        let out = idx
            .search_literal("other", &Filters::default(), 10)
            .unwrap();
        assert_eq!(out.results[0].matches[0].line_no, 2);
    }

    #[test]
    fn case_sensitive_match_beyond_display_truncation_does_not_panic() {
        // Regression: spans index the original line, but `line_text` is
        // truncated to MAX_LINE_LEN for display. A match past the cut must
        // not panic the case-exact scorer (found on the real corpus).
        let prefix = "x".repeat(600);
        let content = format!("{prefix} needle_token\n");
        let idx = TrigramIndex::build(vec![doc("a/b", "src/long.rs", &content)]).unwrap();
        let out = idx
            .search_literal_case_sensitive("needle_token", &Filters::default(), 10)
            .unwrap();
        assert_eq!(out.results.len(), 1);
        assert!(out.results[0].matches[0].line_text.ends_with('…'));
        // The match itself is still reported with its true span.
        assert_eq!(out.results[0].matches[0].spans.len(), 1);
    }

    #[test]
    fn empty_index_errors_on_unindexable_but_ok_on_literal() {
        let idx = TrigramIndex::in_memory();
        let out = idx
            .search_literal("anything", &Filters::default(), 10)
            .unwrap();
        assert!(out.results.is_empty());
    }

    #[test]
    fn literal_query_latency_p50_under_100ms_on_10k_files() {
        let mut docs = Vec::new();
        for i in 0..10_000u32 {
            let body = format!(
                "fn function_{i}() {{\n    let value_{i} = compute_something({i});\n    println!(\"{{}}\", value_{i});\n}}\n"
            );
            docs.push(doc(
                "bench/corpus",
                &format!("src/mod_{}.rs", i % 100),
                &body,
            ));
        }
        let start = std::time::Instant::now();
        let idx = TrigramIndex::build_parallel(docs).unwrap();
        eprintln!("build 10k docs: {:?}", start.elapsed());
        let mut latencies = Vec::new();
        for i in 0..50u32 {
            let t = std::time::Instant::now();
            let out = idx
                .search_literal_case_sensitive(
                    &format!("compute_something({}", 9000 + i),
                    &Filters::default(),
                    10,
                )
                .unwrap();
            assert!(!out.results.is_empty());
            latencies.push(t.elapsed());
        }
        latencies.sort();
        let p50 = latencies[latencies.len() / 2];
        eprintln!(
            "literal p50 over 10k files: {:?} (all: p0={:?} p99={:?})",
            p50, latencies[0], latencies[49]
        );
        assert!(p50.as_millis() < 100, "p50 {:?} >= 100ms", p50);
    }
}
