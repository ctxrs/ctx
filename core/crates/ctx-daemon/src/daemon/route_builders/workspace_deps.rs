use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use ctx_core::ids::WorktreeId;
use ctx_merge_queue::MergeQueueRuntime;
use ctx_observability::perf_telemetry::PerfTelemetry;
use ctx_observability::telemetry::Telemetry;
use ctx_provider_runtime::ProviderRuntime;
use ctx_store::{Store, StoreManager};
use ctx_workspace_active_snapshot::WorkspaceActiveSnapshotHub;
use ctx_workspace_runtime::HarnessRuntimeManager;
use tokio::sync::Mutex;

use crate::daemon::git_status::{WorktreeVcsExecutionHost, WorktreeVcsRuntimeHost};
use crate::daemon::merge_queue::MergeQueueRouteHost;
use crate::daemon::state::{
    ProtectedWorkspaceStoreLookup, SessionRuntime, SessionStoreLookup, TimedEntry,
    WorkspaceActiveHeadsCache, WorkspaceActiveSnapshotCache, WorkspaceFileCompletionsCache,
    WorktreeBootstrapGate,
};
use crate::daemon::workspaces::attachments::{
    WorkspaceAttachmentMaterializationRuntime, WorkspaceAttachmentsRuntime,
};
use crate::daemon::workspaces::stream::WorkspaceVcsStreamRuntime;
use crate::daemon::workspaces::{TaskWorktreeHost, TaskWorktreeHostParts};

pub(super) struct WorkspaceRouteDepsParts {
    pub(super) data_root: PathBuf,
    pub(super) daemon_url: String,
    pub(super) stores: StoreManager,
    pub(super) global_store: Store,
    pub(super) workspace_stores: ProtectedWorkspaceStoreLookup,
    pub(super) session_stores: SessionStoreLookup,
    pub(super) sessions: Arc<SessionRuntime>,
    pub(super) active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
    pub(super) workspace_active_snapshot_cache: WorkspaceActiveSnapshotCache,
    pub(super) workspace_active_heads_cache: WorkspaceActiveHeadsCache,
    pub(super) workspace_file_completions_cache: WorkspaceFileCompletionsCache,
    pub(super) worktree_bootstrap_gates:
        Arc<Mutex<HashMap<WorktreeId, TimedEntry<WorktreeBootstrapGate>>>>,
    pub(super) attachment_materialization: Arc<WorkspaceAttachmentMaterializationRuntime>,
    pub(super) harness: Arc<HarnessRuntimeManager>,
    pub(super) providers: Arc<ProviderRuntime>,
    pub(super) merge_queue: Arc<MergeQueueRuntime>,
    pub(super) merge_queue_host: Arc<MergeQueueRouteHost>,
    pub(super) telemetry: Telemetry,
    pub(super) perf_telemetry: PerfTelemetry,
    pub(super) worktree_vcs_runtime: WorktreeVcsRuntimeHost,
    pub(super) worktree_vcs_execution: WorktreeVcsExecutionHost,
    pub(super) workspace_vcs_stream_runtime: WorkspaceVcsStreamRuntime,
}

#[derive(Clone)]
pub(super) struct WorkspaceRouteDeps {
    pub(super) data_root: PathBuf,
    pub(super) daemon_url: String,
    pub(super) stores: StoreManager,
    pub(super) global_store: Store,
    pub(super) workspace_stores: ProtectedWorkspaceStoreLookup,
    pub(super) session_stores: SessionStoreLookup,
    pub(super) sessions: Arc<SessionRuntime>,
    pub(super) active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
    pub(super) workspace_active_snapshot_cache: WorkspaceActiveSnapshotCache,
    pub(super) workspace_active_heads_cache: WorkspaceActiveHeadsCache,
    pub(super) workspace_file_completions_cache: WorkspaceFileCompletionsCache,
    pub(super) worktree_bootstrap_gates:
        Arc<Mutex<HashMap<WorktreeId, TimedEntry<WorktreeBootstrapGate>>>>,
    pub(super) attachment_materialization: Arc<WorkspaceAttachmentMaterializationRuntime>,
    pub(super) harness: Arc<HarnessRuntimeManager>,
    pub(super) providers: Arc<ProviderRuntime>,
    pub(super) merge_queue: Arc<MergeQueueRuntime>,
    pub(super) merge_queue_host: Arc<MergeQueueRouteHost>,
    pub(super) telemetry: Telemetry,
    pub(super) perf_telemetry: PerfTelemetry,
    pub(super) worktree_vcs_runtime: WorktreeVcsRuntimeHost,
    pub(super) worktree_vcs_execution: WorktreeVcsExecutionHost,
    pub(super) workspace_vcs_stream_runtime: WorkspaceVcsStreamRuntime,
}

