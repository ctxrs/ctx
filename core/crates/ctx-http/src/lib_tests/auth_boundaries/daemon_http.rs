use super::*;

#[tokio::test]
async fn daemon_http_routes_require_bearer_header_not_query_token() {
    let fixture = AuthBoundaryFixture::new().await;
    let git_repo = setup_git_repo().await;

    let app = fixture.app();

    let req = Request::builder()
        .method("GET")
        .uri("/api/workspaces?token=daemon-secret")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let req = Request::builder()
        .method("POST")
        .uri("/api/workspaces?token=daemon-secret")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "root_path": git_repo.path().to_string_lossy(),
                "name": "query-token-ws"
            })
            .to_string(),
        ))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let req = Request::builder()
        .method("GET")
        .uri("/api/workspaces")
        .header("authorization", "Bearer daemon-secret")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let req = Request::builder()
        .method("POST")
        .uri("/api/workspaces")
        .header("authorization", "Bearer daemon-secret")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "root_path": git_repo.path().to_string_lossy(),
                "name": "header-token-ws"
            })
            .to_string(),
        ))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn encoded_api_path_variants_do_not_bypass_auth() {
    let fixture = AuthBoundaryFixture::new().await;
    let app = fixture.app();

    for path in ["/api/workspaces", "/%61pi/workspaces", "/api%2Fworkspaces"] {
        let req = Request::builder()
            .method("GET")
            .uri(path)
            .body(Body::empty())
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert!(
            res.status() == StatusCode::UNAUTHORIZED || res.status() == StatusCode::NOT_FOUND,
            "unexpected status for {path}: {}",
            res.status()
        );
    }
}
