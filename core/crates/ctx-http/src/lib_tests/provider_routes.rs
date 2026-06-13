use super::*;

mod agent_server_config;
mod bootstrap;
mod claude_routes;
mod codex_routes;
mod cursor_routes;
mod delete_paths;
mod harness_config;
mod login_routes;

struct ProviderRouteFixture {
    app: axum::Router,
    fixture: crate::test_support::TestDaemonFixture,
    _home_guard: EnvVarGuard,
    _codex_home_guard: EnvVarGuard,
    _home_dir: tempfile::TempDir,
    _codex_home_dir: Option<tempfile::TempDir>,
    _serial: tokio::sync::MutexGuard<'static, ()>,
}

impl ProviderRouteFixture {
    async fn new() -> Self {
        Self::build(None, false).await
    }

    async fn with_auth_token(auth_token: &str) -> Self {
        Self::build(Some(auth_token.to_string()), false).await
    }

    async fn with_codex_home() -> Self {
        Self::build(None, true).await
    }

    async fn build(auth_token: Option<String>, with_codex_home: bool) -> Self {
        let serial = home_env_test_lock().lock().await;
        let home_dir = tempfile::tempdir().unwrap();
        let home_guard = EnvVarGuard::set("HOME", &home_dir.path().to_string_lossy());
        let codex_home_dir = with_codex_home.then(|| tempfile::tempdir().unwrap());
        let codex_home_guard = if let Some(dir) = &codex_home_dir {
            EnvVarGuard::set("CTX_CODEX_HOME", &dir.path().to_string_lossy())
        } else {
            EnvVarGuard::unset("CTX_CODEX_HOME")
        };
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
            _codex_home_dir: codex_home_dir,
            _serial: serial,
        }
    }

    fn app(&self) -> axum::Router {
        self.app.clone()
    }

    fn daemon(&self) -> &TestDaemon {
        self.fixture.daemon()
    }

    fn data_root(&self) -> &std::path::Path {
        self.fixture.data_root()
    }
}

fn write_invalid_harness_registry(data_root: &std::path::Path) {
    let path = data_root
        .join("providers")
        .join("harness_sources")
        .join("registry.json");
    std::fs::create_dir_all(path.parent().expect("registry parent")).unwrap();
    std::fs::write(path, "{ not valid json").unwrap();
}

fn write_invalid_agent_server_config(data_root: &std::path::Path) {
    let path = data_root
        .join("providers")
        .join("agent-servers")
        .join("agent_servers.json");
    std::fs::create_dir_all(path.parent().expect("agent server config parent")).unwrap();
    std::fs::write(path, "{ not valid json").unwrap();
}

fn clear_agent_server_config(data_root: &std::path::Path) {
    let path = data_root
        .join("providers")
        .join("agent-servers")
        .join("agent_servers.json");
    if path.exists() {
        std::fs::remove_file(path).unwrap();
    }
}
