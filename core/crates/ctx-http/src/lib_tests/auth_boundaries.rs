use super::*;

mod browser_capability_tokens;
mod browser_http_bearers;
mod browser_stream_tokens;
mod daemon_http;
mod mcp_session_scope;
mod mobile_tokens;
mod terminal_stream_tokens;

struct AuthBoundaryFixture {
    app: axum::Router,
    fixture: crate::test_support::TestDaemonFixture,
    _home_guard: EnvVarGuard,
    _codex_home_guard: EnvVarGuard,
    _home_dir: tempfile::TempDir,
    _serial: tokio::sync::MutexGuard<'static, ()>,
}

impl AuthBoundaryFixture {
    async fn new() -> Self {
        Self::build(Some("daemon-secret".to_string())).await
    }

    async fn without_auth_token() -> Self {
        Self::build(None).await
    }

    async fn build(auth_token: Option<String>) -> Self {
        let serial = home_env_test_lock().lock().await;
        let home_dir = tempfile::tempdir().unwrap();
        let home_guard = EnvVarGuard::set("HOME", &home_dir.path().to_string_lossy());
        let codex_home_guard = EnvVarGuard::unset("CTX_CODEX_HOME");
        let fixture = crate::test_support::TestDaemonFixture::with_providers_and_auth_token(
            HashMap::new(),
            "http://127.0.0.1:4399",
            auth_token,
        )
        .await;
        let app = fixture.router();
        Self {
            app,
            fixture,
            _home_guard: home_guard,
            _codex_home_guard: codex_home_guard,
            _home_dir: home_dir,
            _serial: serial,
        }
    }

    fn app(&self) -> axum::Router {
        self.app.clone()
    }

    fn daemon(&self) -> &TestDaemon {
        self.fixture.daemon()
    }
}

async fn serve_test_app(app: axum::Router) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, server)
}

async fn websocket_upgrade_status(
    client: &reqwest::Client,
    addr: std::net::SocketAddr,
    path: &str,
) -> StatusCode {
    client
        .get(format!("http://{addr}{path}"))
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .send()
        .await
        .unwrap()
        .status()
}
