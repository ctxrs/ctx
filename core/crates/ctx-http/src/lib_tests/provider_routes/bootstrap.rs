use super::*;

#[tokio::test]
async fn provider_bootstrap_preserves_invalid_workspace_error() {
    let fixture = ProviderRouteFixture::new().await;
    let app = fixture.app();

    let req = Request::builder()
        .method("GET")
        .uri("/api/workspaces/not-a-uuid/providers/bootstrap")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"].as_str(), Some("invalid workspace id"));
}

#[tokio::test]
async fn provider_bootstrap_preserves_missing_workspace_error() {
    let fixture = ProviderRouteFixture::new().await;
    let app = fixture.app();

    let req = Request::builder()
        .method("GET")
        .uri("/api/workspaces/11111111-1111-4111-8111-111111111111/providers/bootstrap")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"].as_str(), Some("workspace not found"));
}

#[tokio::test]
async fn provider_bootstrap_preserves_execution_settings_context() {
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
            "/api/workspaces/{}/providers/bootstrap",
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
async fn provider_bootstrap_includes_pending_codex_login_sessions() {
    let git_repo = setup_git_repo().await;
    let fixture = ProviderRouteFixture::with_codex_home().await;
    let app = fixture.app();
    let workspace = create_workspace_via_api(&app, &git_repo.path().to_string_lossy()).await;
    fixture
        .daemon()
        .seed_pending_codex_login_for_test("codex-login-1", None)
        .await;

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
    let logins = payload["codex_accounts"]["logins"]
        .as_array()
        .expect("codex login sessions");
    assert!(logins
        .iter()
        .any(|login| login["account_id"].as_str() == Some("codex-login-1")));
}

#[tokio::test]
async fn provider_bootstrap_imports_amp_runtime_auth_registry() {
    let git_repo = setup_git_repo().await;
    let fixture = ProviderRouteFixture::new().await;
    let app = fixture.app();
    let workspace = create_workspace_via_api(&app, &git_repo.path().to_string_lossy()).await;
    let amp_secrets = fixture
        .data_root()
        .join("providers")
        .join("amp")
        .join("home")
        .join(".local")
        .join("share")
        .join("amp")
        .join("secrets.json");
    tokio::fs::create_dir_all(amp_secrets.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(
        &amp_secrets,
        br#"{"apiKey@https://ampcode.com/":"token-value"}"#,
    )
    .await
    .unwrap();

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
    let accounts = payload["amp_accounts"]["accounts"]
        .as_array()
        .expect("amp accounts");
    assert!(accounts
        .iter()
        .any(|account| account["label"].as_str() == Some("Amp Imported Session")));
}
