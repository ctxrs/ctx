use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::Duration;

use anyhow::Result;
use ctx_core::ids::{MessageId, RunId, SessionId, TaskId, TurnId, WorkspaceId, WorktreeId};
use ctx_core::models::{
    ExecutionEnvironment, Session, SessionEvent, SessionEventType, SessionHeadDelta,
    SessionSummaryDelta, SessionTurn, SessionTurnToolSummary, TaskDeltaKind, Workspace, Worktree,
};
use ctx_mcp_auth::{McpAuthCapabilities, McpAuthRegistry};
use ctx_observability::ops_events::{OpsEvent, OpsEvents};
use ctx_observability::perf_telemetry::{PerfMetric, PerfMetricKind, PerfTelemetry};
use ctx_observability::provider_unknown_events::{
    provider_unknown_event_hook, ProviderUnknownEventContext, ProviderUnknownEvents,
};
use ctx_observability::telemetry::{Telemetry, TelemetryEvent};
use ctx_provider_runtime::{ProviderRuntime, ProviderRuntimeHost};
use ctx_session_runtime::runtime::{
    SessionEventPublicationHost, SessionLifecycleHost, SessionReplayCursor,
    SessionTaskDeltaRefreshHost,
};
use ctx_session_tools::interrupt_telemetry::metric_labels;
use ctx_session_tools::interrupt_telemetry::InterruptTelemetryContext;
use ctx_session_tools::order_seq::OrderSeqState;
use ctx_storage_admission::{
    storage_emergency_message, StorageGuardLevel, StorageGuardObservedPath, StorageGuardRuntime,
    StorageGuardStatus,
};
use ctx_store::Store;
use ctx_workspace_active_snapshot::WorkspaceActiveSnapshotHub;
use ctx_worktree_data_plane::WorktreeDataPlaneHost;
use serde_json::json;
use tokio::sync::{watch, Mutex};

use super::SchedulerCommand;
use crate::daemon::mcp_auth::{
    issue_provider_session_mcp_token_with_capabilities_parts,
    revoke_provider_session_mcp_token_parts,
};
use crate::daemon::state::{
    session_store_access_anyhow, ProtectedWorkspaceStoreLookup, SessionRuntime, SessionStoreLookup,
    TimedEntry, WorktreeBootstrapGate,
};
use crate::daemon::{provider_capability_hosts, ProviderWorkspaceLaunchRuntime};
use ctx_resource_utilization::ResourceSampler;
use ctx_update_service::UpdateDrainCoordinator;

use super::lifecycle::{
    fail_starting_turn, finalize_start_failure_if_needed, handle_provider_exit,
    handle_provider_stall, stop_running_turn, RunningTurn, StopReason,
};
use super::persistence::{emit_event_with_host, SchedulerPersistenceHost};
use super::{runtime, QueuedMessage};

#[derive(Clone)]
pub(in crate::daemon) struct SessionSchedulerWorkerHost {
    session_stores: SessionStoreLookup,
    session_runtime: Arc<SessionRuntime>,
    ops_events: OpsEvents,
    lifecycle: Arc<WorkerLifecycleHost>,
    turn_runtime: Arc<TurnRuntimeHost>,
}

#[derive(Clone)]
pub(in crate::daemon) struct SessionSchedulerWorkerHostParts {
    pub(in crate::daemon) session_stores: SessionStoreLookup,
    pub(in crate::daemon) session_runtime: Arc<SessionRuntime>,
    pub(in crate::daemon) workspace_stores: ProtectedWorkspaceStoreLookup,
    pub(in crate::daemon) active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
    pub(in crate::daemon) global_store: Store,
    pub(in crate::daemon) providers: Arc<ProviderRuntime>,
    pub(in crate::daemon) provider_launch_runtime: Arc<ProviderWorkspaceLaunchRuntime>,
    pub(in crate::daemon) worktree_bootstrap_gates:
        Arc<Mutex<HashMap<WorktreeId, TimedEntry<WorktreeBootstrapGate>>>>,
    pub(in crate::daemon) storage_guard: Arc<StorageGuardRuntime>,
    pub(in crate::daemon) update_drain: Arc<UpdateDrainCoordinator>,
    pub(in crate::daemon) mcp_auth: Arc<McpAuthRegistry>,
    pub(in crate::daemon) perf_telemetry: PerfTelemetry,
    pub(in crate::daemon) telemetry: Telemetry,
    pub(in crate::daemon) provider_unknown_events: ProviderUnknownEvents,
    pub(in crate::daemon) resource_sampler: Arc<Mutex<ResourceSampler>>,
    pub(in crate::daemon) tool_output_spool_enabled: bool,
    pub(in crate::daemon) tool_output_spool_dir: PathBuf,
    pub(in crate::daemon) ops_events: OpsEvents,
}

