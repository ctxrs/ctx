use super::*;

impl transport_deps::TransportRouteDeps {
    pub fn mobile_store(&self) -> MobileStoreHandle {
        MobileStoreHandle::new(self.global_store.clone())
    }
    pub fn mobile_runtime(&self) -> MobileRuntimeHandle {
        MobileRuntimeHandle::new(
            self.global_store.clone(),
            self.mobile_tunnel.clone(),
            self.daemon_url.clone(),
            self.auth_token_configured,
        )
    }
    pub fn mobile_secure_proxy(&self) -> MobileSecureProxyHandle {
        MobileSecureProxyHandle::new(
            self.global_store.clone(),
            self.health.clone(),
            self.telemetry.clone(),
        )
    }
    fn terminal_launch_host(&self) -> TerminalLaunchHost {
        TerminalLaunchHost::new(
            self.global_store.clone(),
            self.workspace_store_lookup(),
            self.data_root.clone(),
            self.daemon_url.clone(),
            Arc::clone(&self.harness),
            Arc::clone(&self.terminals),
        )
    }
    fn web_session_worker_runtime_host(&self) -> WebSessionWorkerRuntimeHost {
        WebSessionWorkerRuntimeHost::new(
            self.data_root.clone(),
            Arc::clone(&self.providers),
            self.ops_events.clone(),
        )
    }
    fn web_session_launch_host(&self) -> WebSessionLaunchHost {
        WebSessionLaunchHost::new(
            self.global_store.clone(),
            self.workspace_store_lookup(),
            self.data_root.clone(),
            self.web_session_worker_runtime_host(),
            Arc::clone(&self.web_sessions),
        )
    }
    pub fn terminal_route(&self) -> TerminalRouteHandle {
        TerminalRouteHandle::new(Arc::clone(&self.terminals), self.terminal_launch_host())
    }
    pub fn web_session_route(&self) -> WebSessionRouteHandle {
        WebSessionRouteHandle::new(
            Arc::clone(&self.web_sessions),
            self.web_session_launch_host(),
        )
    }
}
