use crate::{Error, Result};
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, Value, ValueRef};
use rusqlite::Connection;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const ZSTD_INGEST_LEVEL: i32 = 3;
pub const DEFAULT_PACK_ROTATE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Blake3Hash(pub [u8; 32]);

impl Blake3Hash {
    pub fn of(bytes: &[u8]) -> Self {
        Self(blake3::hash(bytes).into())
    }

    pub fn hex(&self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut s = String::with_capacity(64);
        for &b in &self.0 {
            s.push(HEX[(b >> 4) as usize] as char);
            s.push(HEX[(b & 0x0f) as usize] as char);
        }
        s
    }

    pub fn from_hex(s: &str) -> Result<Self> {
        if s.len() != 64 {
            return Err(Error::InvalidHash(s.to_string()));
        }
        let mut out = [0u8; 32];
        for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
            let hi = (chunk[0] as char).to_digit(16);
            let lo = (chunk[1] as char).to_digit(16);
            match (hi, lo) {
                (Some(hi), Some(lo)) => out[i] = ((hi << 4) | lo) as u8,
                _ => return Err(Error::InvalidHash(s.to_string())),
            }
        }
        Ok(Self(out))
    }
}

impl fmt::Debug for Blake3Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "b3:{}", &self.hex()[..16])
    }
}

impl fmt::Display for Blake3Hash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.hex())
    }
}

impl ToSql for Blake3Hash {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::Owned(Value::Text(self.hex())))
    }
}

impl FromSql for Blake3Hash {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let s = value.as_str()?;
        Blake3Hash::from_hex(s).map_err(|_| FromSqlError::InvalidType)
    }
}

impl Serialize for Blake3Hash {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.hex())
    }
}

impl<'de> Deserialize<'de> for Blake3Hash {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Blake3Hash::from_hex(&s).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CasStats {
    pub blobs: u64,
    pub packs: u64,
    pub bytes_raw: u64,
    pub bytes_stored: u64,
    pub bytes_offered: u64,
    pub dedup_ratio: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepoFileRow {
    pub repo: String,
    pub path: String,
    pub hash: Blake3Hash,
    pub raw_len: u64,
    pub token_hash: Option<Blake3Hash>,
    pub spdx: Option<String>,
}

struct PackWriter {
    id: i64,
    file: std::fs::File,
    offset: u64,
}

pub struct CasStore {
    root: PathBuf,
    db: Connection,
    writer: Option<PackWriter>,
    level: i32,
    rotate_at: u64,
}

impl CasStore {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        std::fs::create_dir_all(&root)?;
        let packs_dir = root.join("packs");
        std::fs::create_dir_all(&packs_dir)?;
        let db = Connection::open(root.join("index.db"))?;
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.pragma_update(None, "synchronous", "NORMAL")?;
        db.pragma_update(None, "mmap_size", 268435456i64)?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS packs(id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, blobs INTEGER NOT NULL DEFAULT 0, bytes_raw INTEGER NOT NULL DEFAULT 0, bytes_stored INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE IF NOT EXISTS blobs(hash TEXT PRIMARY KEY, pack_id INTEGER NOT NULL REFERENCES packs(id), offset INTEGER NOT NULL, raw_len INTEGER NOT NULL, comp_len INTEGER NOT NULL);
             CREATE INDEX IF NOT EXISTS idx_blobs_pack ON blobs(pack_id);
             CREATE TABLE IF NOT EXISTS repo_blobs(repo TEXT NOT NULL, path TEXT NOT NULL, hash TEXT NOT NULL, raw_len INTEGER NOT NULL, token_hash TEXT, spdx TEXT, PRIMARY KEY(repo, path));
             CREATE INDEX IF NOT EXISTS idx_repo_blobs_hash ON repo_blobs(hash);
             CREATE TABLE IF NOT EXISTS fork_map(repo TEXT PRIMARY KEY, canonical TEXT NOT NULL);",
        )?;
        Ok(Self {
            root,
            db,
            writer: None,
            level: ZSTD_INGEST_LEVEL,
            rotate_at: DEFAULT_PACK_ROTATE_BYTES,
        })
    }

    pub fn set_zstd_level(&mut self, level: i32) {
        self.level = level;
    }

    pub fn set_rotate_at(&mut self, bytes: u64) {
        self.rotate_at = bytes;
    }

    fn bump_meta(&self, key: &str, delta: i64) -> Result<()> {
        self.db.execute(
            "INSERT INTO meta(key, value) VALUES(?1, ?2) ON CONFLICT(key) DO UPDATE SET value = value + ?2",
            (key, delta),
        )?;
        Ok(())
    }

    fn meta_u64(&self, key: &str) -> Result<u64> {
        Ok(self
            .db
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap_or(0) as u64)
    }

