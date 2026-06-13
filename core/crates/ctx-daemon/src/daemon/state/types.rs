use super::*;
use ctx_core::models::WorktreeVcsSnapshot;
use ctx_execution_runtime::ExecutionSetupCoordinator;
use ctx_mcp_auth::McpAuthRegistry;
use ctx_storage_admission::StorageGuardRuntime;
use ctx_update_service::UpdateDrainCoordinator;
use ctx_workspace_active_snapshot::{
    WorkspaceActiveHeadCacheEntry, WorkspaceActiveSnapshotCacheEntry,
};
use ctx_worktree_vcs_service::CachedFileCompletions;
use ctx_worktree_vcs_service::{
    GitStatusSnapshotCacheEntry, WorktreeVcsRuntimeState, WorktreeVcsSchedulerRuntime,
    WorktreeVcsSnapshotCacheEntry,
};

use crate::daemon::provider_capability_hosts::ProviderLifecycleBackgroundHost;
use crate::daemon::sessions::SessionSchedulerWorkerHostFactory;
use crate::daemon::task_session_effects::{
    SessionPublicationEffects, TaskPublicationHost, TaskSessionCleanupHost,
};
use crate::daemon::workspaces::attachments::WorkspaceAttachmentMaterializationRuntime;

pub(crate) type WorkspaceFileCompletionsCache =
    Arc<Mutex<HashMap<WorkspaceId, TimedEntry<CachedFileCompletions>>>>;
pub(crate) type WorktreeFileCompletionsCache =
    Arc<Mutex<HashMap<WorktreeId, TimedEntry<CachedFileCompletions>>>>;
pub(crate) type WorkspaceActiveSnapshotCache =
    Arc<Mutex<HashMap<WorkspaceId, TimedEntry<WorkspaceActiveSnapshotCacheEntry>>>>;
pub(crate) type WorkspaceActiveHeadsCache =
    Arc<Mutex<HashMap<WorkspaceId, TimedEntry<WorkspaceActiveHeadCacheEntry>>>>;

pub struct CoreState {
    pub(crate) data_root: PathBuf,
    pub(crate) storage_guard: Arc<StorageGuardRuntime>,
    pub(crate) tool_output_spool_dir: PathBuf,
    pub(crate) stores: StoreManager,
    pub(crate) daemon_url: String,
    pub(crate) public_base_url: Option<String>,
    pub(crate) auth_token: Option<String>,
    pub(crate) local_shutdown_token: Option<String>,
    pub(crate) mcp_auth: Arc<McpAuthRegistry>,
    pub(crate) ask_user_question: Arc<AskUserQuestionBroker>,
    pub(crate) shutdown_tx: broadcast::Sender<()>,
    pub(crate) update_drain: Arc<UpdateDrainCoordinator>,
}

pub type SessionRuntime = ctx_session_runtime::runtime::SessionRuntime<SchedulerCommand>;

pub struct WorkspaceRuntime {
    pub(crate) worktree_vcs_enabled: bool,
    pub(crate) file_completions_cache: WorktreeFileCompletionsCache,
    pub(crate) workspace_file_completions_cache: WorkspaceFileCompletionsCache,
    pub(crate) git_status_snapshots:
        Mutex<HashMap<WorktreeId, TimedEntry<GitStatusSnapshotCacheEntry>>>,
    pub(crate) worktree_vcs_snapshots:
        Arc<Mutex<HashMap<WorktreeId, TimedEntry<WorktreeVcsSnapshotCacheEntry>>>>,
    pub(crate) worktree_vcs_active: Arc<Mutex<HashMap<WorktreeId, usize>>>,
    pub(crate) worktree_vcs_refresh_locks:
        Arc<Mutex<HashMap<WorktreeId, std::sync::Weak<Mutex<()>>>>>,
    pub(crate) worktree_vcs_open_panes: Arc<Mutex<HashMap<WorktreeId, usize>>>,
    pub(crate) worktree_vcs_summary_gen: Arc<Mutex<HashMap<WorktreeId, u64>>>,
    pub(crate) worktree_vcs_runtime: Arc<Mutex<HashMap<WorktreeId, WorktreeVcsRuntimeState>>>,
    pub(crate) worktree_vcs_scheduler: WorktreeVcsSchedulerRuntime,
    pub(crate) worktree_vcs_events: broadcast::Sender<WorktreeVcsSnapshot>,
    pub(crate) git_status_watchers: Arc<Mutex<HashSet<WorktreeId>>>,
    pub(crate) workspace_active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
    pub(crate) workspace_active_snapshot_cache: WorkspaceActiveSnapshotCache,
    pub(crate) workspace_active_heads_cache: WorkspaceActiveHeadsCache,
    pub(crate) worktree_bootstrap_gates:
        Arc<Mutex<HashMap<WorktreeId, TimedEntry<WorktreeBootstrapGate>>>>,
    pub(crate) attachment_materialization: Arc<WorkspaceAttachmentMaterializationRuntime>,
}

pub type ProviderRuntime = ctx_provider_runtime::ProviderRuntime;

pub struct TelemetryRuntime {
    pub(crate) telemetry: Telemetry,
    pub(crate) ops_events: OpsEvents,
    pub(crate) perf_telemetry: PerfTelemetry,
    pub(crate) provider_unknown_events:
        ctx_observability::provider_unknown_events::ProviderUnknownEvents,
    pub(crate) resource_governance: Arc<Mutex<ResourceGovernanceRuntime>>,
    pub(crate) resource_sampler: Arc<Mutex<ResourceSampler>>,
}

pub struct TransportRuntime {
    pub(crate) terminals: Arc<TerminalManager>,
    pub(crate) mobile_tunnel: MobileTunnelManager,
    pub(crate) web_sessions: Arc<WebSessionManager>,
    pub(crate) merge_queue: Arc<ctx_merge_queue::MergeQueueRuntime>,
}

pub struct ExecutionRuntime {
    pub(crate) harness: Arc<HarnessRuntimeManager>,
    pub(crate) setup: Arc<ExecutionSetupCoordinator>,
}

pub struct DaemonState {
    pub(crate) core: CoreState,
    pub(crate) sessions: Arc<SessionRuntime>,
    pub(crate) workspaces: WorkspaceRuntime,
    pub(crate) providers: Arc<ProviderRuntime>,
    pub(crate) telemetry: TelemetryRuntime,
    pub(crate) transport: TransportRuntime,
    pub(crate) execution: ExecutionRuntime,
    pub(crate) session_publication: SessionPublicationEffects,
    pub(crate) provider_lifecycle_background: Arc<ProviderLifecycleBackgroundHost>,
    pub(crate) task_publication: Arc<TaskPublicationHost>,
    pub(crate) task_session_cleanup: TaskSessionCleanupHost,
    pub(in crate::daemon) session_scheduler_worker_host: SessionSchedulerWorkerHostFactory,
}

pub enum StoreLookup {
    Found(Store),
    Missing,
    Deleting,
    Unavailable(anyhow::Error),
}

pub struct WorktreeBootstrapGate {
    pub wait_for_completion: bool,
    pub done_tx: watch::Sender<bool>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AppRuntimeFlags {
    pub worktree_vcs_enabled: bool,
}
