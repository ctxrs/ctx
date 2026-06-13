use super::*;

#[tokio::test]
async fn provider_list_invalid_target_preserves_bare_bad_request() {
    let fixture = ProviderRouteFixture::new().await;
    let app = fixture.app();

    let req = Request::builder()
        .method("GET")
        .uri("/api/providers?target=definitely-invalid")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    assert!(
        body.is_empty(),
        "list providers invalid target should preserve bare 400 body"
    );
}

#[tokio::test]
async fn provider_get_invalid_target_preserves_json_error() {
    let fixture = ProviderRouteFixture::new().await;
    let app = fixture.app();

    let req = Request::builder()
        .method("GET")
        .uri("/api/providers/qwen?target=definitely-invalid")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        payload["error"].as_str(),
        Some(
            "invalid install target 'definitely-invalid'; expected host, container, linux-aarch64, or linux-x86_64"
        )
    );
}

#[tokio::test]
async fn provider_get_missing_provider_preserves_json_error() {
    let fixture = ProviderRouteFixture::new().await;
    let app = fixture.app();

    let req = Request::builder()
        .method("GET")
        .uri("/api/providers/missing-provider")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        payload["error"].as_str(),
        Some("provider not found: missing-provider")
    );
}

#[tokio::test]
async fn provider_status_empty_target_query_still_uses_default_target() {
    let fixture = ProviderRouteFixture::new().await;
    let app = fixture.app();

    let list_req = Request::builder()
        .method("GET")
        .uri("/api/providers?target=")
        .body(Body::empty())
        .unwrap();
    let list_res = app.clone().oneshot(list_req).await.unwrap();
    assert_eq!(list_res.status(), StatusCode::OK);

    let get_req = Request::builder()
        .method("GET")
        .uri("/api/providers/qwen?target=")
        .body(Body::empty())
        .unwrap();
    let get_res = app.clone().oneshot(get_req).await.unwrap();
    assert_eq!(get_res.status(), StatusCode::OK);
}

#[tokio::test]
async fn provider_authenticate_without_body_preserves_invalid_workspace_error() {
    let fixture = ProviderRouteFixture::new().await;
    let app = fixture.app();

    let req = Request::builder()
        .method("POST")
        .uri("/api/workspaces/not-a-uuid/providers/qwen/authenticate")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"].as_str(), Some("invalid workspace id"));
}

#[tokio::test]
async fn provider_verify_preserves_execution_settings_context() {
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
        .method("POST")
        .uri(format!(
            "/api/workspaces/{}/providers/qwen/verify",
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
async fn provider_authenticate_preserves_execution_settings_context() {
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
        .method("POST")
        .uri(format!(
            "/api/workspaces/{}/providers/qwen/authenticate",
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
async fn provider_verify_surfaces_agent_server_config_errors() {
    let git_repo = setup_git_repo().await;
    let fixture = ProviderRouteFixture::new().await;
    write_invalid_agent_server_config(fixture.data_root());
    let app = fixture.app();
    let workspace = create_workspace_via_api(&app, &git_repo.path().to_string_lossy()).await;

    let req = Request::builder()
        .method("POST")
        .uri(format!(
            "/api/workspaces/{}/providers/qwen/verify",
            workspace.id.0
        ))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["status"].as_str(), Some("error"));
    assert!(payload["message"]
        .as_str()
        .is_some_and(|value| value.contains("parsing agent server config")));
}

#[tokio::test]
async fn provider_get_surfaces_agent_server_config_errors_in_status() {
    let fixture = ProviderRouteFixture::new().await;
    write_invalid_agent_server_config(fixture.data_root());
    let app = fixture.app();

    let req = Request::builder()
        .method("GET")
        .uri("/api/providers/qwen")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["health"].as_str(), Some("error"));
    assert_eq!(
        payload
            .pointer("/usability/reason_code")
            .and_then(|v| v.as_str()),
        Some("managed_config_error")
    );
    assert!(payload["diagnostics"]
        .as_array()
        .is_some_and(|values| values.iter().any(|value| value
            .as_str()
            .is_some_and(|message| message.contains("parsing agent server config")))));
}

#[tokio::test]
async fn provider_bootstrap_marks_statuses_with_agent_server_config_errors() {
    let git_repo = setup_git_repo().await;
    let fixture = ProviderRouteFixture::new().await;
    write_invalid_agent_server_config(fixture.data_root());
    let app = fixture.app();
    let workspace = create_workspace_via_api(&app, &git_repo.path().to_string_lossy()).await;

    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/workspaces/{}/providers/bootstrap",
            workspace.id.0
        ))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let qwen = payload["providers"]
        .as_array()
        .and_then(|providers| {
            providers
                .iter()
                .find(|provider| provider["provider_id"].as_str() == Some("qwen"))
        })
        .expect("qwen bootstrap status");
    assert_eq!(qwen["health"].as_str(), Some("error"));
    assert_eq!(
        qwen.pointer("/usability/reason_code")
            .and_then(|v| v.as_str()),
        Some("managed_config_error")
    );
}

#[tokio::test]
async fn provider_authenticate_surfaces_agent_server_config_errors() {
    let git_repo = setup_git_repo().await;
    let fixture = ProviderRouteFixture::new().await;
    write_invalid_agent_server_config(fixture.data_root());
    let app = fixture.app();
    let workspace = create_workspace_via_api(&app, &git_repo.path().to_string_lossy()).await;

    let req = Request::builder()
        .method("POST")
        .uri(format!(
            "/api/workspaces/{}/providers/qwen/authenticate",
            workspace.id.0
        ))
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["status"].as_str(), Some("error"));
    assert_eq!(payload["auth_required"].as_bool(), Some(false));
    assert!(payload["message"]
        .as_str()
        .is_some_and(|value| value.contains("parsing agent server config")));
}
