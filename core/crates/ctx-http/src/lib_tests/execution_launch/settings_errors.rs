use super::*;

#[tokio::test]
async fn execution_launch_start_returns_bad_request_when_execution_settings_fail() {
    let _serial = home_env_test_lock().lock().await;
    let git_repo = setup_git_repo().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());

    let data_dir = tempfile::tempdir().unwrap();
    let fixture = test_daemon_fixture_with_fake_provider_for_test(data_dir.path(), None).await;
    let state = fixture.daemon();
    let app = fixture.router();

    let workspace = create_workspace_via_api(&app, &git_repo.path().to_string_lossy()).await;
    state
        .seed_invalid_workspace_runtime_settings_document_for_test(workspace.id, "{")
        .await
        .unwrap();

    let req = Request::builder()
        .method("POST")
        .uri("/api/execution/launch/start")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "workspace_id": workspace.id.0.to_string(),
            })
            .to_string(),
        ))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(value["error"]
        .as_str()
        .unwrap_or_default()
        .contains("workspace runtime settings"));
}

#[tokio::test]
async fn ensure_workspace_container_returns_bad_request_when_execution_settings_fail() {
    let _serial = home_env_test_lock().lock().await;
    let git_repo = setup_git_repo().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());

    let data_dir = tempfile::tempdir().unwrap();
    let fixture = test_daemon_fixture_with_fake_provider_for_test(data_dir.path(), None).await;
    let state = fixture.daemon();
    let app = fixture.router();

    let workspace = create_workspace_via_api(&app, &git_repo.path().to_string_lossy()).await;
    state
        .seed_invalid_workspace_runtime_settings_document_for_test(workspace.id, "{")
        .await
        .unwrap();

    let req = Request::builder()
        .method("POST")
        .uri(format!(
            "/api/workspaces/{}/harness_container/ensure",
            workspace.id.0
        ))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(value["error"]
        .as_str()
        .unwrap_or_default()
        .contains("workspace runtime settings"));
}
