use super::*;

async fn post_json(
    app: &axum::Router,
    uri: &str,
    payload: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(header::AUTHORIZATION, "Bearer daemon-secret")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(payload.to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let body = if body.is_empty() {
        json!(null)
    } else {
        serde_json::from_slice(&body).unwrap()
    };
    (status, body)
}

async fn get_json(app: &axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header(header::AUTHORIZATION, "Bearer daemon-secret")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let body = if body.is_empty() {
        json!(null)
    } else {
        serde_json::from_slice(&body).unwrap()
    };
    (status, body)
}

#[tokio::test]
async fn update_check_rejects_path_traversal_channel() {
    let data_dir = tempfile::tempdir().unwrap();
    let fixture =
        test_daemon_fixture_for_test(data_dir.path(), Some("daemon-secret".to_string())).await;
    let app = fixture.router();

    let req = Request::builder()
        .method(Method::GET)
        .uri("/api/updates/check?channel=x/../../../secret")
        .header(header::AUTHORIZATION, "Bearer daemon-secret")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn update_activity_reports_idle_and_update_drain_state() {
    let data_dir = tempfile::tempdir().unwrap();
    let fixture =
        test_daemon_fixture_for_test(data_dir.path(), Some("daemon-secret".to_string())).await;
    let app = fixture.router();

    let (status, body) = post_json(
        &app,
        "/api/updates/drain/begin",
        json!({"confirm": true, "reason": "test_update", "owner": "unit_test"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["acquired"], json!(true));

    let (status, body) = get_json(&app, "/api/updates/activity").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["activity"]["idle"], json!(true));
    assert_eq!(
        body["activity"]["update_drain"]["reason"],
        json!("test_update")
    );
    assert_eq!(
        body["activity"]["update_drain"]["owner"],
        json!("unit_test")
    );
    assert!(body["activity"]["update_drain"]["acquired_at_ms"].is_number());
    assert!(body.get("managed_daemon_auto_update").is_none());
}

#[tokio::test]
async fn update_drain_begin_requires_confirm() {
    let data_dir = tempfile::tempdir().unwrap();
    let fixture =
        test_daemon_fixture_for_test(data_dir.path(), Some("daemon-secret".to_string())).await;
    let app = fixture.router();

    let (status, body) = post_json(&app, "/api/updates/drain/begin", json!({})).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, json!({"error": "confirm required"}));
}

#[tokio::test]
async fn update_drain_begin_conflicts_when_already_active() {
    let data_dir = tempfile::tempdir().unwrap();
    let fixture =
        test_daemon_fixture_for_test(data_dir.path(), Some("daemon-secret".to_string())).await;
    let app = fixture.router();

    let (status, body) = post_json(
        &app,
        "/api/updates/drain/begin",
        json!({"confirm": true, "reason": "test_update", "owner": "unit_test"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["acquired"], json!(true));

    let (status, body) =
        post_json(&app, "/api/updates/drain/begin", json!({"confirm": true})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body, json!({"error": "daemon update drain already active"}));

    let (status, body) =
        post_json(&app, "/api/updates/drain/release", json!({"confirm": true})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"released": true}));
}

#[tokio::test]
async fn linux_sandbox_prepare_conflicts_with_active_update_drain() {
    let data_dir = tempfile::tempdir().unwrap();
    let fixture =
        test_daemon_fixture_for_test(data_dir.path(), Some("daemon-secret".to_string())).await;
    let app = fixture.router();

    let (status, body) = post_json(
        &app,
        "/api/updates/drain/begin",
        json!({"confirm": true, "reason": "test_update", "owner": "unit_test"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["acquired"], json!(true));

    let (status, body) = post_json(
        &app,
        "/api/execution/linux_sandbox_runtime/prepare",
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["error"]
        .as_str()
        .unwrap_or_default()
        .contains("already"));
}

#[tokio::test]
async fn update_drain_release_requires_confirm() {
    let data_dir = tempfile::tempdir().unwrap();
    let fixture =
        test_daemon_fixture_for_test(data_dir.path(), Some("daemon-secret".to_string())).await;
    let app = fixture.router();

    let (status, body) = post_json(&app, "/api/updates/drain/release", json!({})).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, json!({"error": "confirm required"}));
}

#[tokio::test]
async fn daemon_shutdown_endpoint_terminalizes_running_turns_before_ack() {
    let data_dir = tempfile::tempdir().unwrap();
    let fixture = {
        let _serial = home_env_test_lock().lock().await;
        let _shutdown_token =
            EnvVarGuard::set("CTX_LOCAL_DAEMON_SHUTDOWN_TOKEN", "local-shutdown-secret");
        test_daemon_fixture_for_test(data_dir.path(), Some("daemon-secret".to_string())).await
    };
    let state = fixture.daemon();

    let turn_fixture = state
        .seed_shutdown_running_turn_for_test(&data_dir.path().join("ws"), "fake", "model")
        .await
        .unwrap();

    let app = fixture.router();
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/daemon/shutdown")
        .header(header::AUTHORIZATION, "Bearer daemon-secret")
        .header("x-ctx-local-daemon-shutdown-token", "local-shutdown-secret")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"confirm": true}).to_string()))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let status = state
        .session_turn_status_for_test(turn_fixture.session_id, turn_fixture.turn_id)
        .await
        .unwrap()
        .expect("turn exists");
    assert_eq!(status, ctx_core::models::SessionTurnStatus::Interrupted);
}

