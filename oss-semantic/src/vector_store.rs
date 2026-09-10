//! Flat in-memory + on-disk vector store with cosine top-k search.
//!
//! Brute-force exact search over f32 vectors with metadata pre-filtering
//! (the filter runs *before* scoring, so non-matching rows cost nothing but
//! a metadata check). Fine at personal scale (~10³–10⁶ chunks); the
//! intended upgrade path is **LanceDB** behind the same [`VectorStore`]
//! trait — it adds SQL-style pre-filtering, disk-first memory behavior, and
//! optional ANN without changing call sites. LanceDB is deliberately not a
//! dependency in this pass to keep its C++/Arrow build out of the
//! workspace (research note 16).

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::{Result, SemanticError};

/// Metadata carried alongside every stored vector.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorMeta {
    /// Owning repository (e.g. `tokio-rs/tokio`).
    pub repo: String,
    /// Path within the repository (e.g. `src/lib.rs`).
    pub path: String,
    /// Chunk kind name (see [`crate::ChunkKind::as_str`]).
    pub kind: String,
    /// Language name (e.g. `rust`), or `""` for windowed chunks.
    pub lang: String,
    /// 1-based inclusive start line.
    pub start_line: usize,
    /// 1-based inclusive end line.
    pub end_line: usize,
}

impl VectorMeta {
    /// Render as `path:line` for search-result display.
    pub fn location(&self) -> String {
        format!("{}:{}", self.path, self.start_line)
    }
}

/// Metadata constraints applied *before* scoring. `None` fields match
/// anything.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MetadataFilter {
    /// Exact repository match.
    pub repo: Option<String>,
    /// Path prefix match (component-wise: `src/` matches `src/lib.rs`).
    pub path_prefix: Option<String>,
    /// Exact chunk-kind match.
    pub kind: Option<String>,
    /// Exact language match.
    pub lang: Option<String>,
}

impl MetadataFilter {
    /// Filter matching any metadata.
    pub fn none() -> Self {
        Self::default()
    }

    /// Filter constraining the search to one repository.
    pub fn repo(repo: impl Into<String>) -> Self {
        Self {
            repo: Some(repo.into()),
            ..Self::default()
        }
    }

    fn matches(&self, meta: &VectorMeta) -> bool {
        if let Some(repo) = &self.repo
            && repo != &meta.repo
        {
            return false;
        }
        if let Some(prefix) = &self.path_prefix {
            let ok = meta.path == prefix.as_str()
                || (meta.path.starts_with(prefix.as_str())
                    && (prefix.ends_with('/') || meta.path[prefix.len()..].starts_with('/')));
            if !ok {
                return false;
            }
        }
        if let Some(kind) = &self.kind
            && kind != &meta.kind
        {
            return false;
        }
        if let Some(lang) = &self.lang
            && lang != &meta.lang
        {
            return false;
        }
        true
    }
}

/// One search result.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    /// The vector's id (== the chunk's embedding id).
    pub id: String,
    /// Cosine similarity in `[-1, 1]`.
    pub score: f32,
    /// The stored metadata.
    pub meta: VectorMeta,
}

/// A store of vectors searchable by cosine similarity.
pub trait VectorStore {
    /// Add one vector under `id` with metadata. Fails if the dimension
    /// does not match the store's.
    fn add(&mut self, id: String, vector: Vec<f32>, meta: VectorMeta) -> Result<()>;

    /// Cosine top-k among rows matching `filter` (pre-filtered).
    fn search(&self, query: &[f32], k: usize, filter: &MetadataFilter) -> Vec<SearchHit>;

    /// Number of stored vectors.
    fn len(&self) -> usize;

    /// Whether the store is empty.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Row {
    id: String,
    vector: Vec<f32>,
    norm: f32,
    meta: VectorMeta,
}

#[derive(Serialize, Deserialize)]
struct StoreFile {
    dim: usize,
    rows: Vec<Row>,
}

/// Flat brute-force store: all vectors in memory, exact cosine scoring,
/// single-file binary persistence (bincode with a small magic+version
/// header).
#[derive(Debug, Default)]
pub struct FlatVectorStore {
    dim: Option<usize>,
    rows: Vec<Row>,
}

