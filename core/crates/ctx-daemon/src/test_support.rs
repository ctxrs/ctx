use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ctx_core::ids::{RunId, SessionId, TaskId, TurnId, WorkspaceId, WorktreeId};
use ctx_core::models::{
    ExecutionEnvironment, Message, SandboxBinding, SandboxSubstrate, Session, SessionEvent,
    SessionTurn, SessionTurnStatus, Task, Workspace, Worktree, WorktreeVcsSnapshot,
};
use ctx_providers::adapters::ProviderAdapter;
use ctx_store::{Store, StoreManager};
pub use ctx_worktree_vcs_service::{
    branch_exists, managed_worktree_path, standaloneize_worktree_git_dir,
};

use crate::daemon::{
    route_handles_from_state, AppRuntimeFlags, DaemonHandle, DaemonRouteHandles,
    DaemonShutdownSignal, DaemonState,
};

mod cache_rehydration;
pub use cache_rehydration::{
    CacheRehydrationSessionFixture, CacheRehydrationSubagentFixture, CacheRehydrationTurnFixture,
};
mod ctx_ui_sized_head;
use ctx_ui_sized_head::{
    fixed_test_utc, latest_ctx_ui_sized_turn_id, seed_ctx_ui_sized_session,
    tail_ctx_ui_sized_turn_ids,
};
pub use ctx_ui_sized_head::{
    CtxUiSizedHeadSeedSpec, CtxUiSizedHeadSeedStats, CtxUiSizedToolSummaryProbe,
};
mod merge_queue;
mod mobile;
pub mod provider_scenarios;
mod providers;
pub mod replay_projection;
mod route_handles;
mod runtime_environment;
mod sandbox_cli;
#[cfg(unix)]
pub use sandbox_cli::write_running_container_sandbox_cli_shim;
pub use sandbox_cli::{avf_linux_runtime_manager_test_sandbox_cli_path, sandbox_cli_env_test_lock};
mod session_artifacts;
mod session_events;
mod session_heads;
mod session_lifecycle;
pub mod subagent_mcp;
mod tasks;
mod workspace_active;

#[derive(Clone)]
pub struct TestDaemon {
    state: Arc<DaemonState>,
}

pub struct TestMobileAccessForTest<'a> {
    state: &'a Arc<DaemonState>,
}

pub struct SessionModelSwitchFixture {
    pub workspace: Workspace,
    pub task: Task,
    pub session: Session,
}

pub struct TaskDefaultSessionSnapshot {
    pub task: Option<Task>,
    pub sessions: Vec<Session>,
    pub task_count: usize,
    pub worktree_count: usize,
}

pub struct TaskArchiveManagedWorktreesSnapshot {
    pub session_count: usize,
    pub worktree_count: usize,
    pub managed_worktree_count: usize,
    pub managed_roots: Vec<PathBuf>,
    pub managed_branches: Vec<String>,
}

pub struct WorkspaceAttachmentsDemoFixture {
    pub workspace: Workspace,
    pub worktree: Worktree,
    pub task: Task,
}

pub enum HotEndpointManualHeadProbe {
    UnexpectedlySucceeded,
    FailedClosed,
}

pub struct TaskSessionCreationLockGuardForTest {
    _lock: Arc<tokio::sync::Mutex<()>>,
    _guard: tokio::sync::OwnedMutexGuard<()>,
}

pub struct ShutdownRunningTurnFixture {
    pub workspace_id: WorkspaceId,
    pub session_id: SessionId,
    pub turn_id: TurnId,
}

pub struct RunArchiveRouteFixture {
    pub workspace_id: WorkspaceId,
    pub run_id: RunId,
}

pub struct AssistantChunkStreamSnapshot {
    pub events: Vec<SessionEvent>,
    pub turns: Vec<SessionTurn>,
}

pub struct TerminalTurnPersistenceSnapshot {
    pub turn: SessionTurn,
    pub events: Vec<SessionEvent>,
    pub assistant_messages: Vec<Message>,
}

pub struct NoisyOutputPersistenceSnapshot {
    pub events: Vec<SessionEvent>,
    pub messages: Vec<Message>,
}

pub struct TurnReconciliationSnapshot {
    pub turn: SessionTurn,
    pub events: Vec<SessionEvent>,
    pub last_turn_status: Option<SessionTurnStatus>,
    pub is_working: bool,
}

pub struct GlobalIdRoutingWorkspaceSessionSeed {
    pub name: String,
    pub root_path: PathBuf,
    pub base_commit: String,
    pub provider_id: String,
    pub model_id: String,
}

pub struct GlobalIdRoutingSessionFixture {
    pub session_id: SessionId,
}

pub struct TaskLifecycleWorktreeSeed {
    pub workspace_id: WorkspaceId,
    pub owner_task_id: TaskId,
    pub worktree_id: WorktreeId,
    pub root_path: PathBuf,
    pub base_commit: String,
    pub git_branch: String,
    pub make_primary: bool,
}

pub struct TaskLifecycleSandboxBindingSeed {
    pub worktree_id: WorktreeId,
    pub workspace_id: WorkspaceId,
    pub substrate: SandboxSubstrate,
    pub live_workspace_root: String,
    pub live_worktree_root: String,
    pub execution_settings_json: Option<String>,
    pub container_name: Option<String>,
    pub host_materialization_root: Option<PathBuf>,
}

pub struct TaskLifecycleSessionSeed {
    pub task_id: TaskId,
    pub workspace_id: WorkspaceId,
    pub worktree_id: WorktreeId,
    pub execution_environment: ExecutionEnvironment,
    pub title: String,
    pub parent_session_id: Option<SessionId>,
    pub role: Option<String>,
}

