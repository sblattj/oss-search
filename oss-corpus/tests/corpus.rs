mod common;

use common::*;
use oss_corpus::clone::SyncAction;
use oss_corpus::{AdmissionGate, Blake3Hash, CloneCoordinator, CorpusIngestor, Rejection};

#[test]
fn two_repos_sharing_vendored_file_dedup_to_one_blob() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_a = tmp.path().join("alpha");
    let repo_b = tmp.path().join("beta");
    let vendored = "function sharedLibrary(x) { return x * 2 + 1; }\n".repeat(64);
    let main_a = "fn main() { println!(\"alpha\"); }\n".repeat(32);
    let main_b = "console.log('beta entry point');\n".repeat(32);
    fixture_repo(
        &repo_a,
        &[
            ("src/main.rs", main_a.as_str()),
            ("vendor/lib.js", vendored.as_str()),
            ("LICENSE", include_str!("../src/license-data/MIT.txt")),
        ],
    );
    fixture_repo(
        &repo_b,
        &[
            ("index.js", main_b.as_str()),
            ("vendor/lib.js", vendored.as_str()),
        ],
    );

    let cas_root = tmp.path().join("cas");
    let mut ingestor = CorpusIngestor::open(&cas_root).unwrap();
    let report_a = ingestor.ingest_worktree("alpha", &repo_a).unwrap();
    let report_b = ingestor.ingest_worktree("beta", &repo_b).unwrap();

    assert_eq!(report_a.files_accepted, 3);
    assert_eq!(report_b.files_accepted, 2);
    assert!(report_a.rejections.is_empty());
    assert!(report_b.rejections.is_empty());

    let stats = ingestor.cas.stats().unwrap();
    assert_eq!(stats.blobs, 4, "vendored file must collapse: {stats:?}");
    let vendored_hash = Blake3Hash::of(vendored.as_bytes());
    assert!(ingestor.cas.contains(&vendored_hash).unwrap());
    let alpha_row = ingestor
        .cas
        .repo_files("alpha")
        .unwrap()
        .into_iter()
        .find(|row| row.path == "vendor/lib.js")
        .unwrap();
    let beta_row = ingestor
        .cas
        .repo_files("beta")
        .unwrap()
        .into_iter()
        .find(|row| row.path == "vendor/lib.js")
        .unwrap();
    assert_eq!(alpha_row.hash, vendored_hash);
    assert_eq!(beta_row.hash, vendored_hash);
    assert_eq!(beta_row.hash, alpha_row.hash);

    let total_offered = report_a.bytes_offered + report_b.bytes_offered;
    assert!(stats.dedup_ratio > 1.0, "expected savings, got {stats:?}");
    assert_eq!(stats.bytes_offered, total_offered);
    let recovered = ingestor.cas.get(&vendored_hash).unwrap();
    assert_eq!(recovered, vendored.as_bytes());

    let license = report_a.license.as_ref().unwrap();
    assert_eq!(license.spdx, "MIT");
    assert_eq!(license.file, "LICENSE");
    assert!(report_b.license.is_none());
}

#[test]
fn admission_gates_reject_oversized_and_binary_with_named_reasons() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("gated");
    let oversized = "x".repeat(2 * 1024 * 1024);
    let binary: Vec<u8> = b"MH payload text \x00\x00\x00 more text \x00 trailing"
        .repeat(93);
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/ok.rs"), b"fn main() {}\n").unwrap();
    std::fs::write(repo.join("big.txt"), &oversized).unwrap();
    std::fs::write(repo.join("blob.bin"), &binary).unwrap();

    let mut ingestor = CorpusIngestor::open(tmp.path().join("cas")).unwrap();
    let report = ingestor.ingest_worktree("gated", &repo).unwrap();

    assert_eq!(report.files_total, 3);
    assert_eq!(report.files_accepted, 1);
    assert_eq!(report.rejections.len(), 2);
    let reasons: Vec<&Rejection> = report.rejections.iter().map(|r| &r.reason).collect();
    assert!(reasons.iter().any(|r| matches!(
        r,
        Rejection::TooLarge { size, cap } if *size == 2 * 1024 * 1024 && *cap == 1024 * 1024
    )));
    assert!(reasons.iter().any(|r| matches!(r, Rejection::BinaryNul { .. })));
    let paths: Vec<&str> = report.rejections.iter().map(|r| r.path.as_str()).collect();
    assert!(paths.contains(&"big.txt"));
    assert!(paths.contains(&"blob.bin"));
}

