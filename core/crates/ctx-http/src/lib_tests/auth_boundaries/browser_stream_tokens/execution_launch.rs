use super::*;

#[tokio::test]
async fn execution_launch_stream_requires_browser_scoped_query_token() {
    let fixture = AuthBoundaryFixture::new().await;
    let app = fixture.app();
    let job_id = "job-auth-boundary";

    let expires_at = chrono::Utc::now().timestamp() + STREAM_TOKEN_TTL_SECS;
    let scoped_token = derive_browser_stream_token(
        "daemon-secret",
        &BrowserStreamAuthScope::ExecutionLaunch {
            job_id: job_id.to_string(),
        },
        expires_at,
    );
    let (addr, server) = serve_test_app(app).await;
    let client = reqwest::Client::new();

    assert_eq!(
        websocket_upgrade_status(
            &client,
            addr,
            &format!("/api/execution/launch/stream?job_id={job_id}&token=daemon-secret"),
        )
        .await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        websocket_upgrade_status(
            &client,
            addr,
            &format!("/api/execution/launch/stream?job_id={job_id}"),
        )
        .await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        websocket_upgrade_status(
            &client,
            addr,
            &format!(
                "/api/execution/launch/stream?job_id={job_id}&expires_at={expires_at}&token={scoped_token}"
            ),
        )
        .await,
        StatusCode::NOT_FOUND
    );

    server.abort();
}
