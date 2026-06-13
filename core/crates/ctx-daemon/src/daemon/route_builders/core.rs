use super::*;

impl RouteBuilder {
    pub fn auth(&self) -> AuthHandle {
        AuthHandle::new(
            self.state.core.auth_token.clone(),
            Arc::clone(&self.state.core.mcp_auth),
            self.state.global_store().clone(),
            self.state.telemetry.ops_events.clone(),
        )
    }
    pub fn health(&self) -> HealthHandle {
        HealthHandle::new(
            self.state.core.data_root.clone(),
            self.state.core.daemon_url.clone(),
            self.state.core.auth_token.clone(),
            Arc::clone(&self.state.core.storage_guard),
        )
    }
    pub fn diagnostics(&self) -> DiagnosticsHandle {
        DiagnosticsHandle::new(
            self.health(),
            self.state.core.data_root.clone(),
            Arc::clone(&self.state.execution.setup),
            Arc::clone(&self.state.providers),
        )
    }
    pub fn blob(&self) -> BlobHandle {
        BlobHandle::new(
            self.state.core.data_root.clone(),
            self.state.global_store().clone(),
        )
    }
    pub fn request_base(&self) -> RequestBaseHandle {
        RequestBaseHandle::new(
            self.state.core.daemon_url.clone(),
            self.state.core.public_base_url.clone(),
        )
    }
    pub fn repo_onboarding(&self) -> RepoOnboardingHandle {
        RepoOnboardingHandle::new(self.state.core.data_root.clone())
    }
    pub fn logs(&self) -> LogsHandle {
        LogsHandle::new(self.state.core.data_root.clone())
    }
    pub fn dictation(&self) -> DictationHandle {
        DictationHandle::new(self.state.global_store().clone())
    }
}
