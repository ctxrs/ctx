use super::*;

#[tokio::test]
async fn missing_provider_account_deletes_return_not_found() {
    let fixture = ProviderRouteFixture::with_auth_token("daemon-secret").await;
    let app = fixture.app();

    for route in [
        "/api/providers/codex/accounts/missing",
        "/api/providers/claude-crp/accounts/missing",
        "/api/providers/gemini/accounts/missing",
        "/api/providers/qwen/accounts/missing",
        "/api/providers/kimi/accounts/missing",
        "/api/providers/amp/accounts/missing",
        "/api/providers/mistral/accounts/missing",
        "/api/providers/copilot/accounts/missing",
        "/api/providers/cursor/accounts/missing",
    ] {
        let req = Request::builder()
            .method("DELETE")
            .uri(route)
            .header(header::AUTHORIZATION, "Bearer daemon-secret")
            .body(Body::empty())
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "{route}");
    }
}

#[tokio::test]
async fn missing_provider_harness_endpoint_delete_returns_not_found() {
    let fixture = ProviderRouteFixture::with_auth_token("daemon-secret").await;
    let app = fixture.app();

    let req = Request::builder()
        .method("DELETE")
        .uri("/api/providers/qwen/harness_config/endpoints/missing")
        .header(header::AUTHORIZATION, "Bearer daemon-secret")
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(payload["error"]
        .as_str()
        .is_some_and(|error| error.contains("unknown endpoint")));
}
