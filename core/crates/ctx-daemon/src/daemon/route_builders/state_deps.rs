use super::*;

impl RouteBuilder {
    pub(super) fn execution_route_deps(&self) -> execution_deps::ExecutionRouteDeps {
        execution_deps::ExecutionRouteDeps::new(execution_deps::ExecutionRouteDepsParts {
            data_root: self.state.core.data_root.clone(),
            daemon_url: self.state.core.daemon_url.clone(),
            global_store: self.state.global_store().clone(),
            stores: self.state.core.stores.clone(),
            update_drain: Arc::clone(&self.state.core.update_drain),
            execution_setup: Arc::clone(&self.state.execution.setup),
            harness: Arc::clone(&self.state.execution.harness),
            terminals: Arc::clone(&self.state.transport.terminals),
        })
    }

    pub(super) fn provider_route_deps(&self) -> provider_deps::ProviderRouteDeps {
        provider_deps::ProviderRouteDeps::new(provider_deps::ProviderRouteDepsParts {
            data_root: self.state.core.data_root.clone(),
            daemon_url: self.state.core.daemon_url.clone(),
            auth_token: self.state.core.auth_token.clone(),
            workspace_stores: self.protected_workspace_store_lookup(),
            providers: Arc::clone(&self.state.providers),
            plugins: Arc::clone(&self.state.plugins),
            ops_events: self.state.telemetry.ops_events.clone(),
            shutdown_tx: self.state.core.shutdown_tx.clone(),
            harness: Arc::clone(&self.state.execution.harness),
        })
    }

    pub(super) fn protected_workspace_store_lookup(&self) -> ProtectedWorkspaceStoreLookup {
        ProtectedWorkspaceStoreLookup::new(
            self.state.core.stores.clone(),
            Arc::clone(&self.state.sessions),
            Arc::clone(&self.state.transport.merge_queue),
        )
    }

    pub(super) fn task_route_deps(&self) -> task_deps::TaskRouteDeps {
        task_deps::TaskRouteDeps::new(task_deps::TaskRouteDepsParts {
            data_root: self.state.core.data_root.clone(),
            global_store: self.state.global_store().clone(),
            workspace_stores: self.protected_workspace_store_lookup(),
            active_snapshot: Arc::clone(&self.state.workspaces.workspace_active_snapshot),
            sessions: Arc::clone(&self.state.sessions),
            scheduler_worker_host: self.state.session_scheduler_worker_host.worker_host(),
            providers: Arc::clone(&self.state.providers),
            plugins: Arc::clone(&self.state.plugins),
            web_sessions: Arc::clone(&self.state.transport.web_sessions),
            telemetry: self.state.telemetry.telemetry.clone(),
            ops_events: self.state.telemetry.ops_events.clone(),
            perf_telemetry: self.state.telemetry.perf_telemetry.clone(),
        })
    }

    pub(super) fn session_route_deps(
        &self,
        workspace_routes: &workspace_deps::WorkspaceRouteDeps,
    ) -> session_deps::SessionRouteDeps {
        let workspace_stores = self.protected_workspace_store_lookup();
        let session_stores =
            SessionStoreLookup::new(self.state.global_store().clone(), workspace_stores.clone());
        let weak_session_stores = WeakSessionStoreLookup::new(
            self.state.global_store().clone(),
            self.state.core.stores.clone(),
            Arc::downgrade(&self.state.sessions),
            Arc::clone(&self.state.transport.merge_queue),
        );
        session_deps::SessionRouteDeps::new(session_deps::SessionRouteDepsParts {
            data_root: self.state.core.data_root.clone(),
            tool_output_spool_dir: self.state.core.tool_output_spool_dir.clone(),
            daemon_url: self.state.core.daemon_url.clone(),
            auth_token: self.state.core.auth_token.clone(),
            global_store: self.state.global_store().clone(),
            stores: self.state.core.stores.clone(),
            workspace_stores,
            session_stores,
            weak_session_stores,
            sessions: Arc::clone(&self.state.sessions),
            scheduler_worker_host: self.state.session_scheduler_worker_host.worker_host(),
            active_snapshot: Arc::clone(&self.state.workspaces.workspace_active_snapshot),
            worktree_file_completions_cache: Arc::clone(
                &self.state.workspaces.file_completions_cache,
            ),
            providers: Arc::clone(&self.state.providers),
            plugins: Arc::clone(&self.state.plugins),
            ops_events: self.state.telemetry.ops_events.clone(),
            perf_telemetry: self.state.telemetry.perf_telemetry.clone(),
            provider_unknown_events: self.state.telemetry.provider_unknown_events.clone(),
            ask_user_question: Arc::clone(&self.state.core.ask_user_question),
            update_drain: Arc::clone(&self.state.core.update_drain),
            harness: Arc::clone(&self.state.execution.harness),
            task_publication: Arc::clone(&self.state.task_publication),
            task_worktree_host: workspace_routes.task_worktree_host(),
            worktree_vcs_runtime: workspace_routes.worktree_vcs_runtime_host(),
            worktree_vcs_execution: workspace_routes.worktree_vcs_execution_host(),
        })
    }

