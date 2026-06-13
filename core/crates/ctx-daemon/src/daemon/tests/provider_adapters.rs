use super::*;

#[derive(Default)]
pub(super) struct RecordingProviderAdapter {
    restart_calls: StdMutex<Vec<(String, ProviderRestartMode)>>,
    reap_calls: StdMutex<Vec<ProviderSessionSweepConfig>>,
    reap_result: StdMutex<ProviderSessionSweepStats>,
    pin_calls: StdMutex<Vec<(String, bool)>>,
}

impl RecordingProviderAdapter {
    pub(super) fn restart_calls(&self) -> Vec<(String, ProviderRestartMode)> {
        self.restart_calls
            .lock()
            .expect("recording adapter restart lock")
            .clone()
    }

    pub(super) fn reap_calls(&self) -> Vec<ProviderSessionSweepConfig> {
        self.reap_calls
            .lock()
            .expect("recording adapter reap lock")
            .clone()
    }

    pub(super) fn set_reap_result(&self, stats: ProviderSessionSweepStats) {
        *self
            .reap_result
            .lock()
            .expect("recording adapter reap result lock") = stats;
    }

    pub(super) fn pin_calls(&self) -> Vec<(String, bool)> {
        self.pin_calls
            .lock()
            .expect("recording adapter pin lock")
            .clone()
    }
}

#[derive(Default)]
pub(super) struct BlockingInspectAdapter {
    pub(super) inspect_started: AtomicBool,
    pub(super) release_inspect: tokio::sync::Notify,
}

#[async_trait]
impl ProviderAdapter for RecordingProviderAdapter {
    async fn inspect(&self) -> Result<ProviderStatus> {
        Ok(ProviderStatus {
            provider_id: "recording".into(),
            installed: true,
            detected_path: None,
            version: Some("test".into()),
            capabilities: Some(ProviderCapabilities {
                stream_events: false,
                stream_format: "jsonl".into(),
                has_turn_boundaries: true,
                has_tool_call_ids: false,
                has_file_change_events: false,
                has_command_events: false,
                supports_resume: false,
                supports_stable_session_id: false,
                supports_fork_or_rewind: false,
                supports_headless: true,
                supports_server_mode: false,
                supports_interactive_tui: false,
                supports_private_state_dir: false,
                supports_sandbox_flags: false,
                supports_approval_flags: false,
                notes: Vec::new(),
            }),
            health: ProviderHealth::Ok,
            diagnostics: Vec::new(),
            details: HashMap::new(),
            usability: ProviderUsability::default(),
        })
    }

    async fn run(
        &self,
        _input: TurnInput,
        _workdir: PathBuf,
        _env: HashMap<String, String>,
        _event_sink: tokio::sync::mpsc::Sender<ctx_providers::events::NormalizedEvent>,
        _hooks: ctx_providers::adapters::ProviderRunHooks,
    ) -> Result<RunHandle> {
        anyhow::bail!("not used in test");
    }

    async fn cancel(&self, _handle: &mut RunHandle) -> Result<()> {
        Ok(())
    }

    async fn list_processes(&self) -> Vec<ProviderProcessInfo> {
        Vec::new()
    }

    async fn restart(&self, reason: &str, mode: ProviderRestartMode) -> Result<()> {
        self.restart_calls
            .lock()
            .expect("recording adapter restart lock")
            .push((reason.to_string(), mode));
        Ok(())
    }

    async fn reap_idle_sessions(
        &self,
        config: ProviderSessionSweepConfig,
    ) -> Result<ProviderSessionSweepStats> {
        self.reap_calls
            .lock()
            .expect("recording adapter reap lock")
            .push(config);
        Ok(*self
            .reap_result
            .lock()
            .expect("recording adapter reap result lock"))
    }

    async fn set_session_pinned(&self, session_key: String, pinned: bool) -> Result<()> {
        self.pin_calls
            .lock()
            .expect("recording adapter pin lock")
            .push((session_key, pinned));
        Ok(())
    }
}

#[async_trait]
impl ProviderAdapter for BlockingInspectAdapter {
    async fn inspect(&self) -> Result<ProviderStatus> {
        self.inspect_started.store(true, Ordering::SeqCst);
        self.release_inspect.notified().await;
        Ok(ProviderStatus {
            provider_id: "blocking".into(),
            installed: true,
            detected_path: None,
            version: Some("test".into()),
            capabilities: None,
            health: ProviderHealth::Ok,
            diagnostics: Vec::new(),
            details: HashMap::new(),
            usability: ProviderUsability::default(),
        })
    }

    async fn run(
        &self,
        _input: TurnInput,
        _workdir: PathBuf,
        _env: HashMap<String, String>,
        _event_sink: tokio::sync::mpsc::Sender<ctx_providers::events::NormalizedEvent>,
        _hooks: ctx_providers::adapters::ProviderRunHooks,
    ) -> Result<RunHandle> {
        anyhow::bail!("not used in test");
    }

    async fn cancel(&self, _handle: &mut RunHandle) -> Result<()> {
        Ok(())
    }

    async fn list_processes(&self) -> Vec<ProviderProcessInfo> {
        Vec::new()
    }
}