#[test]
fn minhash_flags_near_duplicate_pair_and_exact_verifies() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("neardup");
    let base = source_lines(200);
    let variant = modified_lines(200);
    let unrelated = (0..200)
        .map(|i| format!("struct Item{i} {{ field{i}: String, other{i}: u64 }}\n"))
        .collect::<String>();
    fixture_repo(
        &repo,
        &[
            ("src/base.rs", &base),
            ("src/variant.rs", &variant),
            ("src/other.rs", &unrelated),
        ],
    );

    let mut ingestor = CorpusIngestor::open(tmp.path().join("cas")).unwrap();
    let report = ingestor.ingest_worktree("neardup", &repo).unwrap();
    assert_eq!(report.files_accepted, 3);
    assert_eq!(report.dup.unique, 2);
    assert_eq!(report.dup.near, 1);
    assert_eq!(report.near_dups.len(), 1);
    let pair = &report.near_dups[0];
    assert_eq!(pair.path, "src/variant.rs");
    assert_eq!(pair.duplicate_of, "src/base.rs");
    assert!(
        pair.jaccard >= 0.85 && pair.jaccard <= 0.97,
        "jaccard {}",
        pair.jaccard
    );
    assert!(pair.estimated > 0.0);
}

#[test]
fn token_tier_collapses_whitespace_variants() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("tokendup");
    let original = "fn alpha() {\n    let beta = compute(gamma) + delta;\n}\n".repeat(20);
    let reformatted = "fn alpha() {\n\tlet  beta=compute( gamma )+delta;\n}\n".repeat(20);
    fixture_repo(
        &repo,
        &[("src/a.rs", &original), ("src/b.rs", &reformatted)],
    );
    let mut ingestor = CorpusIngestor::open(tmp.path().join("cas")).unwrap();
    let report = ingestor.ingest_worktree("tokendup", &repo).unwrap();
    assert_eq!(report.dup.unique, 1);
    assert_eq!(report.dup.token, 1);
    assert_eq!(report.dup.near, 0);
    assert_eq!(ingestor.cas.stats().unwrap().blobs, 2);
}

#[test]
fn spdx_tags_recorded_per_file() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("tags");
    let tagged = "// SPDX-License-Identifier: Apache-2.0\npub fn api() -> u32 { 7 }\n";
    let untagged = "pub fn plain() -> u32 { 8 }\n";
    fixture_repo(&repo, &[("src/api.rs", tagged), ("src/plain.rs", untagged)]);
    let mut ingestor = CorpusIngestor::open(tmp.path().join("cas")).unwrap();
    let report = ingestor.ingest_worktree("tags", &repo).unwrap();
    assert_eq!(report.spdx_tags.len(), 1);
    assert_eq!(report.spdx_tags[0].path, "src/api.rs");
    assert_eq!(report.spdx_tags[0].expression, "Apache-2.0");
}

#[test]
fn fork_map_stored_as_metadata() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("fork");
    fixture_repo(&repo, &[("README.md", "fork of alpha\n")]);
    let mut ingestor = CorpusIngestor::open(tmp.path().join("cas")).unwrap();
    ingestor.set_fork_map(&[("fork", "alpha")]).unwrap();
    let report = ingestor.ingest_worktree("fork", &repo).unwrap();
    assert_eq!(report.canonical.as_deref(), Some("alpha"));
    assert_eq!(ingestor.cas.canonical_for("fork").unwrap().as_deref(), Some("alpha"));
    assert_eq!(ingestor.cas.canonical_for("alpha").unwrap(), None);
}

