use super::*;

#[tokio::test]
async fn workspace_active_snapshot_stream_returns_not_found_before_upgrade() {
    let _serial = home_env_test_lock().lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());

    let data_dir = tempfile::tempdir().unwrap();
    let fixture =
        test_daemon_fixture_for_test(data_dir.path(), Some("daemon-secret".to_string())).await;
    let app = fixture.router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = reqwest::Client::new();

    let res = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        client
            .get(format!(
                "http://{addr}/api/workspaces/11111111-1111-1111-1111-111111111111/active_snapshot/stream"
            ))
            .header("authorization", "Bearer daemon-secret")
            .header("connection", "upgrade")
            .header("upgrade", "websocket")
            .header("sec-websocket-version", "13")
            .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
            .send(),
    )
    .await
    .expect("workspace active route request timed out")
    .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    server.abort();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), server)
        .await
        .expect("workspace active route server shutdown timed out");
}

#[tokio::test]
async fn workspace_active_snapshot_stream_returns_bad_request_for_invalid_workspace_id() {
    let _serial = home_env_test_lock().lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());

    let data_dir = tempfile::tempdir().unwrap();
    let fixture =
        test_daemon_fixture_for_test(data_dir.path(), Some("daemon-secret".to_string())).await;
    let app = fixture.router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = reqwest::Client::new();

    let res = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        client
            .get(format!(
                "http://{addr}/api/workspaces/not-a-workspace/active_snapshot/stream"
            ))
            .header("authorization", "Bearer daemon-secret")
            .header("connection", "upgrade")
            .header("upgrade", "websocket")
            .header("sec-websocket-version", "13")
            .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
            .send(),
    )
    .await
    .expect("workspace active invalid-id request timed out")
    .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    server.abort();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), server)
        .await
        .expect("workspace active invalid-id server shutdown timed out");
}

#[tokio::test]
async fn workspace_vcs_stream_returns_not_found_before_upgrade() {
    let _serial = home_env_test_lock().lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());

    let data_dir = tempfile::tempdir().unwrap();
    let fixture =
        test_daemon_fixture_for_test(data_dir.path(), Some("daemon-secret".to_string())).await;
    let app = fixture.router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = reqwest::Client::new();

    let res = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        client
            .get(format!(
                "http://{addr}/api/workspaces/11111111-1111-1111-1111-111111111111/vcs/stream"
            ))
            .header("authorization", "Bearer daemon-secret")
            .header("connection", "upgrade")
            .header("upgrade", "websocket")
            .header("sec-websocket-version", "13")
            .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
            .send(),
    )
    .await
    .expect("workspace vcs route request timed out")
    .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    server.abort();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), server)
        .await
        .expect("workspace vcs route server shutdown timed out");
}

#[tokio::test]
async fn workspace_vcs_stream_returns_bad_request_for_invalid_workspace_id() {
    let _serial = home_env_test_lock().lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());

    let data_dir = tempfile::tempdir().unwrap();
    let fixture =
        test_daemon_fixture_for_test(data_dir.path(), Some("daemon-secret".to_string())).await;
    let app = fixture.router();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = reqwest::Client::new();

    let res = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        client
            .get(format!(
                "http://{addr}/api/workspaces/not-a-workspace/vcs/stream"
            ))
            .header("authorization", "Bearer daemon-secret")
            .header("connection", "upgrade")
            .header("upgrade", "websocket")
            .header("sec-websocket-version", "13")
            .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
            .send(),
    )
    .await
    .expect("workspace vcs invalid-id request timed out")
    .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    server.abort();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), server)
        .await
        .expect("workspace vcs invalid-id server shutdown timed out");
}
