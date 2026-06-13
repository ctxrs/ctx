use super::*;

#[tokio::test]
async fn mobile_secure_workspace_stream_returns_unauthorized_before_upgrade_for_missing_workspace_without_mobile_access(
) {
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

    let res = client
        .get(format!(
            "http://{addr}/api/mobile/secure/workspaces/11111111-1111-1111-1111-111111111111/stream?device_id=22222222-2222-2222-2222-222222222222&token=bad-token"
        ))
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    server.abort();
}

#[tokio::test]
async fn mobile_secure_workspace_stream_returns_bad_request_for_invalid_workspace_id_before_upgrade(
) {
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

    let res = client
        .get(format!(
            "http://{addr}/api/mobile/secure/workspaces/not-a-workspace/stream?device_id=22222222-2222-2222-2222-222222222222&token=bad-token"
        ))
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    server.abort();
}

#[tokio::test]
async fn mobile_secure_workspace_stream_returns_not_found_before_upgrade_for_authorized_missing_workspace(
) {
    let _serial = home_env_test_lock().lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());

    let (app, _daemon, _workspace_id, device_id, key, _data_dir) =
        build_mobile_access_app(true).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = reqwest::Client::new();

    let missing_workspace_id =
        WorkspaceId(uuid::Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap());
    let query = mobile_secure_stream_query(&device_id, &key, missing_workspace_id);
    let res = client
        .get(format!(
            "http://{addr}/api/mobile/secure/workspaces/{}/stream?{query}",
            missing_workspace_id.0
        ))
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    server.abort();
}

#[tokio::test]
async fn mobile_secure_workspace_stream_returns_unauthorized_before_upgrade_without_mobile_access()
{
    let _serial = home_env_test_lock().lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());

    let git_repo = setup_git_repo().await;
    let data_dir = tempfile::tempdir().unwrap();
    let fixture = test_daemon_fixture_for_test(data_dir.path(), None).await;
    let app = fixture.router();
    let workspace = create_workspace_via_api(&app, &git_repo.path().to_string_lossy()).await;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = reqwest::Client::new();

    let res = client
        .get(format!(
            "http://{addr}/api/mobile/secure/workspaces/{}/stream?device_id=22222222-2222-2222-2222-222222222222&token=bad-token",
            workspace.id.0
        ))
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    server.abort();
}

#[tokio::test]
async fn mobile_secure_workspace_stream_rejects_disabled_mobile_access_before_upgrade() {
    let _serial = home_env_test_lock().lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());

    let (app, _daemon, workspace_id, device_id, key, _data_dir) =
        build_mobile_access_app(false).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = reqwest::Client::new();

    let query = mobile_secure_stream_query(&device_id, &key, workspace_id);
    let res = client
        .get(format!(
            "http://{addr}/api/mobile/secure/workspaces/{}/stream?{query}",
            workspace_id.0
        ))
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    server.abort();
}

#[tokio::test]
async fn mobile_secure_workspace_stream_returns_unauthorized_without_workspace_stream_scope() {
    let _serial = home_env_test_lock().lock().await;
    let home = tempfile::tempdir().unwrap();
    let _home = EnvVarGuard::set("HOME", &home.path().to_string_lossy());

    let (app, _daemon, workspace_id, device_id, key, _data_dir) =
        build_mobile_access_app_with_scopes(true, &["device_registration", "workspace_read"]).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = reqwest::Client::new();

    let query = mobile_secure_stream_query(&device_id, &key, workspace_id);
    let res = client
        .get(format!(
            "http://{addr}/api/mobile/secure/workspaces/{}/stream?{query}",
            workspace_id.0
        ))
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    server.abort();
}