#[tokio::test]
async fn clone_materializes_and_update_path_logs_decisions() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = tmp.path().join("origin");
    let gate = AdmissionGate {
        max_bytes: 64,
        ..Default::default()
    };
    let root = tmp.path().join("corpus");
    fixture_repo(
        &origin,
        &[
            ("main.rs", "fn main() { println!(\"one\"); }\n"),
            ("src/lib.rs", "pub fn lib_one() -> u8 { 1 }\n"),
            ("big.dat", "0123456789012345678901234567890123456789012345678901234567890123456789"),
        ],
    );
    let coordinator = CloneCoordinator::new(&root).with_gate(gate).with_concurrency(2);
    let url = file_url(&origin);
    let reports = coordinator.clone_all(&[url.as_str()]).await;
    assert_eq!(reports.len(), 1);
    let report = &reports[0];
    assert!(report.error.is_none(), "clone error: {:?}", report.error);
    assert_eq!(report.repo, "origin");
    assert_eq!(report.action, Some(SyncAction::Cloned));
    assert!(report.accepted.contains(&"main.rs".to_string()));
    assert!(report.accepted.contains(&"src/lib.rs".to_string()));
    assert_eq!(report.rejected.len(), 1);
    assert_eq!(report.rejected[0].path, "big.dat");
    assert!(matches!(
        report.rejected[0].reason,
        Rejection::TooLarge { .. }
    ));
    let work = root.join("work/origin");
    assert!(work.join("main.rs").exists());
    assert!(work.join("src/lib.rs").exists());
    assert!(!work.join("big.dat").exists());

    let reports = coordinator.clone_all(&[url.as_str()]).await;
    let report = &reports[0];
    assert!(report.error.is_none(), "sync error: {:?}", report.error);
    match &report.action {
        Some(SyncAction::Unchanged { tip }) => assert_eq!(tip.len(), 40),
        other => panic!("expected Unchanged, got {other:?}"),
    }
    assert!(report.accepted.is_empty(), "unchanged tip must not re-materialize");

    commit_files(
        &origin,
        &[("added.rs", "pub fn added() -> bool { true }\n")],
        "second commit moves tip",
    );
    let reports = coordinator.clone_all(&[url.as_str()]).await;
    let report = &reports[0];
    assert!(report.error.is_none(), "update error: {:?}", report.error);
    match &report.action {
        Some(SyncAction::Updated { old_tip, new_tip }) => {
            assert_ne!(old_tip, new_tip);
            assert!(new_tip.len() == 40);
        }
        other => panic!("expected Updated, got {other:?}"),
    }
    assert!(report.accepted.contains(&"added.rs".to_string()));
    let work = root.join("work/origin");
    assert!(work.join("added.rs").exists());
    assert!(work.join("main.rs").exists());
}

#[tokio::test]
async fn parallel_clone_eight_way() {
    let tmp = tempfile::tempdir().unwrap();
    let mut urls = Vec::new();
    for i in 0..8 {
        let origin = tmp.path().join(format!("repo{i}"));
        fixture_repo(
            &origin,
            &[("lib.rs", format!("pub fn f{i}() -> u8 {{ {i} }}\n").as_str())],
        );
        urls.push(file_url(&origin));
    }
    let url_refs: Vec<&str> = urls.iter().map(String::as_str).collect();
    let coordinator = CloneCoordinator::new(tmp.path().join("corpus")).with_concurrency(8);
    let reports = coordinator.clone_all(&url_refs).await;
    assert_eq!(reports.len(), 8);
    for (i, report) in reports.iter().enumerate() {
        assert!(report.error.is_none(), "repo {i} error: {:?}", report.error);
        assert_eq!(report.accepted, vec!["lib.rs".to_string()]);
    }
}

#[test]
fn ingest_worktree_walks_nested_tree_in_sorted_order() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("walk");
    fixture_repo(
        &repo,
        &[
            ("z.txt", "zed\n"),
            ("a/b.txt", "deep\n"),
            ("a/b2.txt", "shallow\n"),
        ],
    );
    let mut ingestor = CorpusIngestor::open(tmp.path().join("cas")).unwrap();
    let report = ingestor.ingest_worktree("walk", &repo).unwrap();
    assert_eq!(report.files_accepted, 3);
    let rows = ingestor.cas.repo_files("walk").unwrap();
    let paths: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
    assert_eq!(paths, vec!["a/b.txt", "a/b2.txt", "z.txt"]);
}
