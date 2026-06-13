use super::*;

async fn post_merge_queue_entry(
    app: &axum::Router,
    bearer: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method("POST")
        .uri("/api/merge-queue/entries")
        .header("authorization", format!("Bearer {bearer}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let body = if body.is_empty() {
        json!(null)
    } else {
        serde_json::from_slice(&body).unwrap()
    };
    (status, body)
}

#[tokio::test]
async fn scoped_mcp_merge_queue_submit_is_bound_to_current_session_worktree() {
    let fixture = AuthBoundaryFixture::new().await;

    let state = fixture.daemon();
    let session_id = SessionId::new();
    let other_session_id = SessionId::new();
    let worktree_id = WorktreeId::new();
    let other_worktree_id = WorktreeId::new();
    let token = state
        .issue_provider_session_mcp_token(session_id, WorkspaceId::new(), worktree_id)
        .await;
    let app = fixture.app();

    let (status, _) = post_merge_queue_entry(
        &app,
        &token,
        json!({
            "session_id": session_id.0.to_string(),
            "worktree_id": worktree_id.0.to_string()
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "provider-session MCP tokens must not include merge queue submit by default"
    );

    let token = state
        .issue_provider_session_mcp_token_with_capabilities(
            session_id,
            WorkspaceId::new(),
            worktree_id,
            ctx_mcp_auth::McpAuthCapabilities::provider_turn_default(),
        )
        .await;

    let (status, body) = post_merge_queue_entry(
        &app,
        &token,
        json!({
            "session_id": session_id.0.to_string(),
            "worktree_id": worktree_id.0.to_string()
        }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, json!({"error": "session not found"}));

    let (status, body) = post_merge_queue_entry(
        &app,
        &token,
        json!({
            "session_id": other_session_id.0.to_string(),
            "worktree_id": worktree_id.0.to_string()
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        body,
        json!({"error": "scoped ctx-mcp merge queue submit is limited to the current session and worktree"})
    );

    let (status, body) = post_merge_queue_entry(
        &app,
        &token,
        json!({
            "session_id": session_id.0.to_string(),
            "worktree_id": other_worktree_id.0.to_string()
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        body,
        json!({"error": "scoped ctx-mcp merge queue submit is limited to the current session and worktree"})
    );

    let (status, body) = post_merge_queue_entry(
        &app,
        &token,
        json!({"worktree_root": "/tmp/other-worktree"}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        body,
        json!({"error": "scoped ctx-mcp merge queue submit cannot override worktree_root"})
    );

    let (status, body) = post_merge_queue_entry(&app, &token, json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, json!({"error": "session not found"}));

    let (status, body) =
        post_merge_queue_entry(&app, &token, json!({"worktree_root": "   "})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body, json!({"error": "session not found"}));
}

#[tokio::test]
async fn merge_queue_submit_rejects_invalid_route_ids() {
    let fixture = AuthBoundaryFixture::new().await;
    let app = fixture.app();

    let (status, body) = post_merge_queue_entry(
        &app,
        "daemon-secret",
        json!({
            "session_id": "not-a-uuid",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, json!({"error": "invalid session_id"}));

    let (status, body) = post_merge_queue_entry(
        &app,
        "daemon-secret",
        json!({
            "worktree_id": "not-a-uuid",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body, json!({"error": "invalid worktree_id"}));
}