    pub(super) fn transport_route_deps(&self) -> transport_deps::TransportRouteDeps {
        transport_deps::TransportRouteDeps::new(transport_deps::TransportRouteDepsParts {
            data_root: self.state.core.data_root.clone(),
            daemon_url: self.state.core.daemon_url.clone(),
            auth_token_configured: self.state.core.auth_token.is_some(),
            global_store: self.state.global_store().clone(),
            workspace_stores: self.protected_workspace_store_lookup(),
            mobile_tunnel: self.state.transport.mobile_tunnel.clone(),
            terminals: Arc::clone(&self.state.transport.terminals),
            web_sessions: Arc::clone(&self.state.transport.web_sessions),
            providers: Arc::clone(&self.state.providers),
            harness: Arc::clone(&self.state.execution.harness),
            health: self.health(),
            telemetry: self.state.telemetry.telemetry.clone(),
            ops_events: self.state.telemetry.ops_events.clone(),
        })
    }

    pub(super) fn workspace_route_deps(&self) -> workspace_deps::WorkspaceRouteDeps {
        let workspace_stores = self.protected_workspace_store_lookup();
        let session_stores =
            SessionStoreLookup::new(self.state.global_store().clone(), workspace_stores.clone());
        workspace_deps::WorkspaceRouteDeps::new(workspace_deps::WorkspaceRouteDepsParts {
            data_root: self.state.core.data_root.clone(),
            daemon_url: self.state.core.daemon_url.clone(),
            stores: self.state.core.stores.clone(),
            global_store: self.state.global_store().clone(),
            workspace_stores,
            session_stores,
            sessions: Arc::clone(&self.state.sessions),
            active_snapshot: Arc::clone(&self.state.workspaces.workspace_active_snapshot),
            workspace_active_snapshot_cache: Arc::clone(
                &self.state.workspaces.workspace_active_snapshot_cache,
            ),
            workspace_active_heads_cache: Arc::clone(
                &self.state.workspaces.workspace_active_heads_cache,
            ),
            workspace_file_completions_cache: Arc::clone(
                &self.state.workspaces.workspace_file_completions_cache,
            ),
            worktree_bootstrap_gates: Arc::clone(&self.state.workspaces.worktree_bootstrap_gates),
            attachment_materialization: Arc::clone(
                &self.state.workspaces.attachment_materialization,
            ),
            harness: Arc::clone(&self.state.execution.harness),
            providers: Arc::clone(&self.state.providers),
            merge_queue: Arc::clone(&self.state.transport.merge_queue),
            merge_queue_host: self.merge_queue_route_host(),
            telemetry: self.state.telemetry.telemetry.clone(),
            perf_telemetry: self.state.telemetry.perf_telemetry.clone(),
            worktree_vcs_runtime: WorktreeVcsRuntimeHost::from_workspace_runtime(
                &self.state.workspaces,
            ),
            worktree_vcs_execution: WorktreeVcsExecutionHost::new(
                self.state.core.data_root.clone(),
                self.state.core.daemon_url.clone(),
                self.state.global_store().clone(),
                self.protected_workspace_store_lookup(),
                Arc::clone(&self.state.execution.harness),
            ),
            workspace_vcs_stream_runtime:
                crate::daemon::workspaces::stream::WorkspaceVcsStreamRuntime::from_workspace_runtime(
                    &self.state.workspaces,
                ),
        })
    }
}
