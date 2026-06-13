use super::super::super::*;
use crate::daemon::workspaces::attachments::WorkspaceAttachmentMaterializationRuntime;
use ctx_worktree_vcs_service::{
    worktree_vcs_scheduler_concurrency_from_env, WorktreeVcsSchedulerRuntime,
};

pub(in crate::daemon::state::builder) fn build_workspace_runtime(
    worktree_vcs_enabled: bool,
    workspace_active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
) -> WorkspaceRuntime {
    WorkspaceRuntime {
        worktree_vcs_enabled,
        file_completions_cache: Arc::new(Mutex::new(HashMap::new())),
        workspace_file_completions_cache: Arc::new(Mutex::new(HashMap::new())),
        git_status_snapshots: Mutex::new(HashMap::new()),
        worktree_vcs_snapshots: Arc::new(Mutex::new(HashMap::new())),
        worktree_vcs_active: Arc::new(Mutex::new(HashMap::new())),
        worktree_vcs_refresh_locks: Arc::new(Mutex::new(HashMap::new())),
        worktree_vcs_open_panes: Arc::new(Mutex::new(HashMap::new())),
        worktree_vcs_summary_gen: Arc::new(Mutex::new(HashMap::new())),
        worktree_vcs_runtime: Arc::new(Mutex::new(HashMap::new())),
        worktree_vcs_scheduler: WorktreeVcsSchedulerRuntime::with_concurrency(
            worktree_vcs_scheduler_concurrency_from_env(),
        ),
        worktree_vcs_events: broadcast::channel(1024).0,
        git_status_watchers: Arc::new(Mutex::new(HashSet::new())),
        workspace_active_snapshot,
        workspace_active_snapshot_cache: Arc::new(Mutex::new(HashMap::new())),
        workspace_active_heads_cache: Arc::new(Mutex::new(HashMap::new())),
        worktree_bootstrap_gates: Arc::new(Mutex::new(HashMap::new())),
        attachment_materialization: Arc::new(WorkspaceAttachmentMaterializationRuntime::new()),
    }
}

pub(in crate::daemon::state::builder) fn build_provider_runtime(
    providers: HashMap<String, Arc<dyn ProviderAdapter>>,
) -> Arc<ProviderRuntime> {
    Arc::new(ProviderRuntime::new(providers))
}

pub(in crate::daemon::state::builder) fn build_telemetry_runtime(
    telemetry: Telemetry,
    ops_events: OpsEvents,
    perf_telemetry: PerfTelemetry,
    provider_unknown_events: ctx_observability::provider_unknown_events::ProviderUnknownEvents,
) -> TelemetryRuntime {
    TelemetryRuntime {
        telemetry,
        ops_events,
        perf_telemetry,
        provider_unknown_events,
        resource_governance: Arc::new(Mutex::new(ResourceGovernanceRuntime::default())),
        resource_sampler: Arc::new(Mutex::new(ResourceSampler::new())),
    }
}

pub(in crate::daemon::state::builder) fn build_transport_runtime(
    terminals: Arc<TerminalManager>,
    web_sessions: Arc<WebSessionManager>,
) -> TransportRuntime {
    TransportRuntime {
        terminals,
        mobile_tunnel: MobileTunnelManager::default(),
        web_sessions,
        merge_queue: Arc::new(ctx_merge_queue::MergeQueueRuntime::new()),
    }
}

pub(in crate::daemon::state::builder) fn build_execution_runtime(
    harness_runtime: Arc<HarnessRuntimeManager>,
    execution_setup: Arc<ExecutionSetupCoordinator>,
) -> ExecutionRuntime {
    ExecutionRuntime {
        harness: harness_runtime,
        setup: execution_setup,
    }
}
