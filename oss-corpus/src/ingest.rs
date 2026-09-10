use crate::admission::{AdmissionGate, Rejection};
use crate::cas::{Blake3Hash, CasStore};
use crate::dedup::{token_hash, tokenize, DedupEngine, DupCounts, DupKind};
use crate::license::{detect_repo_license, is_license_file_name, scan_spdx_tag};
use crate::Result;
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;

pub const DEFAULT_NEAR_DUP_THRESHOLD: f64 = 0.85;

#[derive(Debug, Clone, Serialize)]
pub struct FileRejection {
    pub path: String,
    pub reason: Rejection,
}

#[derive(Debug, Clone, Serialize)]
pub struct NearDupPair {
    pub path: String,
    pub duplicate_of: String,
    pub jaccard: f64,
    pub estimated: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepoLicense {
    pub file: String,
    pub spdx: String,
    pub confidence: f64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SpdxTag {
    pub path: String,
    pub expression: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RepoReport {
    pub repo: String,
    pub canonical: Option<String>,
    pub files_total: u64,
    pub files_accepted: u64,
    pub rejections: Vec<FileRejection>,
    pub bytes_offered: u64,
    pub bytes_new_unique: u64,
    pub dup: DupCounts,
    pub near_dups: Vec<NearDupPair>,
    pub spdx_tags: Vec<SpdxTag>,
    pub license: Option<RepoLicense>,
}

pub struct CorpusIngestor {
    pub cas: CasStore,
    dedup: DedupEngine,
    gate: AdmissionGate,
    path_of: HashMap<Blake3Hash, String>,
}

impl CorpusIngestor {
    pub fn open(cas_root: impl AsRef<Path>) -> Result<Self> {
        Ok(Self {
            cas: CasStore::open(cas_root)?,
            dedup: DedupEngine::new(DEFAULT_NEAR_DUP_THRESHOLD),
            gate: AdmissionGate::default(),
            path_of: HashMap::new(),
        })
    }

    pub fn from_parts(cas: CasStore, dedup: DedupEngine, gate: AdmissionGate) -> Self {
        Self {
            cas,
            dedup,
            gate,
            path_of: HashMap::new(),
        }
    }

    pub fn set_fork_map(&mut self, pairs: &[(&str, &str)]) -> Result<()> {
        for (repo, canonical) in pairs {
            self.cas.set_fork(repo, canonical)?;
        }
        Ok(())
    }

    pub fn ingest_worktree(&mut self, repo: &str, dir: &Path) -> Result<RepoReport> {
        let stats_before = self.cas.stats()?;
        let mut report = RepoReport {
            repo: repo.to_string(),
            canonical: self.cas.canonical_for(repo)?,
            ..Default::default()
        };
        let mut license_candidates: Vec<(String, Vec<u8>)> = Vec::new();
        for entry in walkdir::WalkDir::new(dir)
            .min_depth(1)
            .sort_by_file_name()
            .into_iter()
            .filter_entry(|e| e.file_name().to_str() != Some(".git"))
        {
            let Ok(entry) = entry else {
                continue;
            };
            if !entry.file_type().is_file() {
                continue;
            }
            let Ok(rel) = entry.path().strip_prefix(dir) else {
                continue;
            };
            let rel_path = rel.to_string_lossy().replace('\\', "/");
            let Ok(bytes) = std::fs::read(entry.path()) else {
                continue;
            };
            report.files_total += 1;
            if let Err(reason) = self.gate.check(&bytes) {
                report.rejections.push(FileRejection {
                    path: rel_path,
                    reason,
                });
                continue;
            }
            if is_license_file_name(&rel_path) && rel.components().count() == 1 {
                license_candidates.push((rel_path.clone(), bytes.clone()));
            }
            report.bytes_offered += bytes.len() as u64;
            let hash = self.cas.put(&bytes)?;
            let tokens = tokenize(&String::from_utf8_lossy(&bytes));
            let thash = token_hash(&tokens);
            let verdict = self.dedup.classify(hash, &tokens);
            match &verdict {
                DupKind::Unique => report.dup.unique += 1,
                DupKind::Exact { .. } => report.dup.exact += 1,
                DupKind::Tokens { .. } => report.dup.token += 1,
                DupKind::Near {
                    of,
                    jaccard,
                    estimated,
                } => {
                    report.dup.near += 1;
                    if let Some(origin) = self.path_of.get(of) {
                        report.near_dups.push(NearDupPair {
                            path: rel_path.clone(),
                            duplicate_of: origin.clone(),
                            jaccard: *jaccard,
                            estimated: *estimated,
                        });
                    }
                }
            }
            self.path_of.entry(hash).or_insert_with(|| rel_path.clone());
            let spdx = scan_spdx_tag(&bytes);
            if let Some(expr) = &spdx {
                report.spdx_tags.push(SpdxTag {
                    path: rel_path.clone(),
                    expression: expr.clone(),
                });
            }
            self.cas.record_file(
                repo,
                &rel_path,
                &hash,
                bytes.len() as u64,
                Some(&thash),
                spdx.as_deref(),
            )?;
            report.files_accepted += 1;
        }
        report.license = detect_repo_license(&license_candidates).map(|(file, m)| RepoLicense {
            file,
            spdx: m.spdx,
            confidence: m.confidence,
        });
        self.cas.seal();
        let stats_after = self.cas.stats()?;
        report.bytes_new_unique = stats_after.bytes_raw - stats_before.bytes_raw;
        Ok(report)
    }
}
