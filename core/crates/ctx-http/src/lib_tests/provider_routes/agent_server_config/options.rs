use super::*;

#[tokio::test]
async fn provider_options_preserve_invalid_workspace_error() {
    let fixture = ProviderRouteFixture::new().await;
    let app = fixture.app();

    let req = Request::builder()
        .method("GET")
        .uri("/api/workspaces/not-a-uuid/providers/qwen/options")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"].as_str(), Some("invalid workspace id"));
}

#[tokio::test]
async fn provider_options_preserve_missing_workspace_load_error() {
    let fixture = ProviderRouteFixture::new().await;
    let app = fixture.app();

    let req = Request::builder()
        .method("GET")
        .uri("/api/workspaces/11111111-1111-4111-8111-111111111111/providers/qwen/options")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let error = payload["error"].as_str().unwrap();
    assert!(error.starts_with("failed to load workspace execution settings:"));
    assert!(error.contains("workspace 11111111-1111-4111-8111-111111111111 not found"));
}

#[tokio::test]
async fn provider_options_preserve_execution_settings_context() {
    let git_repo = setup_git_repo().await;
    let fixture = ProviderRouteFixture::new().await;
    let app = fixture.app();
    let workspace = create_workspace_via_api(&app, &git_repo.path().to_string_lossy()).await;
    fixture
        .daemon()
        .seed_invalid_workspace_runtime_settings_document_for_test(workspace.id, "{")
        .await
        .unwrap();

    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/workspaces/{}/providers/qwen/options",
            workspace.id.0
        ))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let error = payload["error"].as_str().unwrap();
    assert!(error.starts_with("failed to load workspace execution settings:"));
    assert!(error.contains(&format!(
        "loading execution settings for workspace {}",
        workspace.id.0
    )));
    assert!(error.contains("workspace runtime settings"));
}

#[tokio::test]
async fn provider_options_preserve_unsupported_provider_error() {
    let git_repo = setup_git_repo().await;
    let fixture = ProviderRouteFixture::new().await;
    let app = fixture.app();
    let workspace = create_workspace_via_api(&app, &git_repo.path().to_string_lossy()).await;

    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/workspaces/{}/providers/missing-provider/options",
            workspace.id.0
        ))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        payload["error"].as_str(),
        Some("unsupported provider id: missing-provider")
    );
}

#[tokio::test]
async fn provider_options_surface_agent_server_config_errors() {
    let git_repo = setup_git_repo().await;
    let fixture = ProviderRouteFixture::new().await;
    write_invalid_agent_server_config(fixture.data_root());
    let app = fixture.app();
    let workspace = create_workspace_via_api(&app, &git_repo.path().to_string_lossy()).await;

    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/workspaces/{}/providers/qwen/options",
            workspace.id.0
        ))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["probe_ok"].as_bool(), Some(false));
    assert!(payload["config_error"]
        .as_str()
        .is_some_and(|value| value.contains("parsing agent server config")));
}

#[tokio::test]
async fn provider_options_ignore_stale_verify_cache_while_agent_server_config_is_broken() {
    let git_repo = setup_git_repo().await;
    let fixture = ProviderRouteFixture::new().await;
    let app = fixture.app();
    let workspace = create_workspace_via_api(&app, &git_repo.path().to_string_lossy()).await;

    let verify_req = Request::builder()
        .method("POST")
        .uri(format!(
            "/api/workspaces/{}/providers/qwen/verify",
            workspace.id.0
        ))
        .body(Body::empty())
        .unwrap();
    let verify_res = app.clone().oneshot(verify_req).await.unwrap();
    assert_eq!(verify_res.status(), StatusCode::OK);

    write_invalid_agent_server_config(fixture.data_root());

    let broken_options_req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/workspaces/{}/providers/qwen/options",
            workspace.id.0
        ))
        .body(Body::empty())
        .unwrap();
    let broken_options_res = app.clone().oneshot(broken_options_req).await.unwrap();
    assert_eq!(broken_options_res.status(), StatusCode::OK);
    let broken_options_body = to_bytes(broken_options_res.into_body(), usize::MAX)
        .await
        .unwrap();
    let broken_payload: serde_json::Value = serde_json::from_slice(&broken_options_body).unwrap();
    assert!(broken_payload["config_error"]
        .as_str()
        .is_some_and(|value| value.contains("parsing agent server config")));
    assert!(
        broken_payload.get("verify").is_none(),
        "stale verify cache should be ignored while config is broken: {broken_payload:#?}"
    );

    clear_agent_server_config(fixture.data_root());

    let repaired_options_req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/workspaces/{}/providers/qwen/options",
            workspace.id.0
        ))
        .body(Body::empty())
        .unwrap();
    let repaired_options_res = app.clone().oneshot(repaired_options_req).await.unwrap();
    assert_eq!(repaired_options_res.status(), StatusCode::OK);
    let repaired_options_body = to_bytes(repaired_options_res.into_body(), usize::MAX)
        .await
        .unwrap();
    let repaired_payload: serde_json::Value =
        serde_json::from_slice(&repaired_options_body).unwrap();
    assert!(
        repaired_payload.get("config_error").is_none(),
        "broken config response should not poison later healthy options responses: {repaired_payload:#?}"
    );
}
