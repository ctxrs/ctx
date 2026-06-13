use super::*;

#[tokio::test]
async fn workspace_active_websocket_stream_requires_browser_scoped_query_token() {
    let fixture = AuthBoundaryFixture::new().await;
    let git_repo = setup_git_repo().await;

    let app = fixture.app();

    let req = Request::builder()
        .method("POST")
        .uri("/api/workspaces")
        .header("authorization", "Bearer daemon-secret")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "root_path": git_repo.path().to_string_lossy(),
                "name": "workspace-stream-auth-boundary"
            })
            .to_string(),
        ))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let workspace: ctx_core::models::Workspace = serde_json::from_slice(&body).unwrap();

    let expires_at = chrono::Utc::now().timestamp() + STREAM_TOKEN_TTL_SECS;
    let scoped_token = derive_browser_stream_token(
        "daemon-secret",
        &BrowserStreamAuthScope::WorkspaceActiveSnapshot {
            workspace_id: workspace.id.0.to_string(),
        },
        expires_at,
    );
    let browser_query_secret = derive_browser_query_secret("daemon-secret");
    let scoped_browser_secret_token = derive_browser_stream_token(
        &browser_query_secret,
        &BrowserStreamAuthScope::WorkspaceActiveSnapshot {
            workspace_id: workspace.id.0.to_string(),
        },
        expires_at,
    );
    let (addr, server) = serve_test_app(app).await;
    let client = reqwest::Client::new();

    assert_eq!(
        websocket_upgrade_status(
            &client,
            addr,
            &format!(
                "/api/workspaces/{}/active_snapshot/stream?token=daemon-secret",
                workspace.id.0
            ),
        )
        .await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        websocket_upgrade_status(
            &client,
            addr,
            &format!("/api/workspaces/{}/active_snapshot/stream", workspace.id.0),
        )
        .await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        websocket_upgrade_status(
            &client,
            addr,
            &format!(
                "/api/workspaces/{}/active_snapshot/stream?expires_at={expires_at}&token={scoped_token}",
                workspace.id.0
            ),
        )
        .await,
        StatusCode::SWITCHING_PROTOCOLS
    );
    assert_eq!(
        websocket_upgrade_status(
            &client,
            addr,
            &format!(
                "/api/workspaces/{}/active_snapshot/stream?expires_at={expires_at}&token={scoped_browser_secret_token}",
                workspace.id.0
            ),
        )
        .await,
        StatusCode::SWITCHING_PROTOCOLS
    );
    let ahead_client_expires_at = chrono::Utc::now().timestamp() + STREAM_TOKEN_TTL_SECS + 5 * 60;
    let ahead_client_token = derive_browser_stream_token(
        "daemon-secret",
        &BrowserStreamAuthScope::WorkspaceActiveSnapshot {
            workspace_id: workspace.id.0.to_string(),
        },
        ahead_client_expires_at,
    );
    assert_eq!(
        websocket_upgrade_status(
            &client,
            addr,
            &format!(
                "/api/workspaces/{}/active_snapshot/stream?expires_at={ahead_client_expires_at}&token={ahead_client_token}",
                workspace.id.0
            ),
        )
        .await,
        StatusCode::SWITCHING_PROTOCOLS
    );
    let behind_client_expires_at = chrono::Utc::now().timestamp() - 2 * 60;
    let behind_client_token = derive_browser_stream_token(
        "daemon-secret",
        &BrowserStreamAuthScope::WorkspaceActiveSnapshot {
            workspace_id: workspace.id.0.to_string(),
        },
        behind_client_expires_at,
    );
    assert_eq!(
        websocket_upgrade_status(
            &client,
            addr,
            &format!(
                "/api/workspaces/{}/active_snapshot/stream?expires_at={behind_client_expires_at}&token={behind_client_token}",
                workspace.id.0
            ),
        )
        .await,
        StatusCode::SWITCHING_PROTOCOLS
    );
    let too_far_ahead_expires_at = chrono::Utc::now().timestamp()
        + STREAM_TOKEN_TTL_SECS
        + STREAM_TOKEN_MAX_FUTURE_SKEW_SECS
        + 60;
    let too_far_ahead_token = derive_browser_stream_token(
        "daemon-secret",
        &BrowserStreamAuthScope::WorkspaceActiveSnapshot {
            workspace_id: workspace.id.0.to_string(),
        },
        too_far_ahead_expires_at,
    );
    assert_eq!(
        websocket_upgrade_status(
            &client,
            addr,
            &format!(
                "/api/workspaces/{}/active_snapshot/stream?expires_at={too_far_ahead_expires_at}&token={too_far_ahead_token}",
                workspace.id.0
            ),
        )
        .await,
        StatusCode::UNAUTHORIZED
    );
    let too_far_past_expires_at =
        chrono::Utc::now().timestamp() - STREAM_TOKEN_MAX_PAST_SKEW_SECS - 1;
    let too_far_past_token = derive_browser_stream_token(
        "daemon-secret",
        &BrowserStreamAuthScope::WorkspaceActiveSnapshot {
            workspace_id: workspace.id.0.to_string(),
        },
        too_far_past_expires_at,
    );
    assert_eq!(
        websocket_upgrade_status(
            &client,
            addr,
            &format!(
                "/api/workspaces/{}/active_snapshot/stream?expires_at={too_far_past_expires_at}&token={too_far_past_token}",
                workspace.id.0
            ),
        )
        .await,
        StatusCode::UNAUTHORIZED
    );

    server.abort();
}
