use super::*;

#[tokio::test]
async fn unauthenticated_health_omits_sensitive_fields_when_daemon_auth_is_enabled() {
    let _serial = home_env_test_lock().lock().await;
    let _build_identity = EnvVarGuard::unset(ctx_update_service::BUILD_IDENTITY_PATH_ENV);

    let data_dir = tempfile::tempdir().unwrap();
    let fixture =
        test_daemon_fixture_for_test(data_dir.path(), Some("daemon-secret".to_string())).await;
    let app = fixture.router();

    let req = Request::builder()
        .method(Method::GET)
        .uri("/api/health")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let health: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(
        health.get("auth_required").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        health.get("version").and_then(|v| v.as_str()),
        Some(env!("CARGO_PKG_VERSION")),
        "health must report the ctx-http package version, not the ctx-daemon package version"
    );
    assert!(health.get("compatibility").is_some());
    assert_eq!(
        health
            .pointer("/compatibility/desktop_dev_instance_id")
            .and_then(|v| v.as_str()),
        Some(""),
        "health leaked desktop_dev_instance_id: {health:#?}"
    );
    assert_eq!(
        health
            .pointer("/compatibility/protocol_compatibility_token")
            .and_then(|v| v.as_str()),
        Some(""),
        "health leaked protocol_compatibility_token: {health:#?}"
    );
    assert!(
        health.get("pid").is_none(),
        "health leaked pid: {health:#?}"
    );
    assert!(
        health.get("data_root").is_none(),
        "health leaked data_root: {health:#?}"
    );
    assert!(
        health.get("daemon_url").is_none(),
        "health leaked daemon_url: {health:#?}"
    );
    assert!(
        health.get("storage").is_none(),
        "health leaked storage state: {health:#?}"
    );
    assert!(
        health.get("open_file_limit").is_none(),
        "health leaked open_file_limit: {health:#?}"
    );
}

#[tokio::test]
async fn authorized_health_keeps_sensitive_fields_when_daemon_auth_is_enabled() {
    let _serial = home_env_test_lock().lock().await;
    let _build_identity = EnvVarGuard::unset(ctx_update_service::BUILD_IDENTITY_PATH_ENV);

    let data_dir = tempfile::tempdir().unwrap();
    let fixture =
        test_daemon_fixture_for_test(data_dir.path(), Some("daemon-secret".to_string())).await;
    let app = fixture.router();

    let req = Request::builder()
        .method(Method::GET)
        .uri("/api/health")
        .header(header::AUTHORIZATION, "Bearer daemon-secret")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let health: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert!(health.get("pid").is_some(), "authorized health missing pid");
    assert!(
        health.get("open_file_limit").is_some(),
        "authorized health missing open_file_limit"
    );
    assert!(
        health.get("data_root").and_then(|v| v.as_str()).is_some(),
        "authorized health missing data_root"
    );
    assert!(
        health.get("daemon_url").and_then(|v| v.as_str()).is_some(),
        "authorized health missing daemon_url"
    );
    assert!(
        health
            .pointer("/storage/level")
            .and_then(|v| v.as_str())
            .is_some(),
        "authorized health missing storage state"
    );
    assert_ne!(
        health
            .pointer("/compatibility/desktop_dev_instance_id")
            .and_then(|v| v.as_str()),
        Some(""),
        "authorized health missing desktop_dev_instance_id"
    );
    assert_ne!(
        health
            .pointer("/compatibility/protocol_compatibility_token")
            .and_then(|v| v.as_str()),
        Some(""),
        "authorized health missing protocol_compatibility_token"
    );
}
