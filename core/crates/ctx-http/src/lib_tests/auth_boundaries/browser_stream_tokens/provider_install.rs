use super::*;

#[tokio::test]
async fn provider_install_stream_requires_browser_scoped_query_token() {
    let fixture = AuthBoundaryFixture::new().await;
    let app = fixture.app();
    let install_id = "11111111-1111-1111-1111-111111111111";

    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/providers/install/{install_id}/stream?token=daemon-secret"
        ))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/providers/install/{install_id}/stream"))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let expires_at = chrono::Utc::now().timestamp() + STREAM_TOKEN_TTL_SECS;
    let scoped_token = derive_browser_stream_token(
        "daemon-secret",
        &BrowserStreamAuthScope::ProviderInstall {
            install_id: install_id.to_string(),
        },
        expires_at,
    );
    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/providers/install/{install_id}/stream?expires_at={expires_at}&token={scoped_token}"
        ))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}
