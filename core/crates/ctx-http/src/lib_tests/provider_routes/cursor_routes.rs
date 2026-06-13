use super::*;

#[tokio::test]
async fn cursor_login_status_preserves_missing_login_error() {
    let fixture = ProviderRouteFixture::new().await;
    let req = Request::builder()
        .method("GET")
        .uri("/api/providers/cursor/accounts/login/missing-login")
        .body(Body::empty())
        .unwrap();

    let res = fixture.app().oneshot(req).await.unwrap();

    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(payload["error"].as_str(), Some("login not found"));
}

#[tokio::test]
async fn cursor_login_start_preserves_runtime_command_bad_request() {
    let fixture = ProviderRouteFixture::new().await;
    let req = Request::builder()
        .method("POST")
        .uri("/api/providers/cursor/accounts/login/start")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({}).to_string()))
        .unwrap();

    let res = fixture.app().oneshot(req).await.unwrap();

    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let message = payload["error"].as_str().unwrap_or_default();
    assert!(message.contains("runtime_command_missing: provider=cursor-login"));
}