impl FlatVectorStore {
    /// Empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// The fixed dimensionality of this store (first [`VectorStore::add`]
    /// pins it), or `None` while empty.
    pub fn dim(&self) -> Option<usize> {
        self.dim
    }

    /// Serialize to a single binary file at `path` (atomic via temp +
    /// rename).
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let dim = self.dim.unwrap_or(0);
        let payload = StoreFile {
            dim,
            rows: self
                .rows
                .iter()
                .map(|r| Row {
                    id: r.id.clone(),
                    vector: r.vector.clone(),
                    norm: r.norm,
                    meta: r.meta.clone(),
                })
                .collect(),
        };
        let body = bincode::serde::encode_to_vec(&payload, bincode::config::standard())
            .map_err(|e| SemanticError::Store(format!("encode: {e}")))?;
        let mut bytes = Vec::with_capacity(8 + body.len());
        bytes.extend_from_slice(b"OSSV");
        bytes.push(1); // format version
        bytes.extend_from_slice(&(body.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&body);
        let path = path.as_ref();
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, &bytes).map_err(|e| SemanticError::Store(format!("write: {e}")))?;
        fs::rename(&tmp, path).map_err(|e| SemanticError::Store(format!("rename: {e}")))?;
        Ok(())
    }

    /// Load a store previously written by [`FlatVectorStore::save`].
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let bytes = fs::read(path).map_err(|e| SemanticError::Store(format!("read: {e}")))?;
        if bytes.len() < 13 || &bytes[0..4] != b"OSSV" {
            return Err(SemanticError::Store("bad magic: not an OSSV file".into()));
        }
        if bytes[4] != 1 {
            return Err(SemanticError::Store(format!(
                "unsupported format version {}",
                bytes[4]
            )));
        }
        let body_len =
            u64::from_le_bytes(bytes[5..13].try_into().expect("8 length bytes")) as usize;
        if 13 + body_len != bytes.len() {
            return Err(SemanticError::Store("truncated or corrupt store".into()));
        }
        let (payload, _): (StoreFile, usize) =
            bincode::serde::decode_from_slice(&bytes[13..], bincode::config::standard())
                .map_err(|e| SemanticError::Store(format!("decode: {e}")))?;
        Ok(Self {
            dim: if payload.dim == 0 {
                None
            } else {
                Some(payload.dim)
            },
            rows: payload.rows,
        })
    }
}

impl VectorStore for FlatVectorStore {
    fn add(&mut self, id: String, vector: Vec<f32>, meta: VectorMeta) -> Result<()> {
        if let Some(dim) = self.dim {
            if vector.len() != dim {
                return Err(SemanticError::Store(format!(
                    "dimension mismatch: store is {dim}, vector is {}",
                    vector.len()
                )));
            }
        } else if !vector.is_empty() {
            self.dim = Some(vector.len());
        }
        let norm = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
        // Replace an existing row with the same id (re-index path).
        if let Some(slot) = self.rows.iter_mut().find(|r| r.id == id) {
            slot.vector = vector;
            slot.norm = norm;
            slot.meta = meta;
            return Ok(());
        }
        self.rows.push(Row {
            id,
            vector,
            norm,
            meta,
        });
        Ok(())
    }

    fn search(&self, query: &[f32], k: usize, filter: &MetadataFilter) -> Vec<SearchHit> {
        if k == 0 || query.is_empty() {
            return Vec::new();
        }
        let q_norm = query.iter().map(|v| v * v).sum::<f32>().sqrt();
        let mut scored: Vec<SearchHit> = self
            .rows
            .iter()
            .filter(|row| filter.matches(&row.meta)) // pre-filter, before any scoring
            .map(|row| {
                let score = if q_norm == 0.0 || row.norm == 0.0 {
                    0.0
                } else {
                    let dot = row
                        .vector
                        .iter()
                        .zip(query)
                        .map(|(a, b)| a * b)
                        .sum::<f32>();
                    dot / (row.norm * q_norm)
                };
                SearchHit {
                    id: row.id.clone(),
                    score,
                    meta: row.meta.clone(),
                }
            })
            .collect();
        // Deterministic order: score desc, then id asc for ties.
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.id.cmp(&b.id))
        });
        scored.truncate(k);
        scored
    }

    fn len(&self) -> usize {
        self.rows.len()
    }
}
