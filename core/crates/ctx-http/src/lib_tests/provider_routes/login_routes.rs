use super::*;

#[tokio::test]
async fn simple_provider_login_statuses_preserve_missing_login_error() {
    let fixture = ProviderRouteFixture::new().await;
    let app = fixture.app();
    let paths = [
        "/api/providers/amp/accounts/login/missing-login",
        "/api/providers/gemini/accounts/login/missing-login",
        "/api/providers/kimi/accounts/login/missing-login",
        "/api/providers/mistral/accounts/login/missing-login",
        "/api/providers/qwen/accounts/login/missing-login",
    ];

    for path in paths {
        let req = Request::builder()
            .method("GET")
            .uri(path)
            .body(Body::empty())
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "{path}");
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(payload["error"].as_str(), Some("login not found"), "{path}");
    }
}
