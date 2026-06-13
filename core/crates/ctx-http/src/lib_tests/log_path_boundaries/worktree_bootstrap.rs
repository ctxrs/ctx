use super::*;

#[tokio::test]
async fn worktree_bootstrap_logs_return_in_root_log_file() {
    let fixture = build_log_path_fixture().await;
    let (_task, session) = create_task_with_primary_session(&fixture).await;

    let log_dir =
        ctx_observability::logs::logs_dir(fixture.data_dir.path()).join("worktree-bootstrap");
    std::fs::create_dir_all(&log_dir).unwrap();
    let log_path = log_dir.join("bootstrap.log");
    std::fs::write(&log_path, b"inside bootstrap log\n").unwrap();

    let worktree_id = fixture
        .daemon()
        .record_worktree_bootstrap_log_for_test(
            &session,
            WorktreeBootstrapStatus::Success,
            &log_path,
            None,
            "true",
        )
        .await
        .unwrap();

    let res = bootstrap_logs_response(&fixture.app, worktree_id).await;
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    assert_eq!(body.as_ref(), b"inside bootstrap log\n");
}

#[tokio::test]
async fn worktree_bootstrap_logs_fail_closed_for_legacy_outside_paths() {
    let fixture = build_log_path_fixture().await;
    let (_task, session) = create_task_with_primary_session(&fixture).await;

    let outside_dir = tempfile::tempdir().unwrap();
    let outside_path = outside_dir.path().join("bootstrap.log");
    std::fs::write(&outside_path, b"outside bootstrap log\n").unwrap();

    let worktree_id = fixture
        .daemon()
        .record_worktree_bootstrap_log_for_test(
            &session,
            WorktreeBootstrapStatus::Failed,
            &outside_path,
            Some("legacy outside path"),
            "false",
        )
        .await
        .unwrap();

    let res = bootstrap_logs_response(&fixture.app, worktree_id).await;
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
