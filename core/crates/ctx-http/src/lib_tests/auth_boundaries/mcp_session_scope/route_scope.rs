use super::*;

#[tokio::test]
async fn scoped_mcp_token_is_limited_to_bound_session_routes() {
    let fixture = AuthBoundaryFixture::new().await;

    let state = fixture.daemon();
    let session_id = SessionId::new();
    let other_session_id = SessionId::new();
    let token = state
        .issue_provider_session_mcp_token(session_id, WorkspaceId::new(), WorktreeId::new())
        .await;
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
        .method("GET")
        .uri(format!("/api/mcp/sessions/{}/list_agents", session_id.0))
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_ne!(res.status(), StatusCode::UNAUTHORIZED);

    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/mcp/sessions/{}/list_agents",
            other_session_id.0
        ))
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/sessions/{}/artifacts", session_id.0))
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(json!({"artifacts":[]}).to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_ne!(res.status(), StatusCode::UNAUTHORIZED);

    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/sessions/{}/artifacts", session_id.0))
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/sessions/{}/artifacts", other_session_id.0))
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    for (method, uri) in [
        ("POST", format!("/api/sessions/{}/interrupt", session_id.0)),
        (
            "POST",
            format!("/api/sessions/{}/authenticate", session_id.0),
        ),
        ("GET", "/api/sessions/web".to_string()),
        ("POST", "/api/sessions/web".to_string()),
        (
            "GET",
            format!(
                "/api/merge-queue/entries?workspace_id={}",
                WorkspaceId::new().0
            ),
        ),
        ("GET", "/api/blobs/blob_test".to_string()),
    ] {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/sessions/{}/cancel", session_id.0))
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}
