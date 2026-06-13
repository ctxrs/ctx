use super::*;

#[tokio::test]
async fn desktop_browser_query_secret_authorizes_high_risk_owner_routes() {
    let fixture = AuthBoundaryFixture::new().await;
    let app = fixture.app();
    let browser_secret = derive_browser_query_secret("daemon-secret");
    let workspace_id = "11111111-1111-1111-1111-111111111111";

    let owner_cases = [
        (
            "POST",
            "/api/workspaces/11111111-1111-1111-1111-111111111111/merge_queue_config".to_string(),
            json!({"enabled": true}),
        ),
        (
            "POST",
            "/api/merge-queue/entries".to_string(),
            json!({"workspace_id": workspace_id}),
        ),
        (
            "POST",
            "/api/providers/codex/harness_config/endpoints".to_string(),
            json!({"base_url": "https://example.invalid"}),
        ),
        ("POST", "/api/daemon/shutdown".to_string(), json!({})),
        ("POST", "/api/updates/drain/begin".to_string(), json!({})),
        ("POST", "/api/updates/appimage/apply".to_string(), json!({})),
    ];

    for (method, uri, body) in owner_cases {
        let req = Request::builder()
            .method(method)
            .uri(uri.as_str())
            .header("authorization", format!("Bearer {browser_secret}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_ne!(res.status(), StatusCode::UNAUTHORIZED, "{method} {uri}");
    }
}

#[tokio::test]
async fn desktop_browser_query_secret_authorizes_transition_provider_install_routes() {
    let fixture = AuthBoundaryFixture::new().await;
    let app = fixture.app();
    let browser_secret = derive_browser_query_secret("daemon-secret");

    let cases = [
        ("POST", "/api/providers/not-real/install"),
        ("POST", "/api/providers/install_all"),
        ("POST", "/api/providers/install/statuses"),
        ("POST", "/api/providers/install/not-real/cancel"),
    ];

    for (method, uri) in cases {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {browser_secret}"))
            .header("content-type", "application/json")
            .body(Body::from("{"))
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_ne!(res.status(), StatusCode::UNAUTHORIZED, "{method} {uri}");
    }
}
