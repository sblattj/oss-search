use oss_corpus::{CloneCoordinator, CorpusIngestor};

#[tokio::test]
#[ignore = "requires network access to github.com"]
async fn clones_two_tiny_real_repos_end_to_end() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("corpus");
    let coordinator = CloneCoordinator::new(&root);
    let urls = [
        "https://github.com/octocat/Hello-World",
        "https://github.com/octocat/Spoon-Knife",
    ];
    let reports = coordinator.clone_all(&urls).await;
    assert_eq!(reports.len(), 2);
    for report in &reports {
        assert!(report.error.is_none(), "{} error: {:?}", report.repo, report.error);
        assert!(!report.accepted.is_empty(), "{} materialized nothing", report.repo);
    }
    let hello = reports
        .iter()
        .find(|r| r.repo == "Hello-World")
        .expect("Hello-World cloned");
    assert!(hello.accepted.iter().any(|p| p.eq_ignore_ascii_case("readme")));

    let mut ingestor = CorpusIngestor::open(tmp.path().join("cas")).unwrap();
    let hello_report = ingestor
        .ingest_worktree("Hello-World", &coordinator.work_root("Hello-World"))
        .unwrap();
    let spoon_report = ingestor
        .ingest_worktree("Spoon-Knife", &coordinator.work_root("Spoon-Knife"))
        .unwrap();
    assert!(hello_report.files_accepted > 0);
    assert!(spoon_report.files_accepted > 0);
    let stats = ingestor.cas.stats().unwrap();
    assert!(stats.blobs > 0);
    assert!(stats.bytes_raw > 0);
}
