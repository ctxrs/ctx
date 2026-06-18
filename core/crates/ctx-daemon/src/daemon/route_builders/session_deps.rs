use std::path::PathBuf;
use std::sync::Arc;

use ctx_observability::ops_events::OpsEvents;
use ctx_observability::perf_telemetry::PerfTelemetry;
use ctx_observability::provider_unknown_events::ProviderUnknownEvents;
use ctx_provider_runtime::ProviderRuntime;
use ctx_providers::ask_user_question::AskUserQuestionBroker;
use ctx_store::{Store, StoreManager};
use ctx_update_service::UpdateDrainCoordinator;
use ctx_workspace_active_snapshot::WorkspaceActiveSnapshotHub;
use ctx_workspace_runtime::HarnessRuntimeManager;

use crate::daemon::git_status::{WorktreeVcsExecutionHost, WorktreeVcsRuntimeHost};
use crate::daemon::plugins::PluginInventoryRuntime;
use crate::daemon::scheduler::SessionSchedulerWorkerHost;
use crate::daemon::state::{
    ProtectedWorkspaceStoreLookup, SessionRuntime, SessionStoreLookup, WeakSessionStoreLookup,
    WorktreeFileCompletionsCache,
};
use crate::daemon::task_session_effects::{
    SessionPublicationEffects, TaskPublicationHost, TaskSessionCleanupHost,
};
use crate::daemon::workspaces::TaskWorktreeHost;

pub(super) struct SessionRouteDepsParts {
    pub(super) data_root: PathBuf,
    pub(super) tool_output_spool_dir: PathBuf,
    pub(super) daemon_url: String,
    pub(super) auth_token: Option<String>,
    pub(super) global_store: Store,
    pub(super) stores: StoreManager,
    pub(super) workspace_stores: ProtectedWorkspaceStoreLookup,
    pub(super) session_stores: SessionStoreLookup,
    pub(super) weak_session_stores: WeakSessionStoreLookup,
    pub(super) sessions: Arc<SessionRuntime>,
    pub(super) scheduler_worker_host: Arc<SessionSchedulerWorkerHost>,
    pub(super) active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
    pub(super) worktree_file_completions_cache: WorktreeFileCompletionsCache,
    pub(super) providers: Arc<ProviderRuntime>,
    pub(super) plugins: Arc<PluginInventoryRuntime>,
    pub(super) ops_events: OpsEvents,
    pub(super) perf_telemetry: PerfTelemetry,
    pub(super) provider_unknown_events: ProviderUnknownEvents,
    pub(super) ask_user_question: Arc<AskUserQuestionBroker>,
    pub(super) update_drain: Arc<UpdateDrainCoordinator>,
    pub(super) harness: Arc<HarnessRuntimeManager>,
    pub(super) task_publication: Arc<TaskPublicationHost>,
    pub(super) task_worktree_host: Arc<TaskWorktreeHost>,
    pub(super) worktree_vcs_runtime: WorktreeVcsRuntimeHost,
    pub(super) worktree_vcs_execution: WorktreeVcsExecutionHost,
}

#[derive(Clone)]
pub(super) struct SessionRouteDeps {
    pub(super) data_root: PathBuf,
    pub(super) tool_output_spool_dir: PathBuf,
    pub(super) daemon_url: String,
    pub(super) auth_token: Option<String>,
    pub(super) global_store: Store,
    pub(super) stores: StoreManager,
    pub(super) workspace_stores: ProtectedWorkspaceStoreLookup,
    pub(super) session_stores: SessionStoreLookup,
    pub(super) weak_session_stores: WeakSessionStoreLookup,
    pub(super) sessions: Arc<SessionRuntime>,
    pub(super) scheduler_worker_host: Arc<SessionSchedulerWorkerHost>,
    pub(super) active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
    pub(super) worktree_file_completions_cache: WorktreeFileCompletionsCache,
    pub(super) providers: Arc<ProviderRuntime>,
    pub(super) plugins: Arc<PluginInventoryRuntime>,
    pub(super) ops_events: OpsEvents,
    pub(super) perf_telemetry: PerfTelemetry,
    pub(super) provider_unknown_events: ProviderUnknownEvents,
    pub(super) ask_user_question: Arc<AskUserQuestionBroker>,
    pub(super) update_drain: Arc<UpdateDrainCoordinator>,
    pub(super) harness: Arc<HarnessRuntimeManager>,
    pub(super) task_publication: Arc<TaskPublicationHost>,
    pub(super) task_worktree_host: Arc<TaskWorktreeHost>,
    pub(super) worktree_vcs_runtime: WorktreeVcsRuntimeHost,
    pub(super) worktree_vcs_execution: WorktreeVcsExecutionHost,
}

impl SessionRouteDeps {
    pub(super) fn new(parts: SessionRouteDepsParts) -> Self {
        Self {
            data_root: parts.data_root,
            tool_output_spool_dir: parts.tool_output_spool_dir,
            daemon_url: parts.daemon_url,
            auth_token: parts.auth_token,
            global_store: parts.global_store,
            stores: parts.stores,
            workspace_stores: parts.workspace_stores,
            session_stores: parts.session_stores,
            weak_session_stores: parts.weak_session_stores,
            sessions: parts.sessions,
            scheduler_worker_host: parts.scheduler_worker_host,
            active_snapshot: parts.active_snapshot,
            worktree_file_completions_cache: parts.worktree_file_completions_cache,
            providers: parts.providers,
            plugins: parts.plugins,
            ops_events: parts.ops_events,
            perf_telemetry: parts.perf_telemetry,
            provider_unknown_events: parts.provider_unknown_events,
            ask_user_question: parts.ask_user_question,
            update_drain: parts.update_drain,
            harness: parts.harness,
            task_publication: parts.task_publication,
            task_worktree_host: parts.task_worktree_host,
            worktree_vcs_runtime: parts.worktree_vcs_runtime,
            worktree_vcs_execution: parts.worktree_vcs_execution,
        }
    }

    pub(super) fn session_store_lookup(&self) -> SessionStoreLookup {
        self.session_stores.clone()
    }

    pub(super) fn weak_session_store_lookup(&self) -> WeakSessionStoreLookup {
        self.weak_session_stores.clone()
    }

    pub(super) fn workspace_store_lookup(&self) -> ProtectedWorkspaceStoreLookup {
        self.workspace_stores.clone()
    }

    pub(super) fn task_publication_host(&self) -> Arc<TaskPublicationHost> {
        Arc::clone(&self.task_publication)
    }

    pub(super) fn task_session_cleanup_host(&self) -> TaskSessionCleanupHost {
        TaskSessionCleanupHost::new(
            self.global_store.clone(),
            Arc::clone(&self.sessions),
            Arc::clone(&self.providers),
            Arc::clone(&self.active_snapshot),
            self.workspace_store_lookup(),
        )
    }

    pub(super) fn session_publication_effects(&self) -> SessionPublicationEffects {
        SessionPublicationEffects::new(
            Arc::clone(&self.sessions),
            self.session_store_lookup(),
            self.task_publication_host(),
        )
    }

    pub(super) fn task_worktree_host(&self) -> Arc<TaskWorktreeHost> {
        Arc::clone(&self.task_worktree_host)
    }
}
