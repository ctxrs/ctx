use super::*;

#[tokio::test]
async fn scoped_mcp_token_revokes_prior_token_for_same_session_scope() {
    let fixture = AuthBoundaryFixture::new().await;

    let state = fixture.daemon();
    let session_id = SessionId::new();
    let workspace_id = WorkspaceId::new();
    let worktree_id = WorktreeId::new();
    let stale_token = state
        .issue_provider_session_mcp_token(session_id, workspace_id, worktree_id)
        .await;
    let fresh_token = state
        .issue_provider_session_mcp_token(session_id, workspace_id, worktree_id)
        .await;
    let app = fixture.app();

    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/mcp/sessions/{}/list_agents", session_id.0))
        .header("authorization", format!("Bearer {stale_token}"))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/mcp/sessions/{}/list_agents", session_id.0))
        .header("authorization", format!("Bearer {fresh_token}"))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_ne!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn scoped_mcp_token_can_be_revoked_exactly() {
    let fixture = AuthBoundaryFixture::new().await;

    let state = fixture.daemon();
    let session_id = SessionId::new();
    let token = state
        .issue_provider_session_mcp_token(session_id, WorkspaceId::new(), WorktreeId::new())
        .await;
    let app = fixture.app();

    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/mcp/sessions/{}/list_agents", session_id.0))
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_ne!(res.status(), StatusCode::UNAUTHORIZED);

    assert!(state.revoke_provider_session_mcp_token(&token).await);

    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/mcp/sessions/{}/list_agents", session_id.0))
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}