impl SessionSchedulerWorkerHost {
    pub(in crate::daemon) fn new(parts: SessionSchedulerWorkerHostParts) -> Self {
        let publication_host = SessionSchedulerWorkerPublicationHost::new(
            parts.session_stores.clone(),
            parts.workspace_stores,
            parts.active_snapshot,
        );
        let lifecycle = Arc::new(WorkerLifecycleHost::new(WorkerLifecycleHostParts {
            global_store: parts.global_store,
            session_stores: parts.session_stores.clone(),
            session_runtime: Arc::clone(&parts.session_runtime),
            publication_host: publication_host.clone(),
            providers: parts.providers,
            mcp_auth: parts.mcp_auth.clone(),
            ops_events: parts.ops_events.clone(),
            perf_telemetry: parts.perf_telemetry.clone(),
        }));
        let event_loop = Arc::new(TurnEventLoopHost::new(TurnEventLoopHostParts {
            session_stores: parts.session_stores.clone(),
            session_runtime: Arc::clone(&parts.session_runtime),
            publication_host: publication_host.clone(),
            active_snapshot: publication_host.active_snapshot.clone(),
            storage_guard: parts.storage_guard.clone(),
            tool_output_spool_enabled: parts.tool_output_spool_enabled,
            tool_output_spool_dir: parts.tool_output_spool_dir,
            ops_events: parts.ops_events.clone(),
            perf_telemetry: parts.perf_telemetry.clone(),
            telemetry: parts.telemetry.clone(),
        }));
        let provider_launch = Arc::new(ProviderTurnLaunchHost::new(ProviderTurnLaunchHostParts {
            launch_runtime: parts.provider_launch_runtime,
            mcp_auth: parts.mcp_auth,
            ops_events: parts.ops_events.clone(),
            perf_telemetry: parts.perf_telemetry.clone(),
            telemetry: parts.telemetry,
            provider_unknown_events: parts.provider_unknown_events,
            event_loop,
        }));
        let turn_runtime = Arc::new(TurnRuntimeHost::new(TurnRuntimeHostParts {
            session_stores: parts.session_stores.clone(),
            session_runtime: Arc::clone(&parts.session_runtime),
            worktree_bootstrap_gates: parts.worktree_bootstrap_gates,
            storage_guard: parts.storage_guard,
            update_drain: parts.update_drain,
            resource_sampler: parts.resource_sampler,
            ops_events: parts.ops_events.clone(),
            perf_telemetry: parts.perf_telemetry.clone(),
            provider_launch,
        }));
        Self {
            session_stores: parts.session_stores,
            session_runtime: parts.session_runtime,
            ops_events: parts.ops_events,
            lifecycle,
            turn_runtime,
        }
    }

    pub(in crate::daemon) async fn existing_session_store(
        &self,
        session_id: SessionId,
    ) -> Result<Store> {
        self.session_stores
            .existing_session_store_for_write(session_id)
            .await
            .map_err(session_store_access_anyhow)
    }

    pub(in crate::daemon) async fn session_order_seq_state(
        &self,
        store: &Store,
        session_id: SessionId,
    ) -> Arc<Mutex<OrderSeqState>> {
        self.session_runtime
            .get_order_seq_state(store, session_id)
            .await
    }

    pub(in crate::daemon) async fn subscribe_session_event_head(
        &self,
        session_id: SessionId,
    ) -> watch::Receiver<i64> {
        self.session_runtime
            .subscribe_session_event_head(session_id)
            .await
    }

    pub(in crate::daemon) async fn provider_inactivity_timeout(&self) -> Duration {
        self.session_runtime.provider_inactivity_timeout().await
    }

    #[cfg(test)]
    pub(in crate::daemon) fn event_loop_host_weak(&self) -> Weak<TurnEventLoopHost> {
        self.turn_runtime
            .provider_launch_host()
            .event_loop_host_weak()
    }

    pub(in crate::daemon) async fn emit_event(
        &self,
        session_id: SessionId,
        run_id: Option<RunId>,
        turn_id: Option<TurnId>,
        event_type: SessionEventType,
        payload_json: serde_json::Value,
    ) -> Result<SessionEvent> {
        emit_event_with_host(self, session_id, run_id, turn_id, event_type, payload_json).await
    }

    pub(in crate::daemon) fn emit_worktree_resolved_event(
        &self,
        session: &Session,
        workdir: &Path,
        session_root_kind: &str,
        worktree: &Worktree,
    ) {
        let mut worktree_event = OpsEvent::new("info", "worktree_resolved");
        worktree_event.session_id = Some(session.id.0.to_string());
        worktree_event.worktree_id = Some(session.worktree_id.0.to_string());
        worktree_event.worktree_root = Some(workdir.to_string_lossy().to_string());
        worktree_event.meta = Some(json!({
            "execution_environment": session.execution_environment.as_str(),
            "session_root_kind": session_root_kind,
            "vcs_kind": worktree.vcs_kind,
            "vcs_ref": worktree.vcs_ref,
            "git_branch": worktree.git_branch,
        }));
        self.ops_events.emit(worktree_event);
    }

    pub(in crate::daemon) async fn set_running(
        &self,
        session_id: SessionId,
        running: bool,
    ) -> bool {
        self.lifecycle.set_running(session_id, running).await;
        true
    }

    pub(in crate::daemon) async fn start_turn(
        &self,
        session: &Session,
        workdir: &Path,
        session_root_kind: &str,
        queued: QueuedMessage,
        order_seq_state: Arc<Mutex<OrderSeqState>>,
    ) -> Option<Result<RunningTurn>> {
        Some(
            runtime::start_turn(
                self.turn_runtime.as_ref(),
                self.lifecycle.as_ref(),
                session,
                workdir,
                session_root_kind,
                queued,
                order_seq_state,
            )
            .await,
        )
    }

    pub(in crate::daemon) async fn handle_provider_exit(
        &self,
        session_id: SessionId,
        turn: RunningTurn,
    ) -> Option<bool> {
        Some(handle_provider_exit(self.lifecycle.as_ref(), session_id, turn).await)
    }

    pub(in crate::daemon) async fn stop_running_turn(
        &self,
        session_id: SessionId,
        turn: RunningTurn,
        reason: StopReason,
        interrupt: Option<InterruptTelemetryContext>,
    ) -> Option<bool> {
        Some(stop_running_turn(self.lifecycle.as_ref(), session_id, turn, reason, interrupt).await)
    }

    pub(in crate::daemon) async fn fail_starting_turn(
        &self,
        session_id: SessionId,
        turn: RunningTurn,
        error_message: &str,
    ) -> bool {
        fail_starting_turn(self.lifecycle.as_ref(), session_id, turn, error_message).await;
        true
    }

    pub(in crate::daemon) async fn handle_provider_stall(
        &self,
        session_id: SessionId,
        turn: RunningTurn,
    ) -> Option<bool> {
        Some(handle_provider_stall(self.lifecycle.as_ref(), session_id, turn).await)
    }

    pub(in crate::daemon) async fn finalize_start_failure_if_needed(
        &self,
        session_id: SessionId,
        run_id: Option<RunId>,
        turn_id: TurnId,
        message_id: MessageId,
        error_message: &str,
    ) -> bool {
        finalize_start_failure_if_needed(
            self.lifecycle.as_ref(),
            session_id,
            run_id,
            turn_id,
            message_id,
            error_message,
        )
        .await;
        true
    }
}