pub struct TaskLifecycleSnapshot {
    pub task: Option<Task>,
    pub worktree: Option<Worktree>,
    pub worktree_index_workspace_id: Option<WorkspaceId>,
    pub sandbox_binding: Option<SandboxBinding>,
}

impl TestDaemon {
    pub fn new(
        data_root: PathBuf,
        stores: StoreManager,
        providers: HashMap<String, Arc<dyn ProviderAdapter>>,
        daemon_url: String,
        auth_token: Option<String>,
    ) -> Self {
        Self::new_with_public_base_url(data_root, stores, providers, daemon_url, None, auth_token)
    }

    pub async fn new_for_test(data_root: PathBuf, daemon_url: String) -> anyhow::Result<Self> {
        let stores = StoreManager::open(&data_root).await?;
        Ok(Self::new(
            data_root,
            stores,
            HashMap::new(),
            daemon_url,
            None,
        ))
    }

    pub async fn new_with_providers_for_test(
        data_root: PathBuf,
        providers: HashMap<String, Arc<dyn ProviderAdapter>>,
        daemon_url: String,
        auth_token: Option<String>,
    ) -> anyhow::Result<Self> {
        let stores = StoreManager::open(&data_root).await?;
        Ok(Self::new(
            data_root, stores, providers, daemon_url, auth_token,
        ))
    }

    pub async fn new_with_runtime_flags_for_test(
        data_root: PathBuf,
        providers: HashMap<String, Arc<dyn ProviderAdapter>>,
        daemon_url: String,
        public_base_url: Option<String>,
        auth_token: Option<String>,
        runtime_flags: AppRuntimeFlags,
    ) -> anyhow::Result<Self> {
        let stores = StoreManager::open(&data_root).await?;
        Ok(Self::new_with_runtime_flags(
            data_root,
            stores,
            providers,
            daemon_url,
            public_base_url,
            auth_token,
            runtime_flags,
        ))
    }

    pub fn new_with_public_base_url(
        data_root: PathBuf,
        stores: StoreManager,
        providers: HashMap<String, Arc<dyn ProviderAdapter>>,
        daemon_url: String,
        public_base_url: Option<String>,
        auth_token: Option<String>,
    ) -> Self {
        Self::from_state(Arc::new(DaemonState::new_with_public_base_url(
            data_root,
            stores,
            providers,
            daemon_url,
            public_base_url,
            auth_token,
        )))
    }

    pub fn new_with_runtime_flags(
        data_root: PathBuf,
        stores: StoreManager,
        providers: HashMap<String, Arc<dyn ProviderAdapter>>,
        daemon_url: String,
        public_base_url: Option<String>,
        auth_token: Option<String>,
        runtime_flags: AppRuntimeFlags,
    ) -> Self {
        Self::from_state(Arc::new(DaemonState::new_with_runtime_flags(
            data_root,
            stores,
            providers,
            daemon_url,
            public_base_url,
            auth_token,
            runtime_flags,
        )))
    }

    pub fn from_state(state: Arc<DaemonState>) -> Self {
        Self { state }
    }

    pub fn handle(&self) -> DaemonHandle {
        DaemonHandle::new(self.state.core.shutdown_tx.clone())
    }

    pub fn route_handles(&self) -> DaemonRouteHandles {
        route_handles_from_state(&self.state)
    }

    pub fn shutdown_signal(&self) -> DaemonShutdownSignal {
        self.handle().shutdown_signal()
    }

    pub fn emit_shutdown_for_test(&self) {
        let _ = self.state.core.shutdown_tx.send(());
    }

    pub fn data_root(&self) -> &Path {
        &self.state.core.data_root
    }

    pub fn tool_output_spool_dir(&self) -> &Path {
        self.state.test_tool_output_spool_dir()
    }

    pub fn daemon_url(&self) -> &str {
        &self.state.core.daemon_url
    }

    pub fn global_store(&self) -> &Store {
        self.state.global_store()
    }

    pub fn stores(&self) -> &StoreManager {
        &self.state.core.stores
    }

    pub fn request_shutdown(&self) {
        let _ = self.state.core.shutdown_tx.send(());
    }

    pub async fn set_session_running(&self, session_id: SessionId, running: bool) {
        self.state
            .task_session_cleanup
            .set_running(session_id, running)
            .await;
    }

    pub async fn cache_worktree_vcs_snapshot_for_test(&self, snapshot: WorktreeVcsSnapshot) {
        self.state.test_cache_worktree_vcs_snapshot(snapshot).await;
    }

    pub async fn is_session_running(&self, session_id: SessionId) -> bool {
        self.state.sessions.is_running(session_id).await
    }

    pub async fn store_for_session(&self, session_id: SessionId) -> anyhow::Result<Store> {
        self.state.store_for_session(session_id).await
    }

    pub async fn store_for_workspace(&self, workspace_id: WorkspaceId) -> anyhow::Result<Store> {
        self.state.store_for_workspace(workspace_id).await
    }

    pub async fn uncached_store_for_workspace(
        &self,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<Store> {
        self.state
            .core
            .stores
            .workspace_uncached(workspace_id)
            .await
    }

    pub async fn store_for_task(&self, task_id: TaskId) -> anyhow::Result<Store> {
        self.state.store_for_task(task_id).await
    }

    pub async fn task_session_creation_lock(&self, task_id: TaskId) -> Arc<tokio::sync::Mutex<()>> {
        self.state
            .sessions
            .task_session_creation_lock(task_id)
            .await
    }
}
