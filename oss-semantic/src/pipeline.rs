//! Pipeline glue: chunk → embed → store (vectors + SQLite), then query by
//! embedding and resolve hits back to `file:line`.

use std::path::Path;

use crate::Result;
use crate::chunk_store::{ChunkStore, StoredChunk};
use crate::chunking::chunk_source;
use crate::embedding::Embedder;
use crate::vector_store::{FlatVectorStore, MetadataFilter, SearchHit, VectorMeta, VectorStore};

/// A dense search hit resolved to its chunk metadata.
#[derive(Debug, Clone)]
pub struct ResolvedHit {
    /// The vector store hit (id, score, metadata).
    pub hit: SearchHit,
    /// The chunk row (file, line range, text) or `None` if the vector has
    /// no chunk row.
    pub chunk: Option<StoredChunk>,
}

impl ResolvedHit {
    /// `path:line` location for display.
    pub fn location(&self) -> String {
        self.chunk
            .as_ref()
            .map(|c| format!("{}:{}", c.path, c.start_line))
            .unwrap_or_else(|| self.hit.meta.location())
    }
}

/// Chunker + embedder + flat vector store + SQLite chunk store.
pub struct SemanticPipeline<E: Embedder> {
    embedder: E,
    vectors: FlatVectorStore,
    chunks: ChunkStore,
}

impl<E: Embedder> SemanticPipeline<E> {
    /// Build a pipeline with a fresh in-memory vector store and a chunk
    /// store at `chunk_db_path` (or in memory when `None`).
    pub fn new(embedder: E, chunk_db_path: Option<&Path>) -> Result<Self> {
        let chunks = match chunk_db_path {
            Some(p) => ChunkStore::open(p)?,
            None => ChunkStore::open_in_memory()?,
        };
        Ok(Self {
            embedder,
            vectors: FlatVectorStore::new(),
            chunks,
        })
    }

    /// Access the underlying vector store (for persistence with
    /// [`FlatVectorStore::save`]).
    pub fn vectors(&self) -> &FlatVectorStore {
        &self.vectors
    }

    /// Mutable access to the vector store (for [`FlatVectorStore::load`]).
    pub fn vectors_mut(&mut self) -> &mut FlatVectorStore {
        &mut self.vectors
    }

    /// Access the chunk store.
    pub fn chunks(&self) -> &ChunkStore {
        &self.chunks
    }

    /// Chunk one source file, embed each chunk, and record both the vector
    /// (embedding id = chunk id) and the chunk row. Returns the number of
    /// chunks indexed. Re-indexing the same file replaces its rows.
    pub fn index_source(
        &mut self,
        repo: &str,
        lang: &str,
        path: &str,
        source: &str,
    ) -> Result<usize> {
        let chunks = chunk_source(source, lang, path);
        if chunks.is_empty() {
            return Ok(0);
        }
        let texts: Vec<String> = chunks.iter().map(|c| c.text.clone()).collect();
        let embeddings = self.embedder.embed(&texts)?;
        let mut n = 0;
        for (chunk, vector) in chunks.iter().zip(&embeddings) {
            let meta = VectorMeta {
                repo: repo.to_string(),
                path: chunk.path.clone(),
                kind: chunk.kind.as_str().to_string(),
                lang: lang.to_string(),
                start_line: chunk.start_line,
                end_line: chunk.end_line,
            };
            self.vectors.add(chunk.id.clone(), vector.clone(), meta)?;
            n += 1;
        }
        // Chunk rows land after embedding so a failed embed leaves no rows.
        let ids: Vec<String> = chunks.iter().map(|c| c.id.clone()).collect();
        self.chunks.add_chunks(repo, lang, chunks, Some(&ids))?;
        Ok(n)
    }

    /// Embed `query` and return the top-k dense hits (metadata-filtered).
    pub fn semantic_hits(
        &self,
        query: &str,
        k: usize,
        filter: &MetadataFilter,
    ) -> Result<Vec<SearchHit>> {
        let qv = &self.embedder.embed(&[query.to_string()])?[0];
        Ok(self.vectors.search(qv, k, filter))
    }

    /// [`Self::semantic_hits`] with each hit resolved to its chunk row.
    pub fn search(
        &self,
        query: &str,
        k: usize,
        filter: &MetadataFilter,
    ) -> Result<Vec<ResolvedHit>> {
        let hits = self.semantic_hits(query, k, filter)?;
        let mut out = Vec::with_capacity(hits.len());
        for hit in hits {
            let chunk = self.chunks.resolve_embedding_id(&hit.id)?;
            out.push(ResolvedHit { hit, chunk });
        }
        Ok(out)
    }
}
