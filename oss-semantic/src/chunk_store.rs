//! SQLite-backed chunk metadata store.
//!
//! Chunks are stored with `embedding_id` foreign to the vector store, so a
//! fused hybrid result (which carries ids and ranks) resolves to concrete
//! `file:line` locations and chunk text. Uses `rusqlite` with the bundled
//! SQLite so the workspace builds with no system dependency.

use std::path::Path;

use rusqlite::Connection;

use crate::{Chunk, Result};

/// A chunk row as stored in SQLite.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredChunk {
    pub id: String,
    pub repo: String,
    pub path: String,
    /// Chunk kind name ([`crate::ChunkKind::as_str`]).
    pub kind: String,
    pub start_line: usize,
    pub end_line: usize,
    pub text: String,
    /// Language name, or `"window"` for fallback windows.
    pub lang: String,
    /// Id of the corresponding vector in the vector store.
    pub embedding_id: String,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS chunks (
    id           TEXT PRIMARY KEY,
    repo         TEXT NOT NULL,
    path         TEXT NOT NULL,
    kind         TEXT NOT NULL,
    start_line   INTEGER NOT NULL,
    end_line     INTEGER NOT NULL,
    text         TEXT NOT NULL,
    lang         TEXT NOT NULL,
    embedding_id TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_chunks_embedding_id ON chunks(embedding_id);
CREATE INDEX IF NOT EXISTS idx_chunks_repo_path ON chunks(repo, path);
";

/// On-disk (or in-memory with `:memory:`) chunk store.
pub struct ChunkStore {
    conn: Connection,
}

impl ChunkStore {
    /// Open (creating if needed) a store at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    /// Open an in-memory store.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    /// Insert or replace chunks in one transaction. `embedding_id`
    /// defaults to the chunk's own `id` when `None`.
    pub fn add_chunks<I>(
        &mut self,
        repo: &str,
        lang: &str,
        chunks: I,
        embedding_ids: Option<&[String]>,
    ) -> Result<usize>
    where
        I: IntoIterator<Item = Chunk>,
    {
        let mut count = 0usize;
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT OR REPLACE INTO chunks
                 (id, repo, path, kind, start_line, end_line, text, lang, embedding_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?;
            for (i, chunk) in chunks.into_iter().enumerate() {
                let embedding_id = embedding_ids
                    .and_then(|ids| ids.get(i))
                    .map(String::as_str)
                    .unwrap_or(chunk.id.as_str());
                stmt.execute(rusqlite::params![
                    chunk.id,
                    repo,
                    chunk.path,
                    chunk.kind.as_str(),
                    chunk.start_line as i64,
                    chunk.end_line as i64,
                    chunk.text,
                    lang,
                    embedding_id,
                ])?;
                count += 1;
            }
        }
        tx.commit()?;
        Ok(count)
    }

    /// Fetch one chunk by id.
    pub fn get(&self, id: &str) -> Result<Option<StoredChunk>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT id, repo, path, kind, start_line, end_line, text, lang, embedding_id FROM chunks WHERE id = ?1")?;
        let mut rows = stmt.query([id])?;
        match rows.next()? {
            Some(row) => Ok(Some(Self::row_to_chunk(row)?)),
            None => Ok(None),
        }
    }

    /// Resolve vector-store hits to chunks by embedding id (one row per
    /// hit; `None` if the vector has no chunk row).
    pub fn resolve_embedding_id(&self, embedding_id: &str) -> Result<Option<StoredChunk>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT id, repo, path, kind, start_line, end_line, text, lang, embedding_id FROM chunks WHERE embedding_id = ?1 LIMIT 1")?;
        let mut rows = stmt.query([embedding_id])?;
        match rows.next()? {
            Some(row) => Ok(Some(Self::row_to_chunk(row)?)),
            None => Ok(None),
        }
    }

    /// All chunks for one file.
    pub fn by_path(&self, repo: &str, path: &str) -> Result<Vec<StoredChunk>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT id, repo, path, kind, start_line, end_line, text, lang, embedding_id FROM chunks WHERE repo = ?1 AND path = ?2 ORDER BY start_line")?;
        let rows = stmt.query_map([repo, path], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let r = row?;
            out.push(StoredChunk {
                id: r.0,
                repo: r.1,
                path: r.2,
                kind: r.3,
                start_line: r.4 as usize,
                end_line: r.5 as usize,
                text: r.6,
                lang: r.7,
                embedding_id: r.8,
            });
        }
        Ok(out)
    }

    /// Total number of chunk rows.
    pub fn count(&self) -> Result<usize> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM chunks", [], |row| row.get(0))?;
        Ok(n as usize)
    }

    fn row_to_chunk(row: &rusqlite::Row<'_>) -> Result<StoredChunk> {
        Ok(StoredChunk {
            id: row.get(0)?,
            repo: row.get(1)?,
            path: row.get(2)?,
            kind: row.get(3)?,
            start_line: row.get::<_, i64>(4)? as usize,
            end_line: row.get::<_, i64>(5)? as usize,
            text: row.get(6)?,
            lang: row.get(7)?,
            embedding_id: row.get(8)?,
        })
    }
}
