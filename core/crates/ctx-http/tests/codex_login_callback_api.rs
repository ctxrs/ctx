use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use ctx_daemon::test_support::TestDaemon;
use ctx_provider_accounts::{
    save_codex_registry, CodexAccountEntry, CodexAccountRegistry, CodexEndpointProfile,
    CODEX_API_SHAPE_OPENAI_RESPONSES, CODEX_CREDENTIAL_KIND_API_KEY,
};
use serde::Deserialize;
use serde_json::json;

mod common;

#[derive(Debug, Deserialize)]
struct CompleteResp {
    accepted: bool,
    status_code: u16,
}

#[derive(Debug, Deserialize)]
struct ErrorResp {
    error: String,
}

async fn start_callback_server() -> (String, tokio::task::JoinHandle<()>) {
    let app = axum::Router::new().route(
        "/auth/callback",
        axum::routing::get(|| async { StatusCode::OK }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://127.0.0.1:{}", addr.port()), handle)
}

async fn start_delayed_callback_server(
    delay: Duration,
) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let hits = Arc::new(AtomicUsize::new(0));
    let route_hits = hits.clone();
    let app = axum::Router::new().route(
        "/auth/callback",
        axum::routing::get(move || {
            let route_hits = route_hits.clone();
            async move {
                route_hits.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(delay).await;
                StatusCode::OK
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://127.0.0.1:{}", addr.port()), hits, handle)
}

async fn start_redirecting_callback_server(
) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let redirected_hits = Arc::new(AtomicUsize::new(0));
    let route_hits = redirected_hits.clone();
    let app = axum::Router::new()
        .route(
            "/auth/callback",
            axum::routing::get(|| async {
                (
                    StatusCode::FOUND,
                    [(axum::http::header::LOCATION, "/redirected")],
                )
            }),
        )
        .route(
            "/redirected",
            axum::routing::get(move || {
                let route_hits = route_hits.clone();
                async move {
                    route_hits.fetch_add(1, Ordering::SeqCst);
                    StatusCode::OK
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (
        format!("http://127.0.0.1:{}", addr.port()),
        redirected_hits,
        handle,
    )
}

async fn insert_pending_login(
    daemon: &TestDaemon,
    account_id: &str,
    expected_callback_url: Option<&str>,
) -> String {
    daemon
        .seed_pending_codex_login_for_test(account_id, expected_callback_url)
        .await
}

async fn start_test_app() -> (common::FakeDaemonFixture, common::TestServer) {
    let fixture = common::fake_daemon_fixture("http://127.0.0.1:0").await;
    let server = fixture.spawn_server().await;
    (fixture, server)
}

#[tokio::test]
async fn complete_login_replays_loopback_callback_and_clears_token() {
    let (fixture, server) = start_test_app().await;
    let daemon = &fixture.daemon;
    let (callback_base, callback_handle) = start_callback_server().await;
    let expected_callback = format!("{callback_base}/auth/callback");
    let callback_url = format!("{expected_callback}?code=abc&state=xyz");
    let account_id = "acct-success";
    let token = insert_pending_login(daemon, account_id, Some(&expected_callback)).await;

    let base = &server.base_url;
    let client = &server.client;
    let resp = client
        .post(format!(
            "{base}/api/providers/codex/accounts/login/{account_id}"
        ))
        .json(&json!({
            "callback_url": callback_url,
            "completion_token": token
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: CompleteResp = resp.json().await.unwrap();
    assert!(body.accepted);
    assert_eq!(body.status_code, 200);

    let completion_token = daemon
        .codex_login_completion_token_state_for_test(account_id)
        .await
        .expect("login status");
    assert!(
        completion_token.is_none(),
        "completion token should be single-use"
    );

    callback_handle.abort();
}

#[tokio::test]
async fn complete_login_replays_localhost_callback_via_ipv4_override() {
    let (fixture, server) = start_test_app().await;
    let daemon = &fixture.daemon;
    let (callback_base, callback_handle) = start_callback_server().await;
    let callback_port = callback_base
        .rsplit_once(':')
        .expect("callback base port")
        .1
        .parse::<u16>()
        .expect("callback port");
    let expected_callback = format!("http://localhost:{callback_port}/auth/callback");
    let callback_url = format!("{expected_callback}?code=abc&state=xyz");
    let account_id = "acct-localhost";
    let token = insert_pending_login(daemon, account_id, Some(&expected_callback)).await;

    let base = &server.base_url;
    let client = &server.client;
    let resp = client
        .post(format!(
            "{base}/api/providers/codex/accounts/login/{account_id}"
        ))
        .json(&json!({
            "callback_url": callback_url,
            "completion_token": token
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body: CompleteResp = resp.json().await.unwrap();
    assert!(body.accepted);
    assert_eq!(body.status_code, 200);

    callback_handle.abort();
}

#[tokio::test]
async fn complete_login_rejects_invalid_completion_token() {
    let (fixture, server) = start_test_app().await;
    let daemon = &fixture.daemon;
    let expected_callback = "http://localhost:43210/auth/callback";
    let _token = insert_pending_login(daemon, "acct-token", Some(expected_callback)).await;
    let base = &server.base_url;
    let client = &server.client;

    let resp = client
        .post(format!(
            "{base}/api/providers/codex/accounts/login/acct-token"
        ))
        .json(&json!({
            "callback_url": "http://localhost:43210/auth/callback?code=abc",
            "completion_token": "wrong-token"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let body: ErrorResp = resp.json().await.unwrap();
    assert!(body.error.contains("invalid completion token"));
}

#[tokio::test]
async fn complete_login_rejects_missing_expected_callback_metadata() {
    let (fixture, server) = start_test_app().await;
    let daemon = &fixture.daemon;
    let (callback_base, callback_hits, callback_handle) =
        start_delayed_callback_server(Duration::from_millis(0)).await;
    let callback_url = format!("{callback_base}/auth/callback?code=abc");
    let account_id = "acct-missing-expected-callback";
    let token = insert_pending_login(daemon, account_id, None).await;
    let base = &server.base_url;
    let client = &server.client;

    let resp = client
        .post(format!(
            "{base}/api/providers/codex/accounts/login/{account_id}"
        ))
        .json(&json!({
            "callback_url": callback_url,
            "completion_token": token
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CONFLICT);
    let body: ErrorResp = resp.json().await.unwrap();
    assert!(body.error.contains("expected callback"));

    let completion_token = daemon
        .codex_login_completion_token_state_for_test(account_id)
        .await
        .expect("login status");
    assert_eq!(completion_token.as_deref(), Some(token.as_str()));
    assert_eq!(callback_hits.load(Ordering::SeqCst), 0);

    callback_handle.abort();
}

#[tokio::test]
async fn complete_login_rejects_non_loopback_host() {
    let (fixture, server) = start_test_app().await;
    let daemon = &fixture.daemon;
    let token = insert_pending_login(
        daemon,
        "acct-host",
        Some("http://localhost:12345/auth/callback"),
    )
    .await;
    let base = &server.base_url;
    let client = &server.client;

    let resp = client
        .post(format!(
            "{base}/api/providers/codex/accounts/login/acct-host"
        ))
        .json(&json!({
            "callback_url": "http://example.com:12345/auth/callback?code=abc",
            "completion_token": token
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: ErrorResp = resp.json().await.unwrap();
    assert!(body.error.contains("loopback"));
}

#[tokio::test]
async fn complete_login_rejects_expected_path_mismatch() {
    let (fixture, server) = start_test_app().await;
    let daemon = &fixture.daemon;
    let token = insert_pending_login(
        daemon,
        "acct-path",
        Some("http://localhost:24567/auth/callback"),
    )
    .await;
    let base = &server.base_url;
    let client = &server.client;

    let resp = client
        .post(format!(
            "{base}/api/providers/codex/accounts/login/acct-path"
        ))
        .json(&json!({
            "callback_url": "http://localhost:24567/auth/other?code=abc",
            "completion_token": token
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: ErrorResp = resp.json().await.unwrap();
    assert!(body.error.contains("path"));
}

#[tokio::test]
async fn complete_login_accepts_loopback_alias_for_expected_host() {
    let (fixture, server) = start_test_app().await;
    let daemon = &fixture.daemon;
    let (callback_base, callback_handle) = start_callback_server().await;
    let expected_callback = format!("{callback_base}/auth/callback");
    let parsed = reqwest::Url::parse(&expected_callback).unwrap();
    let port = parsed.port().unwrap();
    let token = insert_pending_login(daemon, "acct-host-mismatch", Some(&expected_callback)).await;
    let base = &server.base_url;
    let client = &server.client;

    let resp = client
        .post(format!(
            "{base}/api/providers/codex/accounts/login/acct-host-mismatch"
        ))
        .json(&json!({
            "callback_url": format!("http://localhost:{port}/auth/callback?code=abc"),
            "completion_token": token
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    callback_handle.abort();
}

#[tokio::test]
async fn complete_login_token_is_single_use() {
    let (fixture, server) = start_test_app().await;
    let daemon = &fixture.daemon;
    let (callback_base, callback_handle) = start_callback_server().await;
    let expected_callback = format!("{callback_base}/auth/callback");
    let callback_url = format!("{expected_callback}?code=one-time");
    let token = insert_pending_login(daemon, "acct-replay", Some(&expected_callback)).await;

    let base = &server.base_url;
    let client = &server.client;
    let first = client
        .post(format!(
            "{base}/api/providers/codex/accounts/login/acct-replay"
        ))
        .json(&json!({
            "callback_url": callback_url,
            "completion_token": token
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);

    let second = client
        .post(format!(
            "{base}/api/providers/codex/accounts/login/acct-replay"
        ))
        .json(&json!({
            "callback_url": "http://localhost:1/auth/callback?code=replay",
            "completion_token": token
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::UNAUTHORIZED);
    let body: ErrorResp = second.json().await.unwrap();
    assert!(body.error.contains("invalid completion token"));

    callback_handle.abort();
}

#[tokio::test]
async fn complete_login_allows_only_one_concurrent_callback_replay() {
    let (fixture, server) = start_test_app().await;
    let daemon = &fixture.daemon;
    let (callback_base, callback_hits, callback_handle) =
        start_delayed_callback_server(Duration::from_millis(250)).await;
    let expected_callback = format!("{callback_base}/auth/callback");
    let callback_url = format!("{expected_callback}?code=race");
    let token = insert_pending_login(daemon, "acct-race", Some(&expected_callback)).await;

    let base = &server.base_url;
    let client = &server.client;
    let request_url = format!("{base}/api/providers/codex/accounts/login/acct-race");
    let payload = json!({
        "callback_url": callback_url,
        "completion_token": token
    });

    let first = client.post(request_url.clone()).json(&payload).send();
    let second = client.post(request_url).json(&payload).send();
    let (first, second) = tokio::join!(first, second);
    let statuses = [first.unwrap().status(), second.unwrap().status()];

    assert!(statuses.contains(&StatusCode::OK));
    assert!(statuses.contains(&StatusCode::UNAUTHORIZED));
    assert_eq!(callback_hits.load(Ordering::SeqCst), 1);

    callback_handle.abort();
}

#[tokio::test]
async fn complete_login_rejects_redirecting_callback_replay() {
    let (fixture, server) = start_test_app().await;
    let daemon = &fixture.daemon;
    let (callback_base, redirected_hits, callback_handle) =
        start_redirecting_callback_server().await;
    let expected_callback = format!("{callback_base}/auth/callback");
    let callback_url = format!("{expected_callback}?code=redirect");
    let token = insert_pending_login(daemon, "acct-redirect", Some(&expected_callback)).await;

    let base = &server.base_url;
    let client = &server.client;
    let resp = client
        .post(format!(
            "{base}/api/providers/codex/accounts/login/acct-redirect"
        ))
        .json(&json!({
            "callback_url": callback_url,
            "completion_token": token
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    let body: ErrorResp = resp.json().await.unwrap();
    assert!(body.error.contains("302"));
    assert_eq!(redirected_hits.load(Ordering::SeqCst), 0);

    callback_handle.abort();
}

#[tokio::test]
async fn set_active_account_rejects_incompatible_endpoint_profile() {
    let (fixture, server) = start_test_app().await;
    let registry = CodexAccountRegistry {
        active_account_id: None,
        accounts: vec![CodexAccountEntry {
            id: "acct-incompatible".to_string(),
            label: "Incompatible".to_string(),
            kind: CODEX_CREDENTIAL_KIND_API_KEY.to_string(),
            email: None,
            provider_account_id: None,
            plan_type: None,
            created_at: chrono::Utc::now(),
            last_used_at: None,
            secret_ref: None,
            endpoint_profile: CodexEndpointProfile {
                api_shape: CODEX_API_SHAPE_OPENAI_RESPONSES.to_string(),
                auth_type: "basic".to_string(),
                base_url: Some("https://example.com/v1".to_string()),
            },
        }],
    };
    save_codex_registry(fixture.data_dir.path(), &registry)
        .await
        .unwrap();

    let base = &server.base_url;
    let client = &server.client;
    let resp = client
        .put(format!("{base}/api/providers/codex/active-account"))
        .json(&json!({ "account_id": "acct-incompatible" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body: ErrorResp = resp.json().await.unwrap();
    assert!(body.error.contains("auth_type=bearer"));
}