#[async_trait::async_trait]
impl SchedulerPersistenceHost for SessionSchedulerWorkerHost {
    async fn store_for_session(&self, session_id: SessionId) -> Result<Store> {
        self.existing_session_store(session_id).await
    }

    async fn publish_event(&self, event: SessionEvent) {
        self.lifecycle.publish_event(event).await;
    }
}

#[derive(Clone)]
pub(in crate::daemon) struct WorkerLifecycleHost {
    global_store: Store,
    session_stores: SessionStoreLookup,
    session_runtime: Arc<SessionRuntime>,
    publication_host: SessionSchedulerWorkerPublicationHost,
    providers: Arc<ProviderRuntime>,
    mcp_auth: Arc<McpAuthRegistry>,
    ops_events: OpsEvents,
    perf_telemetry: PerfTelemetry,
}

struct WorkerLifecycleHostParts {
    global_store: Store,
    session_stores: SessionStoreLookup,
    session_runtime: Arc<SessionRuntime>,
    publication_host: SessionSchedulerWorkerPublicationHost,
    providers: Arc<ProviderRuntime>,
    mcp_auth: Arc<McpAuthRegistry>,
    ops_events: OpsEvents,
    perf_telemetry: PerfTelemetry,
}

impl WorkerLifecycleHost {
    fn new(parts: WorkerLifecycleHostParts) -> Self {
        Self {
            global_store: parts.global_store,
            session_stores: parts.session_stores,
            session_runtime: parts.session_runtime,
            publication_host: parts.publication_host,
            providers: parts.providers,
            mcp_auth: parts.mcp_auth,
            ops_events: parts.ops_events,
            perf_telemetry: parts.perf_telemetry,
        }
    }

    pub(in crate::daemon) async fn set_running(&self, session_id: SessionId, running: bool) {
        self.session_runtime
            .set_running_with_host(self, session_id, running)
            .await;
    }

    pub(in crate::daemon) async fn publish_event(&self, event: SessionEvent) {
        self.session_runtime
            .publish_event_with_host(&self.publication_host, event)
            .await;
    }

    pub(in crate::daemon) async fn revoke_turn_mcp_token(&self, token: &mut Option<String>) {
        if let Some(token) = token.take() {
            self.revoke_turn_mcp_token_value(&token).await;
        }
    }

    pub(in crate::daemon) async fn revoke_turn_mcp_token_value(&self, token: &str) {
        let _ = revoke_provider_session_mcp_token_parts(
            self.mcp_auth.as_ref(),
            &self.ops_events,
            token,
        )
        .await;
    }

    pub(in crate::daemon) async fn record_interrupt_request_telemetry(
        &self,
        session_id: SessionId,
        turn: &RunningTurn,
        interrupt: &InterruptTelemetryContext,
    ) {
        self.record_interrupt_metric(turn, "request_age", interrupt.elapsed_ms())
            .await;
        tracing::info!(
            session_id = %session_id.0,
            run_id = %turn.run_id.0,
            turn_id = %turn.turn_id.0,
            interrupt_id = %interrupt.interrupt_id(),
            provider_id = %turn.provider_id,
            model_id = %turn.model_id,
            request_age_ms = interrupt.elapsed_ms(),
            "session interrupt requested"
        );
    }

    pub(in crate::daemon) async fn record_provider_cancel_telemetry(
        &self,
        session_id: SessionId,
        turn: &RunningTurn,
        interrupt: &InterruptTelemetryContext,
        cancel_ms: u64,
    ) {
        self.record_interrupt_metric(turn, "provider_cancel", cancel_ms)
            .await;
        tracing::info!(
            session_id = %session_id.0,
            run_id = %turn.run_id.0,
            turn_id = %turn.turn_id.0,
            interrupt_id = %interrupt.interrupt_id(),
            provider_cancel_ms = cancel_ms,
            interrupt_total_ms = interrupt.elapsed_ms(),
            "session interrupt provider cancel finished"
        );
    }

    async fn record_interrupt_metric(&self, turn: &RunningTurn, event: &str, value_ms: u64) {
        let metric = PerfMetric {
            name: "scheduler.interrupt_latency_ms".to_string(),
            kind: PerfMetricKind::Histogram,
            unit: "ms".to_string(),
            value: value_ms as f64,
            labels: metric_labels(
                &turn.provider_id,
                &turn.model_id,
                &turn.execution_environment_label,
                &turn.session_root_kind,
                event,
            ),
        };
        self.perf_telemetry
            .record_metric(metric, Some(turn.run_id.0.to_string()), None, None)
            .await;
    }
}

#[async_trait::async_trait]
impl SchedulerPersistenceHost for WorkerLifecycleHost {
    async fn store_for_session(&self, session_id: SessionId) -> Result<Store> {
        self.session_stores
            .existing_session_store_for_write(session_id)
            .await
            .map_err(session_store_access_anyhow)
    }

    async fn publish_event(&self, event: SessionEvent) {
        WorkerLifecycleHost::publish_event(self, event).await;
    }
}

#[async_trait::async_trait]
impl SessionLifecycleHost for WorkerLifecycleHost {
    async fn set_provider_session_pinned(&self, session_id: SessionId, pinned: bool) {
        self.providers
            .set_provider_session_pinned(session_id.0.to_string(), pinned)
            .await;
    }

    async fn remove_workspace_active_session(&self, session_id: SessionId) {
        let workspace_id = self
            .global_store
            .get_workspace_id_for_session(session_id)
            .await
            .ok()
            .flatten();
        if let Some(workspace_id) = workspace_id {
            self.publication_host
                .active_snapshot
                .remove_session_with_workspace_hint(workspace_id, session_id)
                .await;
        } else {
            self.publication_host
                .active_snapshot
                .remove_session(session_id)
                .await;
        }
    }
}

