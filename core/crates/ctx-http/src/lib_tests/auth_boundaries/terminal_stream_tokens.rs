use super::*;

#[tokio::test]
async fn terminal_websocket_stream_requires_terminal_scoped_query_token() {
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
                "name": "terminal-auth-boundary"
            })
            .to_string(),
        ))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let workspace: ctx_core::models::Workspace = serde_json::from_slice(&body).unwrap();

    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/workspaces/{}/terminals", workspace.id.0))
        .header("authorization", "Bearer daemon-secret")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"cwd": git_repo.path().to_string_lossy()}).to_string(),
        ))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let terminal: ctx_core::models::TerminalSession = serde_json::from_slice(&body).unwrap();

    let req = Request::builder()
        .method("POST")
        .uri(format!("/api/terminals/{}/stream_token", terminal.id.0))
        .header("authorization", "Bearer daemon-secret")
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let stream_path = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["stream_path"]
        .as_str()
        .unwrap()
        .to_string();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = reqwest::Client::new();

    assert_eq!(
        websocket_upgrade_status(
            &client,
            addr,
            &format!(
                "/api/terminals/{}/stream?token=daemon-secret",
                terminal.id.0
            ),
        )
        .await,
        StatusCode::UNAUTHORIZED
    );

    assert_eq!(
        websocket_upgrade_status(
            &client,
            addr,
            "/api/terminals/not-a-terminal/stream?token=x"
        )
        .await,
        StatusCode::BAD_REQUEST
    );

    assert_eq!(
        websocket_upgrade_status(
            &client,
            addr,
            &format!("/api/terminals/{}/stream", terminal.id.0)
        )
        .await,
        StatusCode::UNAUTHORIZED
    );

    assert_eq!(
        websocket_upgrade_status(&client, addr, &stream_path).await,
        StatusCode::SWITCHING_PROTOCOLS
    );

    assert_eq!(
        websocket_upgrade_status(&client, addr, &stream_path).await,
        StatusCode::UNAUTHORIZED
    );

    server.abort();
}
