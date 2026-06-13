use super::*;

impl RouteBuilder {
    pub fn run_archive(&self) -> RunArchiveHandle {
        RunArchiveHandle::new(self.protected_workspace_store_lookup())
    }
    pub fn org_policy(&self) -> OrgPolicyHandle {
        OrgPolicyHandle::new(self.state.global_store().clone())
    }
    pub fn merge_queue_api(&self) -> MergeQueueApiHandle {
        MergeQueueApiHandle::new(self.merge_queue_route_host())
    }
    pub(super) fn merge_queue_route_host(
        &self,
    ) -> Arc<crate::daemon::merge_queue::MergeQueueRouteHost> {
        crate::daemon::merge_queue::route_host_from_state(self.state.as_ref())
    }
    pub fn update_release(&self) -> UpdateReleaseHandle {
        UpdateReleaseHandle::new(self.state.core.data_root.clone())
    }
    pub fn update_activity(&self) -> UpdateActivityHandle {
        UpdateActivityHandle::new(
            self.state.global_store().clone(),
            self.state.core.stores.clone(),
            Arc::clone(&self.state.core.update_drain),
            self.state.core.data_root.clone(),
        )
    }
    pub fn settings(&self) -> SettingsHandle {
        SettingsHandle::new(
            self.state.global_store().clone(),
            self.state.telemetry.telemetry.clone(),
            self.state.telemetry.perf_telemetry.clone(),
            Arc::clone(&self.state.telemetry.resource_sampler),
            Arc::clone(&self.state.telemetry.resource_governance),
            Arc::clone(&self.state.providers),
            Arc::clone(&self.state.transport.terminals),
        )
    }
    pub fn resource_utilization(&self) -> ResourceUtilizationHandle {
        ResourceUtilizationHandle::new(
            ProtectedWorkspaceStoreLookup::new(
                self.state.core.stores.clone(),
                Arc::clone(&self.state.sessions),
                Arc::clone(&self.state.transport.merge_queue),
            ),
            Arc::clone(&self.state.providers),
            Arc::clone(&self.state.telemetry.resource_sampler),
        )
    }
    pub fn telemetry(&self) -> TelemetryHandle {
        TelemetryHandle::new(self.state.core.data_root.clone(), &self.state.telemetry)
    }
    pub fn update_drain(&self) -> UpdateDrainHandle {
        UpdateDrainHandle::new(
            self.state.global_store().clone(),
            self.state.core.stores.clone(),
            Arc::clone(&self.state.core.update_drain),
        )
    }
    pub(super) fn daemon_shutdown_with_session_routes(
        &self,
        session_routes: &session_deps::SessionRouteDeps,
    ) -> DaemonShutdownHandle {
        let shutdown_host = DaemonShutdownHost::new(DaemonShutdownHostParts {
            global_store: self.state.global_store().clone(),
            stores: self.state.core.stores.clone(),
            session_stores: session_routes.session_store_lookup(),
            session_lifecycle: Arc::clone(&self.state.sessions),
            session_publication: session_routes.session_publication_effects(),
            provider_lifecycle: Arc::clone(&self.state.providers),
            update_drain: Arc::clone(&self.state.core.update_drain),
            substrate_lifecycle: Arc::clone(&self.state.execution.harness),
            shutdown_signal: self.state.core.shutdown_tx.clone(),
        });
        DaemonShutdownHandle::new(self.state.core.local_shutdown_token.clone(), shutdown_host)
    }
}
