use super::*;

#[tokio::test]
async fn dictation_websocket_stream_requires_browser_scoped_query_token() {
    let fixture = AuthBoundaryFixture::new().await;
    let app = fixture.app();

    let expires_at = chrono::Utc::now().timestamp() + STREAM_TOKEN_TTL_SECS;
    let scoped_token = derive_browser_stream_token(
        "daemon-secret",
        &BrowserStreamAuthScope::DictationLivekit,
        expires_at,
    );
    let (addr, server) = serve_test_app(app).await;
    let client = reqwest::Client::new();

    assert_eq!(
        websocket_upgrade_status(
            &client,
            addr,
            "/api/dictation/livekit/stream?token=daemon-secret",
        )
        .await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        websocket_upgrade_status(&client, addr, "/api/dictation/livekit/stream").await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        websocket_upgrade_status(
            &client,
            addr,
            &format!("/api/dictation/livekit/stream?expires_at={expires_at}&token={scoped_token}"),
        )
        .await,
        StatusCode::SWITCHING_PROTOCOLS
    );

    server.abort();
}
