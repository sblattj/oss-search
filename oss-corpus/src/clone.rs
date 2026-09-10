use crate::admission::{AdmissionGate, Rejection};
use crate::Result;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use std::process::Stdio;
use tokio::process::Command;
use tokio::task::JoinSet;

pub const DEFAULT_CONCURRENCY: usize = 8;
const CAT_FILE_CHUNK: usize = 128;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum SyncAction {
    Cloned,
    Updated { old_tip: String, new_tip: String },
    Unchanged { tip: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct FileRejection {
    pub path: String,
    pub reason: Rejection,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RepoCloneReport {
    pub url: String,
    pub repo: String,
    pub action: Option<SyncAction>,
    pub error: Option<String>,
    pub accepted: Vec<String>,
    pub rejected: Vec<FileRejection>,
    pub errors: Vec<String>,
}

#[derive(Clone)]
pub struct CloneCoordinator {
    root: PathBuf,
    gate: AdmissionGate,
    concurrency: usize,
}

impl CloneCoordinator {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            gate: AdmissionGate::default(),
            concurrency: DEFAULT_CONCURRENCY,
        }
    }

    pub fn with_gate(mut self, gate: AdmissionGate) -> Self {
        self.gate = gate;
        self
    }

    pub fn with_concurrency(mut self, concurrency: usize) -> Self {
        self.concurrency = concurrency.max(1);
        self
    }

    pub fn repo_root(&self, repo: &str) -> PathBuf {
        self.root.join("repos").join(repo)
    }

    pub fn work_root(&self, repo: &str) -> PathBuf {
        self.root.join("work").join(repo)
    }

    pub async fn clone_all(&self, urls: &[&str]) -> Vec<RepoCloneReport> {
        let mut results = Vec::with_capacity(urls.len());
        let mut set: JoinSet<RepoCloneReport> = JoinSet::new();
        for url in urls {
            while set.len() >= self.concurrency {
                if let Some(done) = set.join_next().await {
                    results.push(done.unwrap_or_else(|e| RepoCloneReport {
                        url: String::new(),
                        repo: String::new(),
                        action: None,
                        error: Some(format!("clone task failed: {e}")),
                        accepted: Vec::new(),
                        rejected: Vec::new(),
                        errors: Vec::new(),
                    }));
                }
            }
            set.spawn(sync_repo(
                self.root.clone(),
                self.gate.clone(),
                (*url).to_string(),
            ));
        }
        while let Some(done) = set.join_next().await {
            if let Ok(report) = done {
                results.push(report);
            }
        }
        results
    }

    pub async fn sync_repo(&self, url: &str) -> RepoCloneReport {
        sync_repo(self.root.clone(), self.gate.clone(), url.to_string()).await
    }
}

fn repo_name_from_url(url: &str) -> String {
    let trimmed = url.trim_end_matches('/');
    let last = trimmed.rsplit('/').next().unwrap_or(trimmed);
    last.strip_suffix(".git").unwrap_or(last).to_string()
}

fn safe_rel_path(path: &str) -> bool {
    !path.starts_with('/')
        && !path.contains('\\')
        && path
            .split('/')
            .all(|component| component != ".." && !component.is_empty())
}

async fn run_git(dir: Option<&Path>, context: &str, args: &[&str]) -> Result<String> {
    let mut cmd = Command::new("git");
    if let Some(d) = dir {
        cmd.current_dir(d);
    }
    let out = cmd
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()
        .await?;
    if !out.status.success() {
        return Err(crate::Error::Git {
            context: context.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

async fn sync_repo(root: PathBuf, gate: AdmissionGate, url: String) -> RepoCloneReport {
    let repo = repo_name_from_url(&url);
    let mut report = RepoCloneReport {
        url: url.clone(),
        repo: repo.clone(),
        ..Default::default()
    };
    let repo_dir = root.join("repos").join(&repo);
    let work_dir = root.join("work").join(&repo);
    let result = sync_repo_inner(&repo_dir, &work_dir, &url, &gate, &mut report).await;
    if let Err(e) = result {
        report.error = Some(e.to_string());
    }
    report
}

async fn sync_repo_inner(
    repo_dir: &Path,
    work_dir: &Path,
    url: &str,
    gate: &AdmissionGate,
    report: &mut RepoCloneReport,
) -> Result<()> {
    if repo_dir.join(".git").exists() {
        update_existing(repo_dir, work_dir, url, gate, report).await
    } else {
        if let Some(parent) = repo_dir.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        run_git(
            parent_of(repo_dir),
            "clone",
            &[
                "clone",
                "--depth",
                "1",
                "--single-branch",
                "--no-checkout",
                "--filter=blob:none",
                url,
                repo_dir.to_str().unwrap_or("repo"),
            ],
        )
        .await?;
        report.action = Some(SyncAction::Cloned);
        materialize(repo_dir, work_dir, gate, report).await
    }
}

fn parent_of(path: &Path) -> Option<&Path> {
    path.parent().filter(|p| !p.as_os_str().is_empty())
}

async fn update_existing(
    repo_dir: &Path,
    work_dir: &Path,
    url: &str,
    gate: &AdmissionGate,
    report: &mut RepoCloneReport,
) -> Result<()> {
    let branch = run_git(Some(repo_dir), "symbolic-ref", &["symbolic-ref", "--short", "HEAD"])
        .await?
        .trim()
        .to_string();
    if branch.is_empty() {
        return Err(crate::Error::Git {
            context: "detached HEAD, cannot update".into(),
            stderr: String::new(),
        });
    }
    let local_tip = run_git(Some(repo_dir), "rev-parse", &["rev-parse", "HEAD"])
        .await?
        .trim()
        .to_string();
    let remote_ref = format!("refs/heads/{branch}");
    let ls = run_git(
        Some(repo_dir),
        "ls-remote",
        &["ls-remote", url, &remote_ref],
    )
    .await?;
    let remote_tip = ls
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().next())
        .unwrap_or_default()
        .to_string();
    if remote_tip.is_empty() {
        return Err(crate::Error::Git {
            context: format!("ls-remote found no {remote_ref}"),
            stderr: ls.trim().to_string(),
        });
    }
    if remote_tip == local_tip {
        report.action = Some(SyncAction::Unchanged { tip: local_tip });
        if !work_dir.exists() || tokio::fs::read_dir(work_dir).await?.next_entry().await?.is_none() {
            materialize(repo_dir, work_dir, gate, report).await?;
        }
        return Ok(());
    }
    run_git(
        Some(repo_dir),
        "fetch",
        &["fetch", "--depth", "1", "origin", &remote_ref],
    )
    .await?;
    run_git(
        Some(repo_dir),
        "update-ref",
        &["update-ref", &remote_ref, &remote_tip],
    )
    .await?;
    report.action = Some(SyncAction::Updated {
        old_tip: local_tip,
        new_tip: remote_tip,
    });
    materialize(repo_dir, work_dir, gate, report).await
}

struct TreeEntry {
    oid: String,
    path: String,
}

async fn materialize(
    repo_dir: &Path,
    work_dir: &Path,
    gate: &AdmissionGate,
    report: &mut RepoCloneReport,
) -> Result<()> {
    let out = run_git(
        Some(repo_dir),
        "ls-tree",
        &["ls-tree", "-r", "-l", "-z", "HEAD"],
    )
    .await?;
    let text = out;
    let mut pending: Vec<TreeEntry> = Vec::new();
    for entry in text.split('\0').filter(|e| !e.is_empty()) {
        let Some((meta, path)) = entry.split_once('\t') else {
            continue;
        };
        let fields: Vec<&str> = meta.split_whitespace().collect();
        if fields.len() < 4 {
            continue;
        }
        let (mode, kind, oid, size) = (fields[0], fields[1], fields[2], fields[3]);
        if kind != "blob" || mode == "120000" || mode == "160000" {
            continue;
        }
        let Ok(size) = size.parse::<u64>() else {
            continue;
        };
        if !safe_rel_path(path) {
            report.errors.push(format!("skipped unsafe path {path}"));
            continue;
        }
        if size > gate.max_bytes {
            report.rejected.push(FileRejection {
                path: path.to_string(),
                reason: Rejection::TooLarge {
                    size,
                    cap: gate.max_bytes,
                },
            });
            continue;
        }
        pending.push(TreeEntry {
            oid: oid.to_string(),
            path: path.to_string(),
        });
    }
    if work_dir.exists() {
        tokio::fs::remove_dir_all(work_dir).await?;
    }
    if pending.is_empty() {
        return Ok(());
    }
    let mut child = Command::new("git")
        .arg("cat-file")
        .arg("--batch")
        .current_dir(repo_dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().expect("cat-file stdin");
    let stdout = child.stdout.take().expect("cat-file stdout");
    let mut reader = BufReader::new(stdout);
    let mut paths_by_oid: HashMap<String, String> = HashMap::new();
    for entry in &pending {
        paths_by_oid.insert(entry.oid.clone(), entry.path.clone());
    }
    let mut failure: Option<crate::Error> = None;
    for chunk in pending.chunks(CAT_FILE_CHUNK) {
        for entry in chunk {
            if stdin.write_all(entry.oid.as_bytes()).await.is_err() {
                failure = Some(crate::Error::StreamTruncated);
                break;
            }
            if stdin.write_all(b"\n").await.is_err() {
                failure = Some(crate::Error::StreamTruncated);
                break;
            }
        }
        if failure.is_some() {
            break;
        }
        stdin.flush().await?;
        for _ in 0..chunk.len() {
            match read_frame(&mut reader).await {
                Ok(Some((oid, kind, data))) => {
                    let Some(path) = paths_by_oid.get(&oid) else {
                        continue;
                    };
                    if kind == "missing" {
                        report.errors.push(format!("blob missing for {path}"));
                        continue;
                    }
                    match gate.check(&data) {
                        Ok(()) => {
                            let target = work_dir.join(path);
                            if let Some(parent) = target.parent() {
                                let _ = tokio::fs::create_dir_all(parent).await;
                            }
                            match tokio::fs::write(&target, &data).await {
                                Ok(()) => report.accepted.push(path.clone()),
                                Err(e) => report.errors.push(format!("write {path}: {e}")),
                            }
                        }
                        Err(reason) => report.rejected.push(FileRejection {
                            path: path.clone(),
                            reason,
                        }),
                    }
                }
                Ok(None) => {
                    failure = Some(crate::Error::StreamTruncated);
                    break;
                }
                Err(e) => {
                    failure = Some(e);
                    break;
                }
            }
        }
        if failure.is_some() {
            break;
        }
    }
    drop(stdin);
    let status = child.wait().await?;
    if let Some(e) = failure {
        return Err(e);
    }
    if !status.success() {
        return Err(crate::Error::Git {
            context: "cat-file --batch".into(),
            stderr: format!("exit status {status}"),
        });
    }
    Ok(())
}

async fn read_frame(
    reader: &mut BufReader<tokio::process::ChildStdout>,
) -> Result<Option<(String, String, Vec<u8>)>> {
    let mut header = String::new();
    if reader.read_line(&mut header).await? == 0 {
        return Ok(None);
    }
    let fields: Vec<&str> = header.split_whitespace().collect();
    if fields.len() == 2 && fields[1] == "missing" {
        return Ok(Some((fields[0].to_string(), "missing".to_string(), Vec::new())));
    }
    if fields.len() < 3 {
        return Err(crate::Error::StreamTruncated);
    }
    let size: usize = fields[2].parse().map_err(|_| crate::Error::StreamTruncated)?;
    let mut data = vec![0u8; size];
    reader.read_exact(&mut data).await?;
    let mut newline = [0u8; 1];
    reader.read_exact(&mut newline).await?;
    Ok(Some((fields[0].to_string(), fields[1].to_string(), data)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_name_extraction() {
        assert_eq!(
            repo_name_from_url("https://github.com/octocat/Hello-World"),
            "Hello-World"
        );
        assert_eq!(
            repo_name_from_url("https://github.com/octocat/Hello-World.git/"),
            "Hello-World"
        );
        assert_eq!(repo_name_from_url("file:///tmp/fixtures/alpha"), "alpha");
    }

    #[test]
    fn safe_paths() {
        assert!(safe_rel_path("src/main.rs"));
        assert!(safe_rel_path("a/b/c.txt"));
        assert!(!safe_rel_path("../escape.rs"));
        assert!(!safe_rel_path("/abs/path.rs"));
        assert!(!safe_rel_path("a//b"));
        assert!(!safe_rel_path("back\\slash"));
    }
}
