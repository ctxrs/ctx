use super::*;

async fn post_launch_start(app: &axum::Router, body: serde_json::Value) -> (StatusCode, String) {
    let req = Request::builder()
        .method("POST")
        .uri("/api/execution/launch/start")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    (
        status,
        value["error"].as_str().unwrap_or_default().to_string(),
    )
}

#[tokio::test]
async fn execution_launch_start_requires_workspace_id_for_workspace_launch() {
    let data_dir = tempfile::tempdir().unwrap();
    let fixture = test_daemon_fixture_with_fake_provider_for_test(data_dir.path(), None).await;
    let app = fixture.router();

    let (status, error) = post_launch_start(&app, json!({})).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error, "workspace_id is required for workspace_launch");
}

#[tokio::test]
async fn execution_launch_start_rejects_malformed_workspace_id() {
    let data_dir = tempfile::tempdir().unwrap();
    let fixture = test_daemon_fixture_with_fake_provider_for_test(data_dir.path(), None).await;
    let app = fixture.router();

    let (status, error) = post_launch_start(&app, json!({ "workspace_id": "not-a-uuid" })).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error, "invalid workspace id");
}

#[tokio::test]
async fn execution_launch_start_returns_not_found_for_missing_workspace() {
    let data_dir = tempfile::tempdir().unwrap();
    let fixture = test_daemon_fixture_with_fake_provider_for_test(data_dir.path(), None).await;
    let app = fixture.router();

    let (status, error) = post_launch_start(
        &app,
        json!({ "workspace_id": "00000000-0000-0000-0000-000000000001" }),
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error, "workspace not found");
}

#[tokio::test]
async fn execution_launch_start_rejects_while_maintenance_drain_active() {
    let data_dir = tempfile::tempdir().unwrap();
    let fixture = test_daemon_fixture_with_fake_provider_for_test(data_dir.path(), None).await;
    let app = fixture.router();
    let update_drain = fixture.daemon().update_drain_handle_for_test();
    update_drain
        .begin_update_drain("test_update".to_string(), "unit_test".to_string())
        .await
        .expect("idle test daemon should acquire maintenance drain");

    let (status, error) = post_launch_start(&app, json!({})).await;
    let _ = update_drain.release_update_drain().await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert!(error.contains("test_update"));
}
