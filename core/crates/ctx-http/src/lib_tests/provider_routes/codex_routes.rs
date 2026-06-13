use super::*;

#[tokio::test]
async fn codex_accounts_usage_surfaces_agent_server_config_errors() {
    let fixture = ProviderRouteFixture::new().await;
    write_invalid_agent_server_config(fixture.data_root());
    let app = fixture.app();

    let req = Request::builder()
        .method("GET")
        .uri("/api/providers/codex/accounts/usage")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(payload["error"]
        .as_str()
        .is_some_and(|value| value.contains("parsing agent server config")));
}

#[tokio::test]
async fn codex_accounts_usage_blocks_deleting_account_broker_home() {
    let fixture = ProviderRouteFixture::new().await;
    let codex_bin_dir = tempfile::tempdir().unwrap();
    let codex_bin = codex_bin_dir.path().join("codex");
    std::fs::write(&codex_bin, "#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(&codex_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    clear_agent_server_config(fixture.data_root());
    let mut cfg = ctx_managed_installs::AgentServerConfigFile::default();
    cfg.providers.insert(
        "codex-cli".to_string(),
        ctx_managed_installs::AgentServerCommand {
            command: codex_bin.to_string_lossy().to_string(),
            args: Vec::new(),
            dependencies: Vec::new(),
            managed: None,
        },
    );
    ctx_managed_installs::save_agent_server_config(fixture.data_root(), &cfg)
        .await
        .unwrap();

    let account_id = "acct-deleting";
    ctx_provider_accounts::save_codex_registry(
        fixture.data_root(),
        &ctx_provider_accounts::CodexAccountRegistry {
            active_account_id: Some(account_id.to_string()),
            accounts: vec![ctx_provider_accounts::CodexAccountEntry {
                id: account_id.to_string(),
                label: "Deleting Account".to_string(),
                kind: ctx_provider_accounts::CODEX_CREDENTIAL_KIND_OAUTH.to_string(),
                email: None,
                provider_account_id: None,
                plan_type: None,
                created_at: chrono::Utc::now(),
                last_used_at: None,
                secret_ref: None,
                endpoint_profile: ctx_provider_accounts::CodexEndpointProfile::default(),
            }],
        },
    )
    .await
    .unwrap();
    let broker_home = ctx_provider_accounts::codex_broker_home(fixture.data_root(), account_id);
    std::fs::create_dir_all(&broker_home).unwrap();
    std::fs::write(
        broker_home.join("auth.json"),
        r#"{"tokens":{"access_token":"stale-access","refresh_token":"refresh-token"}}"#,
    )
    .unwrap();
    std::fs::write(
        broker_home.join("config.toml"),
        "chatgpt_base_url = \"http://127.0.0.1:9\"",
    )
    .unwrap();
    ctx_provider_accounts::begin_codex_account_deletion(fixture.data_root(), account_id)
        .await
        .unwrap();

    let app = fixture.app();

    let req = Request::builder()
        .method("GET")
        .uri("/api/providers/codex/accounts/usage?refresh=true")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let entries = payload["entries"].as_array().expect("usage entries");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["account_id"].as_str(), Some(account_id));
    let usage = &entries[0]["usage"];
    assert_eq!(usage["source"].as_str(), Some("error"));
    assert!(usage.get("payload").is_none());
    assert!(usage["error"]
        .as_str()
        .is_some_and(|value| value.contains("being deleted")));
}

