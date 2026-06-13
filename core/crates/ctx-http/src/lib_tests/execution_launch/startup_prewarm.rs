use super::*;

#[tokio::test]
async fn execution_launch_startup_prewarm_kind_supported() {
    let _serial = sandbox_cli_env_test_lock().lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());

    let data_dir = tempfile::tempdir().unwrap();
    let sandbox_cli_path = write_failing_sandbox_cli_shim(data_dir.path());
    let _sandbox_cli = EnvVarGuard::set(
        "CTX_HARNESS_SANDBOX_CLI_PATH",
        &sandbox_cli_path.to_string_lossy(),
    );
    let fixture = test_daemon_fixture_with_fake_provider_for_test(data_dir.path(), None).await;
    let app = fixture.router();

    let req = Request::builder()
        .method("POST")
        .uri("/api/execution/launch/start")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "kind": "startup_prewarm",
                "prewarm_scope": "builder",
            })
            .to_string(),
        ))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let snapshot: ExecutionLaunchSnapshot = serde_json::from_slice(&body).unwrap();
    assert_eq!(snapshot.kind, ExecutionSetupJobKind::StartupPrewarm);
    assert!(!snapshot.job_id.trim().is_empty());
    assert!(matches!(
        snapshot.state,
        ExecutionLaunchState::Running | ExecutionLaunchState::Error
    ));

    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/execution/launch/status?job_id={}",
            snapshot.job_id
        ))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let mut status_snapshot: ExecutionLaunchSnapshot = serde_json::from_slice(&body).unwrap();
    assert_eq!(status_snapshot.job_id, snapshot.job_id);
    assert_eq!(status_snapshot.kind, ExecutionSetupJobKind::StartupPrewarm);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    while matches!(status_snapshot.state, ExecutionLaunchState::Running)
        && tokio::time::Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(25)).await;
        let req = Request::builder()
            .method("GET")
            .uri(format!(
                "/api/execution/launch/status?job_id={}",
                snapshot.job_id
            ))
            .body(Body::empty())
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        status_snapshot = serde_json::from_slice(&body).unwrap();
    }
    assert!(matches!(
        status_snapshot.state,
        ExecutionLaunchState::Running | ExecutionLaunchState::Ready | ExecutionLaunchState::Error
    ));
}
