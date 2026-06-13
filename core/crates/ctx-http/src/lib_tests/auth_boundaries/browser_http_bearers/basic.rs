use super::*;

#[tokio::test]
async fn desktop_browser_query_secret_authorizes_ordinary_http_routes() {
    let fixture = AuthBoundaryFixture::new().await;
    let app = fixture.app();
    let browser_secret = derive_browser_query_secret("daemon-secret");

    let req = Request::builder()
        .method("GET")
        .uri("/api/workspaces")
        .header("authorization", format!("Bearer {browser_secret}"))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn desktop_browser_query_secret_rejects_wrong_secret() {
    let fixture = AuthBoundaryFixture::new().await;
    let app = fixture.app();
    let wrong_browser_secret = derive_browser_query_secret("wrong-secret");

    let req = Request::builder()
        .method("GET")
        .uri("/api/workspaces")
        .header("authorization", format!("Bearer {wrong_browser_secret}"))
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn desktop_browser_query_secret_authorizes_owner_wide_api_routes() {
    let fixture = AuthBoundaryFixture::new().await;
    let app = fixture.app();
    let browser_secret = derive_browser_query_secret("daemon-secret");

    let req = Request::builder()
        .method("GET")
        .uri("/api/mcp/sessions/11111111-1111-1111-1111-111111111111/list_agents")
        .header("authorization", format!("Bearer {browser_secret}"))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_ne!(res.status(), StatusCode::UNAUTHORIZED);

    let req = Request::builder()
        .method("GET")
        .uri("/api/mobile/access/status")
        .header("authorization", format!("Bearer {browser_secret}"))
        .body(Body::empty())
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_ne!(res.status(), StatusCode::UNAUTHORIZED);
}
