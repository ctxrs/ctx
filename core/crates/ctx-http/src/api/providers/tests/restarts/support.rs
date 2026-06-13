use super::*;

#[derive(Default)]
pub(super) struct RestartTrackingAdapter {
    pub(super) restart_calls: AtomicUsize,
    pub(super) restart_modes: std::sync::Mutex<Vec<ProviderRestartMode>>,
}

#[async_trait::async_trait]
impl ProviderAdapter for RestartTrackingAdapter {
    async fn inspect(&self) -> anyhow::Result<ProviderStatus> {
        Ok(ProviderStatus {
            provider_id: "codex".to_string(),
            installed: true,
            detected_path: None,
            version: Some("test".to_string()),
            capabilities: None,
            health: ProviderHealth::Ok,
            diagnostics: Vec::new(),
            details: HashMap::new(),
            usability: ctx_providers::adapters::ProviderUsability::default(),
        })
    }

    async fn run(
        &self,
        _input: TurnInput,
        _workdir: PathBuf,
        _env: HashMap<String, String>,
        _event_sink: tokio::sync::mpsc::Sender<ctx_providers::events::NormalizedEvent>,
        _hooks: ctx_providers::adapters::ProviderRunHooks,
    ) -> anyhow::Result<RunHandle> {
        anyhow::bail!("run not used in this test")
    }

    async fn cancel(&self, _handle: &mut RunHandle) -> anyhow::Result<()> {
        Ok(())
    }

    async fn list_processes(&self) -> Vec<ProviderProcessInfo> {
        Vec::new()
    }

    async fn restart(&self, _reason: &str, mode: ProviderRestartMode) -> anyhow::Result<()> {
        self.restart_calls.fetch_add(1, Ordering::SeqCst);
        self.restart_modes
            .lock()
            .expect("restart mode lock")
            .push(mode);
        Ok(())
    }

    fn supports_restart_mode(&self, _mode: ProviderRestartMode) -> bool {
        true
    }
}

#[derive(Default)]
pub(super) struct RestartFailingAdapter {
    pub(super) restart_calls: AtomicUsize,
    pub(super) restart_modes: std::sync::Mutex<Vec<ProviderRestartMode>>,
}

#[async_trait::async_trait]
impl ProviderAdapter for RestartFailingAdapter {
    async fn inspect(&self) -> anyhow::Result<ProviderStatus> {
        Ok(ProviderStatus {
            provider_id: "codex".to_string(),
            installed: true,
            detected_path: None,
            version: Some("test".to_string()),
            capabilities: None,
            health: ProviderHealth::Ok,
            diagnostics: Vec::new(),
            details: HashMap::new(),
            usability: ctx_providers::adapters::ProviderUsability::default(),
        })
    }

    async fn run(
        &self,
        _input: TurnInput,
        _workdir: PathBuf,
        _env: HashMap<String, String>,
        _event_sink: tokio::sync::mpsc::Sender<ctx_providers::events::NormalizedEvent>,
        _hooks: ctx_providers::adapters::ProviderRunHooks,
    ) -> anyhow::Result<RunHandle> {
        anyhow::bail!("run not used in this test")
    }

    async fn cancel(&self, _handle: &mut RunHandle) -> anyhow::Result<()> {
        Ok(())
    }

    async fn list_processes(&self) -> Vec<ProviderProcessInfo> {
        Vec::new()
    }

    async fn restart(&self, _reason: &str, mode: ProviderRestartMode) -> anyhow::Result<()> {
        self.restart_calls.fetch_add(1, Ordering::SeqCst);
        self.restart_modes
            .lock()
            .expect("restart mode lock")
            .push(mode);
        anyhow::bail!("restart failed")
    }

    fn supports_restart_mode(&self, _mode: ProviderRestartMode) -> bool {
        true
    }
}

pub(super) struct UnsupportedRestartAdapter;

#[async_trait::async_trait]
impl ProviderAdapter for UnsupportedRestartAdapter {
    async fn inspect(&self) -> anyhow::Result<ProviderStatus> {
        Ok(ProviderStatus {
            provider_id: "codex".to_string(),
            installed: true,
            detected_path: None,
            version: Some("test".to_string()),
            capabilities: None,
            health: ProviderHealth::Ok,
            diagnostics: Vec::new(),
            details: HashMap::new(),
            usability: ctx_providers::adapters::ProviderUsability::default(),
        })
    }

    async fn run(
        &self,
        _input: TurnInput,
        _workdir: PathBuf,
        _env: HashMap<String, String>,
        _event_sink: tokio::sync::mpsc::Sender<ctx_providers::events::NormalizedEvent>,
        _hooks: ctx_providers::adapters::ProviderRunHooks,
    ) -> anyhow::Result<RunHandle> {
        anyhow::bail!("run not used in this test")
    }

    async fn cancel(&self, _handle: &mut RunHandle) -> anyhow::Result<()> {
        Ok(())
    }

    async fn list_processes(&self) -> Vec<ProviderProcessInfo> {
        Vec::new()
    }
}
