use super::*;

#[tokio::test]
async fn mobile_api_tokens_do_not_authorize_desktop_api_routes() {
    let fixture = AuthBoundaryFixture::new().await;
    let git_repo = setup_git_repo().await;

    let state = fixture.daemon();

    let token = "ctxm_test_mobile_token";
    state
        .mobile_access_for_test()
        .seed_mobile_api_profile_for_test(token, &["device_registration"])
        .await
        .unwrap();

    let app = fixture.app();
    let req = Request::builder()
        .method("GET")
        .uri("/api/workspaces")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let req = Request::builder()
        .method("POST")
        .uri("/api/workspaces")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "root_path": git_repo.path().to_string_lossy(),
                "name": "mobile-token-ws"
            })
            .to_string(),
        ))
        .unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}
