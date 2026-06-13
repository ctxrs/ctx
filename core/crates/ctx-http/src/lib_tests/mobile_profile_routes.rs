use super::*;

#[tokio::test]
async fn create_mobile_connection_profile_normalizes_explicit_scopes() {
    let _serial = home_env_test_lock().lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());

    let data_dir = tempfile::tempdir().unwrap();
    let fixture =
        test_daemon_fixture_for_test(data_dir.path(), Some("daemon-secret".to_string())).await;

    let app = fixture.router();
    let req = Request::builder()
        .method("POST")
        .uri("/api/mobile/connection_profiles")
        .header(header::AUTHORIZATION, "Bearer daemon-secret")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "label": "mobile",
                "base_url": "https://example.com/",
                "scopes": [" workspace_stream ", "device_registration", "workspace_stream"]
            })
            .to_string(),
        ))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        payload["profile"]["scopes"],
        json!(["device_registration", "workspace_stream"])
    );
}

#[tokio::test]
async fn create_mobile_connection_profile_rejects_unknown_scope_names() {
    let _serial = home_env_test_lock().lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());

    let data_dir = tempfile::tempdir().unwrap();
    let fixture =
        test_daemon_fixture_for_test(data_dir.path(), Some("daemon-secret".to_string())).await;

    let app = fixture.router();
    let req = Request::builder()
        .method("POST")
        .uri("/api/mobile/connection_profiles")
        .header(header::AUTHORIZATION, "Bearer daemon-secret")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "label": "mobile",
                "base_url": "https://example.com",
                "scopes": ["workspace_read", "unknown"]
            })
            .to_string(),
        ))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"], "unknown mobile scope: unknown");
}

#[tokio::test]
async fn create_mobile_connection_profile_requires_explicit_scopes_field() {
    let _serial = home_env_test_lock().lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());

    let data_dir = tempfile::tempdir().unwrap();
    let fixture =
        test_daemon_fixture_for_test(data_dir.path(), Some("daemon-secret".to_string())).await;

    let app = fixture.router();
    let req = Request::builder()
        .method("POST")
        .uri("/api/mobile/connection_profiles")
        .header(header::AUTHORIZATION, "Bearer daemon-secret")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "label": "mobile",
                "base_url": "https://example.com"
            })
            .to_string(),
        ))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn delete_mobile_connection_profile_returns_not_found_after_first_removal() {
    let _serial = home_env_test_lock().lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());

    let data_dir = tempfile::tempdir().unwrap();
    let fixture =
        test_daemon_fixture_for_test(data_dir.path(), Some("daemon-secret".to_string())).await;
    let state = fixture.daemon();

    let token = "ctxm_test_mobile_token";
    let profile = state
        .mobile_access_for_test()
        .seed_mobile_api_profile_for_test(token, &[])
        .await
        .unwrap();

    let app = fixture.router();
    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/api/mobile/connection_profiles/{}", profile.id.0))
        .header(header::AUTHORIZATION, "Bearer daemon-secret")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NO_CONTENT);

    let req = Request::builder()
        .method("DELETE")
        .uri(format!("/api/mobile/connection_profiles/{}", profile.id.0))
        .header(header::AUTHORIZATION, "Bearer daemon-secret")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_mobile_connection_profile_rejects_invalid_profile_id() {
    let _serial = home_env_test_lock().lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());

    let data_dir = tempfile::tempdir().unwrap();
    let fixture =
        test_daemon_fixture_for_test(data_dir.path(), Some("daemon-secret".to_string())).await;

    let app = fixture.router();
    let req = Request::builder()
        .method("DELETE")
        .uri("/api/mobile/connection_profiles/not-a-profile-id")
        .header(header::AUTHORIZATION, "Bearer daemon-secret")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn list_mobile_devices_for_profile_rejects_invalid_profile_id() {
    let _serial = home_env_test_lock().lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());

    let data_dir = tempfile::tempdir().unwrap();
    let fixture =
        test_daemon_fixture_for_test(data_dir.path(), Some("daemon-secret".to_string())).await;

    let app = fixture.router();
    let req = Request::builder()
        .method("GET")
        .uri("/api/mobile/connection_profiles/not-a-profile-id/devices")
        .header(header::AUTHORIZATION, "Bearer daemon-secret")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}
