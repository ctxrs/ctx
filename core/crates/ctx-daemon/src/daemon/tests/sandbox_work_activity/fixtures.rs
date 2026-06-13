use super::*;

pub(super) struct SandboxWorkActivityFixture {
    state: Arc<DaemonState>,
    temp: tempfile::TempDir,
    _disable: EnvVarGuard,
    _serial: tokio::sync::MutexGuard<'static, ()>,
}

impl SandboxWorkActivityFixture {
    pub(super) async fn new() -> Self {
        let serial = sandbox_cli_env_test_lock().lock().await;
        let disable = EnvVarGuard::set("CTX_TEST_SANDBOX_CLI_AVAILABLE", "0");
        let temp = tempdir().unwrap();
        let stores = StoreManager::open(temp.path()).await.unwrap();
        let state = Arc::new(DaemonState::new(
            temp.path().to_path_buf(),
            stores,
            HashMap::new(),
            "http://localhost".to_string(),
            None,
        ));

        Self {
            state,
            temp,
            _disable: disable,
            _serial: serial,
        }
    }

    pub(super) fn state(&self) -> Arc<DaemonState> {
        self.state.clone()
    }

    pub(super) fn root(&self) -> &std::path::Path {
        self.temp.path()
    }
}