#[derive(Clone)]
pub(in crate::daemon) struct TurnEventLoopHost {
    session_stores: SessionStoreLookup,
    session_runtime: Arc<SessionRuntime>,
    publication_host: SessionSchedulerWorkerPublicationHost,
    active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
    storage_guard: Arc<StorageGuardRuntime>,
    tool_output_spool_enabled: bool,
    tool_output_spool_dir: PathBuf,
    ops_events: OpsEvents,
    perf_telemetry: PerfTelemetry,
    telemetry: Telemetry,
}

struct TurnEventLoopHostParts {
    session_stores: SessionStoreLookup,
    session_runtime: Arc<SessionRuntime>,
    publication_host: SessionSchedulerWorkerPublicationHost,
    active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
    storage_guard: Arc<StorageGuardRuntime>,
    tool_output_spool_enabled: bool,
    tool_output_spool_dir: PathBuf,
    ops_events: OpsEvents,
    perf_telemetry: PerfTelemetry,
    telemetry: Telemetry,
}

impl TurnEventLoopHost {
    fn new(parts: TurnEventLoopHostParts) -> Self {
        Self {
            session_stores: parts.session_stores,
            session_runtime: parts.session_runtime,
            publication_host: parts.publication_host,
            active_snapshot: parts.active_snapshot,
            storage_guard: parts.storage_guard,
            tool_output_spool_enabled: parts.tool_output_spool_enabled,
            tool_output_spool_dir: parts.tool_output_spool_dir,
            ops_events: parts.ops_events,
            perf_telemetry: parts.perf_telemetry,
            telemetry: parts.telemetry,
        }
    }

    pub(in crate::daemon) async fn publish_event(&self, event: SessionEvent) {
        self.session_runtime
            .publish_event_with_host(&self.publication_host, event)
            .await;
    }

    pub(in crate::daemon) async fn publish_session_gap(
        &self,
        workspace_id: WorkspaceId,
        session_id: SessionId,
        event_seq: i64,
        reason: Option<String>,
    ) {
        self.active_snapshot
            .publish_session_gap(workspace_id, session_id, event_seq, reason)
            .await;
    }

    pub(in crate::daemon) async fn record_perf_metric(
        &self,
        metric: PerfMetric,
        run_id: Option<String>,
    ) {
        self.perf_telemetry
            .record_metric(metric, run_id, None, None)
            .await;
    }

    pub(in crate::daemon) async fn emit_telemetry(&self, event: TelemetryEvent) {
        self.telemetry.emit(event).await;
    }

    pub(in crate::daemon) fn emit_ops_event(&self, event: OpsEvent) {
        self.ops_events.emit(event);
    }

    pub(in crate::daemon) fn storage_guard_snapshot(&self) -> StorageGuardStatus {
        self.storage_guard.snapshot()
    }

    pub(in crate::daemon) fn tool_output_spool_enabled(&self) -> bool {
        self.tool_output_spool_enabled
    }

    pub(in crate::daemon) fn tool_output_spool_dir(&self) -> &Path {
        &self.tool_output_spool_dir
    }

    pub(in crate::daemon) async fn emit_compat_payload_reject_counter(
        &self,
        surface: &str,
        issue: &str,
        extra_label: Option<(&str, &str)>,
    ) {
        let mut labels = HashMap::new();
        labels.insert("source".to_string(), "daemon".to_string());
        labels.insert("surface".to_string(), surface.to_string());
        labels.insert("issue".to_string(), issue.to_string());
        if let Some((key, value)) = extra_label {
            labels.insert(key.to_string(), value.to_string());
        }
        self.record_perf_metric(
            PerfMetric {
                name: "compat.payload_reject_count".to_string(),
                kind: PerfMetricKind::Counter,
                unit: "count".to_string(),
                value: 1.0,
                labels,
            },
            None,
        )
        .await;
    }
}

#[async_trait::async_trait]
impl SchedulerPersistenceHost for TurnEventLoopHost {
    async fn store_for_session(&self, session_id: SessionId) -> Result<Store> {
        self.session_stores
            .existing_session_store_for_write(session_id)
            .await
            .map_err(session_store_access_anyhow)
    }

    async fn publish_event(&self, event: SessionEvent) {
        TurnEventLoopHost::publish_event(self, event).await;
    }
}

#[derive(Clone)]
pub(in crate::daemon) struct TurnRuntimeHost {
    session_stores: SessionStoreLookup,
    session_runtime: Arc<SessionRuntime>,
    worktree_bootstrap_gates: Arc<Mutex<HashMap<WorktreeId, TimedEntry<WorktreeBootstrapGate>>>>,
    storage_guard: Arc<StorageGuardRuntime>,
    update_drain: Arc<UpdateDrainCoordinator>,
    resource_sampler: Arc<Mutex<ResourceSampler>>,
    ops_events: OpsEvents,
    perf_telemetry: PerfTelemetry,
    provider_launch: Arc<ProviderTurnLaunchHost>,
}

struct TurnRuntimeHostParts {
    session_stores: SessionStoreLookup,
    session_runtime: Arc<SessionRuntime>,
    worktree_bootstrap_gates: Arc<Mutex<HashMap<WorktreeId, TimedEntry<WorktreeBootstrapGate>>>>,
    storage_guard: Arc<StorageGuardRuntime>,
    update_drain: Arc<UpdateDrainCoordinator>,
    resource_sampler: Arc<Mutex<ResourceSampler>>,
    ops_events: OpsEvents,
    perf_telemetry: PerfTelemetry,
    provider_launch: Arc<ProviderTurnLaunchHost>,
}

pub(in crate::daemon) struct ProviderRunStartedOpsEvent<'a> {
    pub(in crate::daemon) session: &'a Session,
    pub(in crate::daemon) run_id: RunId,
    pub(in crate::daemon) turn_id: TurnId,
    pub(in crate::daemon) workdir_str: &'a str,
    pub(in crate::daemon) full_model_id: &'a str,
    pub(in crate::daemon) execution_environment: &'a str,
    pub(in crate::daemon) session_root_kind: &'a str,
}