    pub fn put(&mut self, bytes: &[u8]) -> Result<Blake3Hash> {
        let hash = Blake3Hash::of(bytes);
        self.bump_meta("puts", 1)?;
        self.bump_meta("bytes_offered", bytes.len() as i64)?;
        if self.db.query_row(
            "SELECT 1 FROM blobs WHERE hash = ?1",
            [&hash],
            |_| Ok(()),
        ).is_ok() {
            return Ok(hash);
        }
        let compressed = zstd::bulk::compress(bytes, self.level)?;
        self.ensure_writer(compressed.len() as u64)?;
        let writer = self.writer.as_mut().expect("pack writer open");
        let offset = writer.offset;
        writer.file.write_all(&compressed)?;
        writer.file.flush()?;
        let stored = compressed.len() as u64;
        writer.offset += stored;
        let pack_id = writer.id;
        self.db.execute(
            "INSERT INTO blobs(hash, pack_id, offset, raw_len, comp_len) VALUES(?1, ?2, ?3, ?4, ?5)",
            (&hash, pack_id, offset as i64, bytes.len() as i64, stored as i64),
        )?;
        self.db.execute(
            "UPDATE packs SET blobs = blobs + 1, bytes_raw = bytes_raw + ?2, bytes_stored = bytes_stored + ?3 WHERE id = ?1",
            (pack_id, bytes.len() as i64, stored as i64),
        )?;
        Ok(hash)
    }

    fn ensure_writer(&mut self, incoming: u64) -> Result<()> {
        let rotate = self
            .writer
            .as_ref()
            .is_some_and(|w| w.offset > 0 && w.offset + incoming > self.rotate_at);
        if rotate {
            self.seal();
        }
        if self.writer.is_none() {
            let next_id: i64 = self
                .db
                .query_row("SELECT COALESCE(MAX(id), 0) + 1 FROM packs", [], |r| r.get(0))?;
            let name = format!("pack-{next_id:06}.pack");
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.root.join("packs").join(&name))?;
            self.db.execute("INSERT INTO packs(id, name) VALUES(?1, ?2)", (next_id, name))?;
            self.writer = Some(PackWriter {
                id: next_id,
                file,
                offset: 0,
            });
        }
        Ok(())
    }

    pub fn seal(&mut self) {
        if let Some(w) = self.writer.take() {
            let _ = w.file.sync_all();
        }
    }

