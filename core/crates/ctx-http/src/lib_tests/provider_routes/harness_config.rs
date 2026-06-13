use super::*;

#[tokio::test]
async fn provider_options_surface_harness_config_errors() {
    let git_repo = setup_git_repo().await;
    let fixture = ProviderRouteFixture::new().await;
    write_invalid_harness_registry(fixture.data_root());
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
    assert_eq!(payload["auth_mode"].as_str(), Some("none"));
    assert!(payload["config_error"]
        .as_str()
        .is_some_and(|value| value.contains("parsing harness source registry")));
}

#[tokio::test]
async fn provider_bootstrap_surfaces_harness_config_errors() {
    let git_repo = setup_git_repo().await;
    let fixture = ProviderRouteFixture::new().await;
    write_invalid_harness_registry(fixture.data_root());
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
    assert!(payload["provider_options"]["qwen"]["config_error"]
        .as_str()
        .is_some_and(|value| value.contains("parsing harness source registry")));
    assert!(payload["provider_harness_config"].get("qwen").is_none());
}

#[tokio::test]
async fn provider_verify_surfaces_harness_config_errors() {
    let git_repo = setup_git_repo().await;
    let fixture = ProviderRouteFixture::new().await;
    write_invalid_harness_registry(fixture.data_root());
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
        .is_some_and(|value| value.contains("parsing harness source registry")));
}