impl TurnRuntimeHost {
    fn new(parts: TurnRuntimeHostParts) -> Self {
        Self {
            session_stores: parts.session_stores,
            session_runtime: parts.session_runtime,
            worktree_bootstrap_gates: parts.worktree_bootstrap_gates,
            storage_guard: parts.storage_guard,
            update_drain: parts.update_drain,
            resource_sampler: parts.resource_sampler,
            ops_events: parts.ops_events,
            perf_telemetry: parts.perf_telemetry,
            provider_launch: parts.provider_launch,
        }
    }

    pub(in crate::daemon) fn provider_launch_host(&self) -> &ProviderTurnLaunchHost {
        self.provider_launch.as_ref()
    }

    pub(in crate::daemon) async fn store_for_session(
        &self,
        session_id: SessionId,
    ) -> Result<Store> {
        self.session_stores
            .existing_session_store_for_write(session_id)
            .await
            .map_err(session_store_access_anyhow)
    }

    pub(in crate::daemon) async fn wait_for_worktree_bootstrap(&self, worktree_id: WorktreeId) {
        let mut done_rx = {
            let mut map = self.worktree_bootstrap_gates.lock().await;
            let Some(gate) = map.get_mut(&worktree_id) else {
                return;
            };
            gate.touch();
            if !gate.value.wait_for_completion {
                return;
            }
            gate.value.done_tx.subscribe()
        };
        if *done_rx.borrow() {
            return;
        }
        let _ = done_rx.changed().await;
    }

    pub(in crate::daemon) async fn reject_if_update_draining(&self) -> Result<()> {
        self.update_drain.reject_if_draining().await?;
        Ok(())
    }

    pub(in crate::daemon) async fn preflight_turn_start(&self, workdir: &Path) -> Result<()> {
        let current = self.storage_guard.snapshot();
        if current.is_emergency() {
            anyhow::bail!(storage_emergency_message(current.active.as_ref()));
        }

        let mut observed_paths = Vec::new();
        let mut seen = HashSet::new();
        push_observed_path(
            &mut observed_paths,
            &mut seen,
            "CTX data root",
            self.provider_launch.data_root().to_path_buf(),
        );
        push_observed_path(
            &mut observed_paths,
            &mut seen,
            "temp storage",
            std::env::temp_dir(),
        );
        for running_workdir in self.running_session_workdirs().await {
            push_observed_path(
                &mut observed_paths,
                &mut seen,
                "active worktree",
                running_workdir,
            );
        }
        push_observed_path(
            &mut observed_paths,
            &mut seen,
            "active worktree",
            workdir.to_path_buf(),
        );

        let disks = {
            let mut sampler = self.resource_sampler.lock().await;
            let (_system, disks, _cache_age_ms) = sampler.system_snapshot();
            disks
        };
        let (previous, snapshot) = self.storage_guard.sample_preflight(
            self.provider_launch.data_root(),
            &observed_paths,
            &disks,
        );
        self.publish_storage_guard_snapshot(&previous, &snapshot)
            .await;
        if snapshot.is_emergency() {
            anyhow::bail!(storage_emergency_message(snapshot.active.as_ref()));
        }
        Ok(())
    }

    pub(in crate::daemon) async fn record_queue_wait_metric(
        &self,
        session: &Session,
        full_model_id: &str,
        execution_environment: &str,
        session_root_kind: &str,
        perf_run_id: Option<String>,
        queue_wait_ms: u64,
    ) {
        let mut queue_labels = HashMap::new();
        queue_labels.insert("provider_id".to_string(), session.provider_id.clone());
        queue_labels.insert("model_id".to_string(), full_model_id.to_string());
        queue_labels.insert(
            "execution_environment".to_string(),
            execution_environment.to_string(),
        );
        queue_labels.insert(
            "session_root_kind".to_string(),
            session_root_kind.to_string(),
        );
        queue_labels.insert("event".to_string(), "queue_wait".to_string());
        let queue_metric = PerfMetric {
            name: "scheduler.queue_wait_ms".to_string(),
            kind: PerfMetricKind::Histogram,
            unit: "ms".to_string(),
            value: queue_wait_ms as f64,
            labels: queue_labels,
        };
        self.perf_telemetry
            .record_metric(queue_metric, perf_run_id, None, None)
            .await;
    }

    pub(in crate::daemon) fn emit_provider_run_started_event(
        &self,
        event: ProviderRunStartedOpsEvent<'_>,
    ) {
        let ProviderRunStartedOpsEvent {
            session,
            run_id,
            turn_id,
            workdir_str,
            full_model_id,
            execution_environment,
            session_root_kind,
        } = event;
        let mut run_event = OpsEvent::new("info", "provider_run_started");
        run_event.session_id = Some(session.id.0.to_string());
        run_event.worktree_id = Some(session.worktree_id.0.to_string());
        run_event.run_id = Some(run_id.0.to_string());
        run_event.turn_id = Some(turn_id.0.to_string());
        run_event.provider_id = Some(session.provider_id.clone());
        run_event.cwd = Some(workdir_str.to_string());
        run_event.worktree_root = Some(workdir_str.to_string());
        run_event.meta = Some(json!({
            "model_id": full_model_id,
            "reasoning_effort": session.reasoning_effort.clone(),
            "execution_environment": execution_environment,
            "session_root_kind": session_root_kind,
        }));
        self.ops_events.emit(run_event);
    }

    async fn running_session_workdirs(&self) -> Vec<std::path::PathBuf> {
        let mut workdirs = Vec::new();
        for session_id in self.session_runtime.list_running_sessions().await {
            let Ok(store) = self.session_stores.existing_session_store(session_id).await else {
                continue;
            };
            let Ok(Some(session)) = store.get_session(session_id).await else {
                continue;
            };
            let Ok(Some(worktree)) = store.get_worktree(session.worktree_id).await else {
                continue;
            };
            workdirs.push(std::path::PathBuf::from(worktree.root_path));
        }
        workdirs
    }