impl WorkspaceRouteDeps {
    pub(super) fn new(parts: WorkspaceRouteDepsParts) -> Self {
        Self {
            data_root: parts.data_root,
            daemon_url: parts.daemon_url,
            stores: parts.stores,
            global_store: parts.global_store,
            workspace_stores: parts.workspace_stores,
            session_stores: parts.session_stores,
            sessions: parts.sessions,
            active_snapshot: parts.active_snapshot,
            workspace_active_snapshot_cache: parts.workspace_active_snapshot_cache,
            workspace_active_heads_cache: parts.workspace_active_heads_cache,
            workspace_file_completions_cache: parts.workspace_file_completions_cache,
            worktree_bootstrap_gates: parts.worktree_bootstrap_gates,
            attachment_materialization: parts.attachment_materialization,
            harness: parts.harness,
            providers: parts.providers,
            merge_queue: parts.merge_queue,
            merge_queue_host: parts.merge_queue_host,
            telemetry: parts.telemetry,
            perf_telemetry: parts.perf_telemetry,
            worktree_vcs_runtime: parts.worktree_vcs_runtime,
            worktree_vcs_execution: parts.worktree_vcs_execution,
            workspace_vcs_stream_runtime: parts.workspace_vcs_stream_runtime,
        }
    }

    pub(super) fn workspace_store_lookup(&self) -> ProtectedWorkspaceStoreLookup {
        self.workspace_stores.clone()
    }

    pub(super) fn session_store_lookup(&self) -> SessionStoreLookup {
        self.session_stores.clone()
    }

    pub(super) fn merge_queue_route_host(&self) -> Arc<MergeQueueRouteHost> {
        Arc::clone(&self.merge_queue_host)
    }

    pub(super) fn worktree_vcs_execution_host(&self) -> WorktreeVcsExecutionHost {
        self.worktree_vcs_execution.clone()
    }

    pub(super) fn worktree_vcs_runtime_host(&self) -> WorktreeVcsRuntimeHost {
        self.worktree_vcs_runtime.clone()
    }

    pub(super) fn workspace_attachments_runtime(&self) -> Arc<WorkspaceAttachmentsRuntime> {
        Arc::new(WorkspaceAttachmentsRuntime::new(
            self.data_root.clone(),
            self.daemon_url.clone(),
            self.global_store.clone(),
            self.workspace_store_lookup(),
            Arc::clone(&self.harness),
            Arc::clone(&self.attachment_materialization),
        ))
    }

    pub(super) fn task_worktree_host(&self) -> Arc<TaskWorktreeHost> {
        let vcs_hooks = Arc::new(
            crate::daemon::workspaces::vcs_hooks::WorkspaceVcsHookHost::new(
                self.data_root.clone(),
                self.daemon_url.clone(),
                self.global_store.clone(),
                self.workspace_store_lookup(),
                Arc::clone(&self.harness),
            ),
        );
        Arc::new(TaskWorktreeHost::new(TaskWorktreeHostParts {
            data_root: self.data_root.clone(),
            daemon_url: self.daemon_url.clone(),
            global_store: self.global_store.clone(),
            workspace_stores: self.workspace_store_lookup(),
            harness: Arc::clone(&self.harness),
            active_snapshot: Arc::clone(&self.active_snapshot),
            bootstrap_gates: Arc::clone(&self.worktree_bootstrap_gates),
            attachments: self.workspace_attachments_runtime(),
            vcs_hooks,
        }))
    }
}
