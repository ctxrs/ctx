use super::*;

#[tokio::test]
async fn spawn_agent_surfaces_agent_server_config_errors() {
    let _serial = home_env_test_lock().lock().await;
    let git_repo = setup_git_repo().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());
    let data_dir = tempfile::tempdir().unwrap();
    let (fixture, app, session) =
        build_fake_app_with_session(data_dir.path(), &git_repo.path().to_string_lossy()).await;
    let daemon = fixture.daemon();
    {
        let mut statuses = HashMap::new();
        statuses.insert(
            "qwen".into(),
            ProviderStatus {
                provider_id: "qwen".into(),
                installed: true,
                detected_path: None,
                version: Some("0.1.0".into()),
                capabilities: None,
                health: ctx_providers::adapters::ProviderHealth::Ok,
                diagnostics: vec![],
                details: HashMap::new(),
                usability: ctx_providers::adapters::ProviderUsability::default(),
            },
        );
        daemon.replace_provider_statuses(statuses).await;
    }
    write_invalid_agent_server_config(daemon.data_root());

    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/mcp/sessions/{}/spawn_agent", session.id.0))
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "worktree": "inherit",
                "task_label": "q1",
                "prompt": "test prompt",
                "harness": "qwen",
                "model": "qwen2.5-coder-32b-instruct"
            })
            .to_string(),
        ))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(payload["error"]
        .as_str()
        .is_some_and(|value| value.contains("parsing agent server config")));
}

#[tokio::test]
async fn authenticate_session_surfaces_agent_server_config_errors() {
    let _serial = home_env_test_lock().lock().await;
    let git_repo = setup_git_repo().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());
    let data_dir = tempfile::tempdir().unwrap();
    let (fixture, app, session) =
        build_fake_app_with_session(data_dir.path(), &git_repo.path().to_string_lossy()).await;
    let daemon = fixture.daemon();
    write_invalid_agent_server_config(daemon.data_root());

    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/sessions/{}/authenticate", session.id.0))
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let error = payload["error"].as_str().unwrap_or("");
    assert!(
        error.contains("parsing agent server config"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn set_session_model_surfaces_agent_server_config_errors() {
    let _serial = home_env_test_lock().lock().await;
    let git_repo = setup_git_repo().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());
    let data_dir = tempfile::tempdir().unwrap();
    let (fixture, app, session) =
        build_fake_app_with_session(data_dir.path(), &git_repo.path().to_string_lossy()).await;
    let daemon = fixture.daemon();
    write_invalid_agent_server_config(daemon.data_root());

    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/sessions/{}/model", session.id.0))
        .header("content-type", "application/json")
        .body(Body::from(json!({"model_id":"fake-model"}).to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let error = payload["error"].as_str().unwrap_or("");
    assert!(
        error.contains("parsing agent server config"),
        "unexpected error: {error}"
    );
}