    async fn publish_storage_guard_snapshot(
        &self,
        previous: &StorageGuardStatus,
        snapshot: &StorageGuardStatus,
    ) {
        let should_interrupt = previous.level != StorageGuardLevel::Emergency
            && snapshot.level == StorageGuardLevel::Emergency;
        if !snapshot.same_meaningful_state(previous) {
            let mut event = OpsEvent::new(
                match snapshot.level {
                    StorageGuardLevel::Emergency => "error",
                    StorageGuardLevel::Warning => "warning",
                    StorageGuardLevel::Normal => "info",
                },
                "storage_guard_state_changed",
            );
            event.meta = Some(json!({
                "level": snapshot.level,
                "reserve_file_active": snapshot.reserve_file_active,
                "active": snapshot.active,
            }));
            self.ops_events.emit(event);
        }
        self.storage_guard.publish(snapshot.clone());
        if should_interrupt {
            self.dispatch_storage_emergency_interrupts(snapshot).await;
        }
    }

    async fn dispatch_storage_emergency_interrupts(&self, snapshot: &StorageGuardStatus) {
        let running_sessions = self.session_runtime.list_running_sessions().await;
        let mut interrupted = 0usize;
        for session_id in running_sessions {
            if let Some(tx) = self.session_runtime.scheduler_sender(session_id).await {
                if tx.send(SchedulerCommand::StorageEmergency).await.is_ok() {
                    interrupted += 1;
                }
            }
        }
        tracing::warn!(
            interrupted_sessions = interrupted,
            level = ?snapshot.level,
            active_path = snapshot.active.as_ref().map(|path| path.path.as_str()),
            "storage emergency interrupted active sessions"
        );
    }
}

fn push_observed_path(
    paths: &mut Vec<StorageGuardObservedPath>,
    seen: &mut HashSet<std::path::PathBuf>,
    label: &'static str,
    path: std::path::PathBuf,
) {
    if !seen.insert(path.clone()) {
        return;
    }
    paths.push(StorageGuardObservedPath::new(label, path));
}

#[derive(Clone)]
pub(in crate::daemon) struct ProviderTurnLaunchHost {
    launch_runtime: Arc<ProviderWorkspaceLaunchRuntime>,
    mcp_auth: Arc<McpAuthRegistry>,
    ops_events: OpsEvents,
    perf_telemetry: PerfTelemetry,
    telemetry: Telemetry,
    provider_unknown_events: ProviderUnknownEvents,
    event_loop: Arc<TurnEventLoopHost>,
}

struct ProviderTurnLaunchHostParts {
    launch_runtime: Arc<ProviderWorkspaceLaunchRuntime>,
    mcp_auth: Arc<McpAuthRegistry>,
    ops_events: OpsEvents,
    perf_telemetry: PerfTelemetry,
    telemetry: Telemetry,
    provider_unknown_events: ProviderUnknownEvents,
    event_loop: Arc<TurnEventLoopHost>,
}

pub(in crate::daemon) struct ProviderRunFailedOpsEvent<'a> {
    pub(in crate::daemon) session: &'a Session,
    pub(in crate::daemon) run_id: RunId,
    pub(in crate::daemon) turn_id: TurnId,
    pub(in crate::daemon) workdir_str: &'a str,
    pub(in crate::daemon) full_model_id: &'a str,
    pub(in crate::daemon) execution_environment: ExecutionEnvironment,
    pub(in crate::daemon) session_root_kind: &'a str,
    pub(in crate::daemon) err: &'a anyhow::Error,
}

impl ProviderTurnLaunchHost {
    fn new(parts: ProviderTurnLaunchHostParts) -> Self {
        Self {
            launch_runtime: parts.launch_runtime,
            mcp_auth: parts.mcp_auth,
            ops_events: parts.ops_events,
            perf_telemetry: parts.perf_telemetry,
            telemetry: parts.telemetry,
            provider_unknown_events: parts.provider_unknown_events,
            event_loop: parts.event_loop,
        }
    }

    pub(in crate::daemon) fn data_root(&self) -> &Path {
        self.launch_runtime.data_root()
    }

    pub(in crate::daemon) fn daemon_url(&self) -> &str {
        self.launch_runtime.daemon_url()
    }

    pub(in crate::daemon) fn global_store(&self) -> &Store {
        self.launch_runtime.global_store()
    }