    pub fn get(&self, hash: &Blake3Hash) -> Result<Vec<u8>> {
        let row = self.db.query_row(
            "SELECT p.name, b.offset, b.raw_len, b.comp_len FROM blobs b JOIN packs p ON p.id = b.pack_id WHERE b.hash = ?1",
            [hash],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            },
        );
        let (pack_name, offset, raw_len, comp_len) = match row {
            Ok(v) => v,
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                return Err(Error::BlobNotFound(hash.hex()))
            }
            Err(e) => return Err(e.into()),
        };
        let mut file = std::fs::File::open(self.root.join("packs").join(pack_name))?;
        file.seek(SeekFrom::Start(offset as u64))?;
        let mut compressed = vec![0u8; comp_len as usize];
        file.read_exact(&mut compressed)?;
        let bytes = zstd::bulk::decompress(&compressed, raw_len as usize)?;
        if bytes.len() as i64 != raw_len {
            return Err(Error::CorruptBlob(hash.hex()));
        }
        if Blake3Hash::of(&bytes) != *hash {
            return Err(Error::CorruptBlob(hash.hex()));
        }
        Ok(bytes)
    }

    pub fn contains(&self, hash: &Blake3Hash) -> Result<bool> {
        Ok(self
            .db
            .query_row("SELECT 1 FROM blobs WHERE hash = ?1", [hash], |_| Ok(()))
            .is_ok())
    }

    pub fn stats(&self) -> Result<CasStats> {
        let (blobs, bytes_raw, bytes_stored): (i64, i64, i64) = self.db.query_row(
            "SELECT COUNT(*), COALESCE(SUM(raw_len), 0), COALESCE(SUM(comp_len), 0) FROM blobs",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let packs: i64 =
            self.db
                .query_row("SELECT COUNT(*) FROM packs", [], |r| r.get(0))?;
        let bytes_offered = self.meta_u64("bytes_offered")?;
        let dedup_ratio = if bytes_raw > 0 {
            bytes_offered as f64 / bytes_raw as f64
        } else {
            1.0
        };
        Ok(CasStats {
            blobs: blobs as u64,
            packs: packs as u64,
            bytes_raw: bytes_raw as u64,
            bytes_stored: bytes_stored as u64,
            bytes_offered,
            dedup_ratio,
        })
    }

    pub fn record_file(
        &self,
        repo: &str,
        path: &str,
        hash: &Blake3Hash,
        raw_len: u64,
        token_hash: Option<&Blake3Hash>,
        spdx: Option<&str>,
    ) -> Result<()> {
        self.db.execute(
            "INSERT INTO repo_blobs(repo, path, hash, raw_len, token_hash, spdx) VALUES(?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(repo, path) DO UPDATE SET hash = ?3, raw_len = ?4, token_hash = ?5, spdx = ?6",
            (repo, path, hash, raw_len as i64, token_hash, spdx),
        )?;
        Ok(())
    }

    pub fn repo_files(&self, repo: &str) -> Result<Vec<RepoFileRow>> {
        let mut stmt = self.db.prepare(
            "SELECT repo, path, hash, raw_len, token_hash, spdx FROM repo_blobs WHERE repo = ?1 ORDER BY path",
        )?;
        let rows = stmt.query_map([repo], |r| {
            let hash_str: String = r.get(2)?;
            let token_str: Option<String> = r.get(4)?;
            let hash = Blake3Hash::from_hex(&hash_str).map_err(|_| FromSqlError::InvalidType)?;
            let token_hash = token_str
                .map(|s| Blake3Hash::from_hex(&s))
                .transpose()
                .map_err(|_| FromSqlError::InvalidType)?;
            Ok(RepoFileRow {
                repo: r.get(0)?,
                path: r.get(1)?,
                hash,
                raw_len: r.get::<_, i64>(3)? as u64,
                token_hash,
                spdx: r.get(5)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn set_fork(&self, repo: &str, canonical: &str) -> Result<()> {
        self.db.execute(
            "INSERT INTO fork_map(repo, canonical) VALUES(?1, ?2) ON CONFLICT(repo) DO UPDATE SET canonical = ?2",
            (repo, canonical),
        )?;
        Ok(())
    }

    pub fn canonical_for(&self, repo: &str) -> Result<Option<String>> {
        let out = self
            .db
            .query_row(
                "SELECT canonical FROM fork_map WHERE repo = ?1",
                [repo],
                |r| r.get::<_, String>(0),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        Ok(out)
    }
}

impl Drop for CasStore {
    fn drop(&mut self) {
        self.seal();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_hex_roundtrip() {
        let h = Blake3Hash::of(b"abc");
        assert_eq!(Blake3Hash::from_hex(&h.hex()).unwrap(), h);
        assert_eq!(h.hex().len(), 64);
    }

    #[test]
    fn put_get_dedup_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cas = CasStore::open(tmp.path()).unwrap();
        let a = b"fn main() { println!(\"hello, world\"); }\n".repeat(200);
        let h1 = cas.put(&a).unwrap();
        let h2 = cas.put(&a).unwrap();
        assert_eq!(h1, h2);
        let b = b"// a different file\n".repeat(100);
        let h3 = cas.put(&b).unwrap();
        assert_ne!(h1, h3);
        let stats = cas.stats().unwrap();
        assert_eq!(stats.blobs, 2);
        assert_eq!(stats.bytes_raw, (a.len() + b.len()) as u64);
        assert!(stats.bytes_stored < stats.bytes_raw, "zstd must shrink source-like text");
        let expected_ratio = (2.0 * a.len() as f64 + b.len() as f64) / (a.len() + b.len()) as f64;
        assert!((stats.dedup_ratio - expected_ratio).abs() < 1e-9);
        assert_eq!(cas.get(&h1).unwrap(), a);
        assert_eq!(cas.get(&h3).unwrap(), b);
    }

    #[test]
    fn seal_starts_new_pack() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cas = CasStore::open(tmp.path()).unwrap();
        cas.put(b"one").unwrap();
        cas.seal();
        cas.put(b"two").unwrap();
        let stats = cas.stats().unwrap();
        assert_eq!(stats.packs, 2);
        assert_eq!(stats.blobs, 2);
    }

    #[test]
    fn rotation_by_size() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cas = CasStore::open(tmp.path()).unwrap();
        cas.set_rotate_at(1024);
        for i in 0..50 {
            cas.put(format!("payload {i} ").repeat(32).as_bytes()).unwrap();
        }
        let stats = cas.stats().unwrap();
        assert!(stats.packs > 1, "expected rotation, got {stats:?}");
        for i in 0..50 {
            let content = format!("payload {i} ").repeat(32);
            let hash = Blake3Hash::of(content.as_bytes());
            assert_eq!(cas.get(&hash).unwrap(), content.as_bytes());
        }
    }

    #[test]
    fn missing_blob_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let cas = CasStore::open(tmp.path()).unwrap();
        let missing = Blake3Hash::of(b"never stored");
        match cas.get(&missing) {
            Err(Error::BlobNotFound(h)) => assert_eq!(h, missing.hex()),
            other => panic!("expected BlobNotFound, got {other:?}"),
        }
    }
}