#[tokio::test]
async fn daemon_shutdown_endpoint_rejects_missing_confirm_with_json_error() {
    let data_dir = tempfile::tempdir().unwrap();
    let fixture = {
        let _serial = home_env_test_lock().lock().await;
        let _shutdown_token =
            EnvVarGuard::set("CTX_LOCAL_DAEMON_SHUTDOWN_TOKEN", "local-shutdown-secret");
        test_daemon_fixture_for_test(data_dir.path(), Some("daemon-secret".to_string())).await
    };
    let app = fixture.router();

    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/daemon/shutdown")
        .header(header::AUTHORIZATION, "Bearer daemon-secret")
        .header("x-ctx-local-daemon-shutdown-token", "local-shutdown-secret")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({}).to_string()))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body, json!({"error": "confirm required"}));
}

#[tokio::test]
async fn daemon_shutdown_endpoint_requires_local_shutdown_token() {
    let data_dir = tempfile::tempdir().unwrap();
    let fixture = {
        let _serial = home_env_test_lock().lock().await;
        let _shutdown_token =
            EnvVarGuard::set("CTX_LOCAL_DAEMON_SHUTDOWN_TOKEN", "local-shutdown-secret");
        test_daemon_fixture_for_test(data_dir.path(), Some("daemon-secret".to_string())).await
    };
    let app = fixture.router();

    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/daemon/shutdown")
        .header(header::AUTHORIZATION, "Bearer daemon-secret")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"confirm": true}).to_string()))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        body,
        json!({"error": "local desktop shutdown token required"})
    );
}

#[tokio::test]
async fn daemon_shutdown_endpoint_ignores_shutdown_token_in_body() {
    let data_dir = tempfile::tempdir().unwrap();
    let fixture = {
        let _serial = home_env_test_lock().lock().await;
        let _shutdown_token =
            EnvVarGuard::set("CTX_LOCAL_DAEMON_SHUTDOWN_TOKEN", "local-shutdown-secret");
        test_daemon_fixture_for_test(data_dir.path(), Some("daemon-secret".to_string())).await
    };
    let app = fixture.router();

    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/daemon/shutdown")
        .header(header::AUTHORIZATION, "Bearer daemon-secret")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            json!({
                "confirm": true,
                "supplied_shutdown_token": "local-shutdown-secret"
            })
            .to_string(),
        ))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        body,
        json!({"error": "local desktop shutdown token required"})
    );
}

#[tokio::test]
async fn daemon_shutdown_endpoint_rejects_invalid_local_shutdown_token() {
    let data_dir = tempfile::tempdir().unwrap();
    let fixture = {
        let _serial = home_env_test_lock().lock().await;
        let _shutdown_token =
            EnvVarGuard::set("CTX_LOCAL_DAEMON_SHUTDOWN_TOKEN", "local-shutdown-secret");
        test_daemon_fixture_for_test(data_dir.path(), Some("daemon-secret".to_string())).await
    };
    let app = fixture.router();

    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/daemon/shutdown")
        .header(header::AUTHORIZATION, "Bearer daemon-secret")
        .header("x-ctx-local-daemon-shutdown-token", "wrong")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"confirm": true}).to_string()))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        body,
        json!({"error": "local desktop shutdown token required"})
    );
}
