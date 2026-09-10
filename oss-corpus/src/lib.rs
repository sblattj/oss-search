use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("sqlite: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("git {context}: {stderr}")]
    Git { context: String, stderr: String },
    #[error("invalid hash string: {0}")]
    InvalidHash(String),
    #[error("blob not found: {0}")]
    BlobNotFound(String),
    #[error("corrupt blob: {0}")]
    CorruptBlob(String),
    #[error("cat-file stream ended early")]
    StreamTruncated,
}

pub type Result<T> = std::result::Result<T, Error>;

pub mod admission;
pub mod cas;
pub mod clone;
pub mod dedup;
pub mod ingest;
pub mod license;

pub use admission::{AdmissionGate, Rejection};
pub use cas::{Blake3Hash, CasStats, CasStore};
pub use clone::{CloneCoordinator, RepoCloneReport, SyncAction};
pub use dedup::{DedupEngine, DupCounts, DupKind};
pub use ingest::{CorpusIngestor, FileRejection, NearDupPair, RepoLicense, RepoReport};
pub use license::LicenseMatch;