    pub(in crate::daemon) async fn store_for_workspace(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Store> {
        self.launch_runtime.store_for_workspace(workspace_id).await
    }

    pub(in crate::daemon) async fn load_workspace(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Option<Workspace>> {
        self.launch_runtime.load_workspace(workspace_id).await
    }

    pub(in crate::daemon) async fn prepare_harness_runtime(
        &self,
        workspace: &Workspace,
        worktree: &Worktree,
        execution_settings: &ctx_settings_model::ExecutionSettings,
    ) -> Result<ctx_harness_runtime::HarnessExecutionPlan> {
        self.launch_runtime
            .harness()
            .prepare(workspace, worktree, execution_settings, self.daemon_url())
            .await
    }

    pub(in crate::daemon) async fn issue_turn_mcp_token(
        &self,
        session: &Session,
        provider_env: &mut HashMap<String, String>,
    ) -> Option<String> {
        let capabilities = McpAuthCapabilities::provider_turn_default();
        let token = issue_provider_session_mcp_token_with_capabilities_parts(
            self.mcp_auth.as_ref(),
            &self.ops_events,
            session.id,
            session.workspace_id,
            session.worktree_id,
            capabilities,
        )
        .await;
        provider_env.insert("CTX_MCP_TOKEN".to_string(), token.clone());
        Some(token)
    }

    pub(in crate::daemon) fn provider_unknown_event_hook(
        &self,
        session: &Session,
        execution_environment: ExecutionEnvironment,
        session_root_kind: &str,
    ) -> ctx_providers::adapters::ProviderUnknownEventHook {
        provider_unknown_event_hook(
            self.provider_unknown_events.clone(),
            ProviderUnknownEventContext {
                provider_id: session.provider_id.clone(),
                execution_environment: Some(execution_environment.as_str().to_string()),
                session_root_kind: Some(session_root_kind.to_string()),
                operation: "turn".to_string(),
            },
        )
    }

    pub(in crate::daemon) async fn record_provider_spawn_metric(
        &self,
        perf_run_id: Option<String>,
        session: &Session,
        full_model_id: &str,
        execution_environment: ExecutionEnvironment,
        session_root_kind: &str,
        spawn_started_at: std::time::Instant,
    ) {
        let spawn_ms = spawn_started_at.elapsed().as_millis() as u64;
        let mut spawn_labels = HashMap::new();
        spawn_labels.insert("provider_id".to_string(), session.provider_id.clone());
        spawn_labels.insert("model_id".to_string(), full_model_id.to_string());
        spawn_labels.insert(
            "execution_environment".to_string(),
            execution_environment.as_str().to_string(),
        );
        spawn_labels.insert(
            "session_root_kind".to_string(),
            session_root_kind.to_string(),
        );
        spawn_labels.insert("event".to_string(), "spawn".to_string());
        let spawn_metric = PerfMetric {
            name: "provider.spawn_ms".to_string(),
            kind: PerfMetricKind::Histogram,
            unit: "ms".to_string(),
            value: spawn_ms as f64,
            labels: spawn_labels,
        };
        self.perf_telemetry
            .record_metric(spawn_metric, perf_run_id, None, None)
            .await;
    }

    pub(in crate::daemon) async fn emit_provider_call_telemetry(
        &self,
        session: &Session,
        full_model_id: &str,
        execution_environment: ExecutionEnvironment,
        session_root_kind: &str,
        ok: bool,
        duration_ms: u64,
    ) {
        self.telemetry
            .emit(TelemetryEvent::provider_call(
                session.provider_id.clone(),
                full_model_id.to_string(),
                Some(execution_environment.as_str().to_string()),
                Some(session_root_kind.to_string()),
                ok,
                duration_ms,
            ))
            .await;
    }

    pub(in crate::daemon) fn emit_provider_run_failed_event(
        &self,
        event: ProviderRunFailedOpsEvent<'_>,
    ) {
        let ProviderRunFailedOpsEvent {
            session,
            run_id,
            turn_id,
            workdir_str,
            full_model_id,
            execution_environment,
            session_root_kind,
            err,
        } = event;
        let mut fail_event = OpsEvent::new("error", "provider_run_failed");
        fail_event.session_id = Some(session.id.0.to_string());
        fail_event.worktree_id = Some(session.worktree_id.0.to_string());
        fail_event.run_id = Some(run_id.0.to_string());
        fail_event.turn_id = Some(turn_id.0.to_string());
        fail_event.provider_id = Some(session.provider_id.clone());
        fail_event.cwd = Some(workdir_str.to_string());
        fail_event.worktree_root = Some(workdir_str.to_string());
        fail_event.meta = Some(json!({
            "model_id": full_model_id,
            "reasoning_effort": session.reasoning_effort.clone(),
            "execution_environment": execution_environment.as_str(),
            "session_root_kind": session_root_kind,
            "error": err.to_string(),
        }));
        self.ops_events.emit(fail_event);
    }

    pub(in crate::daemon::scheduler) fn event_loop_host_weak(&self) -> Weak<TurnEventLoopHost> {
        Arc::downgrade(&self.event_loop)
    }

    #[allow(clippy::too_many_arguments)]
    pub(in crate::daemon) fn emit_provider_run_env_ready_event(
        &self,
        session: &Session,
        run_id: RunId,
        turn_id: TurnId,
        workdir_str: &str,
        full_model_id: &str,
        execution_environment: &str,
        session_root_kind: &str,
        runtime_provider_id: &str,
        using_endpoint_source: bool,
        is_linux_sandbox: bool,
        runtime_plan: &ctx_harness_runtime::HarnessExecutionPlan,
        provider_env: &HashMap<String, String>,
    ) {
        let mut run_env_event = OpsEvent::new("info", "provider_run_env_ready");
        run_env_event.session_id = Some(session.id.0.to_string());
        run_env_event.worktree_id = Some(session.worktree_id.0.to_string());
        run_env_event.run_id = Some(run_id.0.to_string());
        run_env_event.turn_id = Some(turn_id.0.to_string());
        run_env_event.provider_id = Some(session.provider_id.clone());
        run_env_event.cwd = Some(workdir_str.to_string());
        run_env_event.worktree_root = Some(workdir_str.to_string());
        run_env_event.meta = Some(json!({
            "model_id": full_model_id,
            "reasoning_effort": session.reasoning_effort.clone(),
            "execution_environment": execution_environment,
            "session_root_kind": session_root_kind,
            "runtime_provider_id": runtime_provider_id,
            "source_kind": if using_endpoint_source { "endpoint" } else { "subscription" },
            "is_container": is_linux_sandbox,
            "runtime_kind": runtime_plan
                .env_overrides
                .get(ctx_harness_runtime::CTX_HARNESS_RUNTIME_KIND_ENV)
                .cloned()
                .unwrap_or_else(|| "host".to_string()),
            "has_openai_api_key": provider_env
                .get("OPENAI_API_KEY")
                .is_some_and(|value| !value.trim().is_empty()),
            "has_codex_home": provider_env
                .get("CODEX_HOME")
                .is_some_and(|value| !value.trim().is_empty()),
            "openai_base_url_host": provider_env
                .get("OPENAI_BASE_URL")
                .and_then(|value| url::Url::parse(value).ok())
                .and_then(|parsed| parsed.host_str().map(|host| host.to_string())),
        }));
        self.ops_events.emit(run_env_event);
    }
}

impl ProviderRuntimeHost for ProviderTurnLaunchHost {
    fn data_root(&self) -> &Path {
        self.data_root()
    }

    fn current_ctx_version(&self) -> Option<String> {
        provider_capability_hosts::current_ctx_version_for_provider_runtime()
    }

    fn provider_runtime(&self) -> &ctx_provider_runtime::ProviderRuntime {
        self.launch_runtime.providers()
    }

    fn publish_provider_install_ops_events(
        &self,
        events: Vec<ctx_provider_runtime::provider_install_tracker::ProviderInstallOpsEvent>,
    ) {
        provider_capability_hosts::emit_provider_install_ops_events(&self.ops_events, events);
    }
}

#[async_trait::async_trait]
impl WorktreeDataPlaneHost for ProviderTurnLaunchHost {
    async fn get_workspace(state: &Self, workspace_id: WorkspaceId) -> Result<Option<Workspace>> {
        state.load_workspace(workspace_id).await
    }

    async fn workspace_store(state: &Self, workspace_id: WorkspaceId) -> Result<Store> {
        state.store_for_workspace(workspace_id).await
    }
}

#[derive(Clone)]
struct SessionSchedulerWorkerPublicationHost {
    session_stores: SessionStoreLookup,
    active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
    task_delta_refresh_host: Arc<SessionSchedulerWorkerTaskDeltaRefreshHost>,
}

impl SessionSchedulerWorkerPublicationHost {
    fn new(
        session_stores: SessionStoreLookup,
        workspace_stores: ProtectedWorkspaceStoreLookup,
        active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
    ) -> Self {
        let task_delta_refresh_host = Arc::new(SessionSchedulerWorkerTaskDeltaRefreshHost {
            workspace_stores,
            active_snapshot: Arc::clone(&active_snapshot),
        });
        Self {
            session_stores,
            active_snapshot,
            task_delta_refresh_host,
        }
    }

    async fn store_for_session(&self, session_id: SessionId) -> Result<Store> {
        self.session_stores
            .existing_session_store(session_id)
            .await
            .map_err(session_store_access_anyhow)
    }
}

#[async_trait::async_trait]
impl SessionEventPublicationHost for SessionSchedulerWorkerPublicationHost {
    type TaskDeltaRefreshHost = SessionSchedulerWorkerTaskDeltaRefreshHost;

    fn task_delta_refresh_host(&self) -> Arc<Self::TaskDeltaRefreshHost> {
        Arc::clone(&self.task_delta_refresh_host)
    }

    async fn load_session(&self, session_id: SessionId) -> Option<Session> {
        let store = self.store_for_session(session_id).await.ok()?;
        store.get_session(session_id).await.ok().flatten()
    }

    async fn list_turn_tool_summaries_for_turn(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Vec<SessionTurnToolSummary> {
        let Ok(store) = self.store_for_session(session_id).await else {
            return Vec::new();
        };
        store
            .list_turn_tool_summaries_for_turns(session_id, std::slice::from_ref(&turn_id))
            .await
            .unwrap_or_default()
    }

    async fn cached_turn_for_read(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
    ) -> Option<SessionTurn> {
        self.active_snapshot
            .get_cached_session_head_for_read(session_id)
            .await
            .and_then(|head| head.turns.into_iter().find(|turn| turn.turn_id == turn_id))
    }

    async fn load_turn(&self, session_id: SessionId, turn_id: TurnId) -> Option<SessionTurn> {
        let store = self.store_for_session(session_id).await.ok()?;
        store
            .get_session_turn(session_id, turn_id)
            .await
            .ok()
            .flatten()
    }

    async fn session_replay_cursor(
        &self,
        workspace_id: WorkspaceId,
        session_id: SessionId,
    ) -> SessionReplayCursor {
        let cursor = self
            .active_snapshot
            .session_replay_cursor(workspace_id, session_id)
            .await;
        SessionReplayCursor {
            last_event_seq: cursor.last_event_seq,
            projection_rev: cursor.projection_rev,
        }
    }

    async fn load_projection_rev(&self, session_id: SessionId) -> Option<i64> {
        let store = self.store_for_session(session_id).await.ok()?;
        store.get_session_projection_rev(session_id).await.ok()
    }

    async fn publish_session_head_delta(
        &self,
        workspace_id: WorkspaceId,
        session: &Session,
        delta: SessionHeadDelta,
        durable: bool,
    ) {
        self.active_snapshot
            .publish_session_head_delta(workspace_id, session, delta, durable)
            .await;
    }

    async fn publish_session_summary_delta(
        &self,
        workspace_id: WorkspaceId,
        delta: SessionSummaryDelta,
    ) {
        self.active_snapshot
            .publish_session_summary_delta(workspace_id, delta)
            .await;
    }
}

struct SessionSchedulerWorkerTaskDeltaRefreshHost {
    workspace_stores: ProtectedWorkspaceStoreLookup,
    active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
}

#[async_trait::async_trait]
impl SessionTaskDeltaRefreshHost for SessionSchedulerWorkerTaskDeltaRefreshHost {
    async fn emit_task_delta_refresh(&self, task_id: TaskId) {
        let store = match self.workspace_stores.store_for_task(task_id).await {
            Ok(store) => store,
            Err(err) => {
                tracing::warn!(
                    task_id = %task_id.0,
                    "scheduler worker task delta refresh store lookup failed: {err:?}"
                );
                return;
            }
        };
        match store.get_workspace_active_task_summary(task_id).await {
            Ok(Some(summary)) => {
                let _ = self
                    .active_snapshot
                    .publish_task_delta(
                        summary.task.workspace_id,
                        summary.task,
                        TaskDeltaKind::Updated,
                    )
                    .await;
            }
            Ok(None) => match store.get_task(task_id).await {
                Ok(Some(task)) => {
                    let kind = if task.archived_at.is_some() {
                        TaskDeltaKind::Archived
                    } else {
                        TaskDeltaKind::Updated
                    };
                    let _ = self
                        .active_snapshot
                        .publish_task_delta(task.workspace_id, task, kind)
                        .await;
                }
                Ok(None) => {}
                Err(err) => {
                    tracing::warn!(
                        task_id = %task_id.0,
                        "scheduler worker task delta refresh task load failed: {err:?}"
                    );
                }
            },
            Err(err) => {
                tracing::warn!(
                    task_id = %task_id.0,
                    "scheduler worker task delta refresh summary load failed: {err:?}"
                );
            }
        }
    }
}