#[tokio::test]
async fn codex_accounts_usage_surfaces_hydration_errors() {
    let fixture = ProviderRouteFixture::new().await;
    let codex_bin_dir = tempfile::tempdir().unwrap();
    let codex_bin = codex_bin_dir.path().join("codex");
    std::fs::write(&codex_bin, "#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(&codex_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    clear_agent_server_config(fixture.data_root());
    let mut cfg = ctx_managed_installs::AgentServerConfigFile::default();
    cfg.providers.insert(
        "codex-cli".to_string(),
        ctx_managed_installs::AgentServerCommand {
            command: codex_bin.to_string_lossy().to_string(),
            args: Vec::new(),
            dependencies: Vec::new(),
            managed: None,
        },
    );
    ctx_managed_installs::save_agent_server_config(fixture.data_root(), &cfg)
        .await
        .unwrap();

    let account_id = "acct-bad-secret";
    let secret_ref = format!("{account_id}.json");
    ctx_provider_accounts::save_codex_registry(
        fixture.data_root(),
        &ctx_provider_accounts::CodexAccountRegistry {
            active_account_id: None,
            accounts: vec![ctx_provider_accounts::CodexAccountEntry {
                id: account_id.to_string(),
                label: "Bad Secret".to_string(),
                kind: ctx_provider_accounts::CODEX_CREDENTIAL_KIND_OAUTH.to_string(),
                email: None,
                provider_account_id: None,
                plan_type: None,
                created_at: chrono::Utc::now(),
                last_used_at: None,
                secret_ref: Some(secret_ref.clone()),
                endpoint_profile: ctx_provider_accounts::CodexEndpointProfile::default(),
            }],
        },
    )
    .await
    .unwrap();
    let secret_path =
        ctx_provider_accounts::codex_secrets_root(fixture.data_root()).join(secret_ref);
    std::fs::create_dir_all(secret_path.parent().unwrap()).unwrap();
    std::fs::write(&secret_path, "{ not valid json").unwrap();

    let app = fixture.app();

    let req = Request::builder()
        .method("GET")
        .uri("/api/providers/codex/accounts/usage?refresh=true")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let entries = payload["entries"].as_array().expect("usage entries");
    assert_eq!(entries.len(), 1);
    let usage = &entries[0]["usage"];
    assert_eq!(usage["source"].as_str(), Some("error"));
    let error = usage["error"].as_str().expect("usage error");
    assert!(error.contains("preparing codex account auth failed"));
    assert!(error.contains("invalid codex secret JSON"));
}

#[tokio::test]
async fn provider_usage_cache_hit_surfaces_agent_server_config_errors_for_codex() {
    let fixture = ProviderRouteFixture::with_codex_home().await;
    write_invalid_agent_server_config(fixture.data_root());
    let state = fixture.daemon();
    state
        .seed_provider_usage_success_for_test(
            "codex",
            "oauth",
            serde_json::json!({
                "cached": true
            }),
        )
        .await;
    let app = fixture.app();

    let req = Request::builder()
        .method("GET")
        .uri("/api/providers/codex/usage")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(payload["error"]
        .as_str()
        .is_some_and(|value| value.contains("parsing agent server config")));
}

#[tokio::test]
async fn provider_usage_cache_hit_preserves_canonical_provider_id_for_codex() {
    let fixture = ProviderRouteFixture::with_codex_home().await;
    let codex_bin_dir = tempfile::tempdir().unwrap();
    let codex_bin = codex_bin_dir.path().join("codex");
    std::fs::write(&codex_bin, "#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(&codex_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    clear_agent_server_config(fixture.data_root());
    let mut cfg = ctx_managed_installs::AgentServerConfigFile::default();
    cfg.providers.insert(
        "codex-cli".to_string(),
        ctx_managed_installs::AgentServerCommand {
            command: codex_bin.to_string_lossy().to_string(),
            args: Vec::new(),
            dependencies: Vec::new(),
            managed: None,
        },
    );
    ctx_managed_installs::save_agent_server_config(fixture.data_root(), &cfg)
        .await
        .unwrap();
    let state = fixture.daemon();
    state
        .seed_provider_usage_success_for_test(
            "codex",
            "oauth",
            serde_json::json!({
                "cached": true
            }),
        )
        .await;
    let app = fixture.app();

    let req = Request::builder()
        .method("GET")
        .uri("/api/providers/codex/usage")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["provider_id"].as_str(), Some("codex"));
    assert_eq!(payload["payload"]["cached"].as_bool(), Some(true));
}

#[tokio::test]
async fn codex_login_status_preserves_missing_login_error() {
    let fixture = ProviderRouteFixture::new().await;
    let app = fixture.app();

    let req = Request::builder()
        .method("GET")
        .uri("/api/providers/codex/accounts/login/missing-login")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"].as_str(), Some("login not found"));
}

#[tokio::test]
async fn codex_login_complete_preserves_missing_login_error() {
    let fixture = ProviderRouteFixture::new().await;
    let app = fixture.app();

    let req = Request::builder()
        .method("POST")
        .uri("/api/providers/codex/accounts/login/missing-login")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            r#"{"callback_url":"http://localhost:43210/auth/callback?code=abc","completion_token":"token"}"#,
        ))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"].as_str(), Some("login not found"));
}

#[tokio::test]
async fn codex_login_start_surfaces_agent_server_config_errors() {
    let fixture = ProviderRouteFixture::new().await;
    write_invalid_agent_server_config(fixture.data_root());
    let app = fixture.app();

    let req = Request::builder()
        .method("POST")
        .uri("/api/providers/codex/accounts/login/start")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"label":"test"}"#))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(payload["error"]
        .as_str()
        .is_some_and(|value| value.contains("parsing agent server config")));
}
