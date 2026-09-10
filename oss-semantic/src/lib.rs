//! # oss-semantic
//!
//! The semantic layer of the agent-facing OSS code search stack (stage 3):
//! code-aware chunking, local embeddings, a vector store, and hybrid
//! (lexical + dense) fusion.
//!
//! Layout:
//!
//! * [`chunking`] — tree-sitter function/class/impl chunking for a starter
//!   set of languages (rust, python, javascript, typescript, go, c) with a
//!   fixed sliding-window fallback for everything else. Grammars are chosen
//!   because tree-sitter's GLR error recovery still yields usable trees on
//!   broken or partial code (research note 21).
//! * [`embedding`] — the [`embedding::Embedder`] trait, a deterministic
//!   [`embedding::StubEmbedder`] for tests, and (behind the `fastembed`
//!   cargo feature) a real local-inference
//!   [`FastEmbedder`](fast_embedder::FastEmbedder).
//! * [`vector_store`] — a flat in-memory / on-disk brute-force cosine store
//!   with metadata pre-filtering. [`LanceDB`](https://lancedb.github.io) is
//!   the intended swap-in target behind the same
//!   [`vector_store::VectorStore`] trait once the corpus outgrows brute
//!   force; it is not used in this pass to keep the C++ build out of the
//!   workspace (research note 16 recommends LanceDB for that upgrade).
//! * [`chunk_store`] — SQLite-backed chunk metadata so search results
//!   resolve to `file:line`.
//! * [`hybrid`] — Reciprocal Rank Fusion (default `k = 3`; research note 14
//!   recommends k = 2–5 for code workloads with few relevant docs per
//!   query) and a query-shape classifier used to route identifier-shaped
//!   queries to the lexical index alone.
//!
//! No test in this crate touches the network or downloads a model.

pub mod chunk_store;
pub mod chunking;
pub mod embedding;
pub mod hybrid;
mod pipeline;
pub mod vector_store;

#[cfg(feature = "fastembed")]
pub mod fast_embedder;

pub use chunk_store::{ChunkStore, StoredChunk};
pub use chunking::{Chunk, ChunkKind, chunk_by_extension, chunk_source};
pub use embedding::{Embedder, StubEmbedder};
pub use hybrid::{FusedComponents, QueryKind, classify_query, rrf_fuse};
pub use pipeline::{ResolvedHit, SemanticPipeline};
pub use vector_store::{FlatVectorStore, MetadataFilter, SearchHit, VectorMeta, VectorStore};

use std::fmt;

/// Errors produced by the semantic layer.
#[derive(Debug)]
pub enum SemanticError {
    /// A tree-sitter grammar rejected the language or failed to parse.
    Parse(String),
    /// Embedding backend failure.
    Embed(String),
    /// Vector store I/O or format failure.
    Store(String),
    /// SQLite chunk store failure.
    Sqlite(rusqlite::Error),
    /// Text expected to decode as UTF-8 was not.
    Utf8(std::str::Utf8Error),
}

impl fmt::Display for SemanticError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SemanticError::Parse(m) => write!(f, "parse error: {m}"),
            SemanticError::Embed(m) => write!(f, "embedding error: {m}"),
            SemanticError::Store(m) => write!(f, "store error: {m}"),
            SemanticError::Sqlite(e) => write!(f, "sqlite error: {e}"),
            SemanticError::Utf8(e) => write!(f, "utf-8 error: {e}"),
        }
    }
}

impl std::error::Error for SemanticError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            SemanticError::Sqlite(e) => Some(e),
            SemanticError::Utf8(e) => Some(e),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for SemanticError {
    fn from(e: rusqlite::Error) -> Self {
        SemanticError::Sqlite(e)
    }
}

impl From<std::str::Utf8Error> for SemanticError {
    fn from(e: std::str::Utf8Error) -> Self {
        SemanticError::Utf8(e)
    }
}

/// Convenience alias for results in this crate.
pub type Result<T> = std::result::Result<T, SemanticError>;
