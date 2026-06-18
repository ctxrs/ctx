use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use ctx_core::ids::WorkspaceId;
use ctx_core::models::Worktree;
use ctx_merge_queue::MergeQueueRuntime;
use ctx_observability::perf_telemetry::PerfTelemetry;
use ctx_observability::telemetry::Telemetry;
use ctx_store::Store;
use ctx_workspace_runtime::HarnessRuntimeManager;

use super::state::{ProtectedWorkspaceStoreLookup, WorkspaceFileCompletionsCache};
use super::ProviderWorkspaceLaunchRuntime;

#[derive(Clone)]
pub struct WorkspaceOrgPolicyHandle {
    global_store: Store,
    workspace_stores: ProtectedWorkspaceStoreLookup,
}

impl WorkspaceOrgPolicyHandle {
    pub(in crate::daemon) fn new(
        global_store: Store,
        workspace_stores: ProtectedWorkspaceStoreLookup,
    ) -> Self {
        Self {
            global_store,
            workspace_stores,
        }
    }

    pub(in crate::daemon) fn global_store(&self) -> &Store {
        &self.global_store
    }

    pub(in crate::daemon) async fn existing_workspace_store(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Store, crate::daemon::WorkspaceStoreAccessError> {
        self.workspace_stores
            .existing_workspace_store(workspace_id)
            .await
    }
}

#[derive(Clone)]
pub struct WorkspacePromptBootstrapConfigHandle {
    workspace_stores: ProtectedWorkspaceStoreLookup,
}

impl WorkspacePromptBootstrapConfigHandle {
    pub(in crate::daemon) fn new(workspace_stores: ProtectedWorkspaceStoreLookup) -> Self {
        Self { workspace_stores }
    }

    pub(in crate::daemon) async fn existing_workspace_store(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Store, crate::daemon::WorkspaceStoreAccessError> {
        self.workspace_stores
            .existing_workspace_store(workspace_id)
            .await
    }
}

#[derive(Clone)]
pub struct WorkspaceFileCompletionsHandle {
    global_store: Store,
    workspace_file_completions_cache: WorkspaceFileCompletionsCache,
    perf_telemetry: PerfTelemetry,
}

impl WorkspaceFileCompletionsHandle {
    pub(in crate::daemon) fn new(
        global_store: Store,
        workspace_file_completions_cache: WorkspaceFileCompletionsCache,
        perf_telemetry: PerfTelemetry,
    ) -> Self {
        Self {
            global_store,
            workspace_file_completions_cache,
            perf_telemetry,
        }
    }

    pub(in crate::daemon) fn global_store(&self) -> &Store {
        &self.global_store
    }

    pub(in crate::daemon) fn workspace_file_completions_cache(
        &self,
    ) -> &WorkspaceFileCompletionsCache {
        &self.workspace_file_completions_cache
    }

    pub(in crate::daemon) fn perf_telemetry(&self) -> &PerfTelemetry {
        &self.perf_telemetry
    }
}

#[derive(Clone)]
pub struct WorkspaceRegistryHandle {
    global_store: Store,
    workspace_stores: ProtectedWorkspaceStoreLookup,
    telemetry: Telemetry,
}

impl WorkspaceRegistryHandle {
    pub(in crate::daemon) fn new(
        global_store: Store,
        workspace_stores: ProtectedWorkspaceStoreLookup,
        telemetry: Telemetry,
    ) -> Self {
        Self {
            global_store,
            workspace_stores,
            telemetry,
        }
    }

    pub(in crate::daemon) fn global_store(&self) -> &Store {
        &self.global_store
    }

    pub(in crate::daemon) async fn existing_workspace_store(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Store, crate::daemon::WorkspaceStoreAccessError> {
        self.workspace_stores
            .existing_workspace_store(workspace_id)
            .await
    }

    pub(in crate::daemon) fn telemetry(&self) -> &Telemetry {
        &self.telemetry
    }
}

#[derive(Clone)]
pub struct WorkspaceAgentWorkHandle {
    workspace_stores: ProtectedWorkspaceStoreLookup,
}

impl WorkspaceAgentWorkHandle {
    pub(in crate::daemon) fn new(workspace_stores: ProtectedWorkspaceStoreLookup) -> Self {
        Self { workspace_stores }
    }

    pub(in crate::daemon) async fn existing_workspace_store(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Store, crate::daemon::WorkspaceStoreAccessError> {
        self.workspace_stores
            .existing_workspace_store(workspace_id)
            .await
    }
}

#[derive(Clone)]
pub struct WorkspaceDeletionHandle {
    runtime: Arc<crate::daemon::workspaces::WorkspaceDeletionRuntime>,
}

impl WorkspaceDeletionHandle {
    pub(in crate::daemon) fn new(
        runtime: Arc<crate::daemon::workspaces::WorkspaceDeletionRuntime>,
    ) -> Self {
        Self { runtime }
    }

    pub(in crate::daemon) async fn delete_workspace(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<(), crate::daemon::workspaces::WorkspaceDeleteError> {
        self.runtime.delete_workspace(workspace_id).await
    }

    #[cfg(test)]
    pub(in crate::daemon) fn fail_next_delete_after_begin_for_test(&self) {
        self.runtime.fail_next_delete_after_begin_for_test();
    }
}

#[derive(Clone)]
pub struct WorkspaceMergeQueueConfigHandle {
    workspace_stores: ProtectedWorkspaceStoreLookup,
    merge_queue: Arc<MergeQueueRuntime>,
}

impl WorkspaceMergeQueueConfigHandle {
    pub(in crate::daemon) fn new(
        workspace_stores: ProtectedWorkspaceStoreLookup,
        merge_queue: Arc<MergeQueueRuntime>,
    ) -> Self {
        Self {
            workspace_stores,
            merge_queue,
        }
    }

    pub(in crate::daemon) async fn existing_workspace_store(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Store, crate::daemon::WorkspaceStoreAccessError> {
        self.workspace_stores
            .existing_workspace_store(workspace_id)
            .await
    }

    pub(in crate::daemon) async fn schedule_store_if_enabled_and_queued(
        &self,
        store: &Store,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<bool> {
        ctx_merge_queue::schedule_store_if_enabled_and_queued(
            self.merge_queue.as_ref(),
            store,
            workspace_id,
        )
        .await
    }

    pub(in crate::daemon) async fn cancel_store_queued_entries_for_disabled_workspace(
        &self,
        store: &Store,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<()> {
        ctx_merge_queue::cancel_store_queued_entries_for_disabled_workspace(
            self.merge_queue.as_ref(),
            store,
            workspace_id,
        )
        .await
    }
}

#[derive(Clone)]
pub struct WorkspaceAttachmentsHandle {
    global_store: Store,
    workspace_stores: ProtectedWorkspaceStoreLookup,
    runtime: Arc<crate::daemon::workspaces::attachments::WorkspaceAttachmentsRuntime>,
}

impl WorkspaceAttachmentsHandle {
    pub(in crate::daemon) fn new(
        global_store: Store,
        workspace_stores: ProtectedWorkspaceStoreLookup,
        runtime: Arc<crate::daemon::workspaces::attachments::WorkspaceAttachmentsRuntime>,
    ) -> Self {
        Self {
            global_store,
            workspace_stores,
            runtime,
        }
    }

    pub(in crate::daemon) fn global_store(&self) -> &Store {
        &self.global_store
    }

    pub(in crate::daemon) async fn existing_workspace_store(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Store, crate::daemon::WorkspaceStoreAccessError> {
        self.workspace_stores
            .existing_workspace_store(workspace_id)
            .await
    }

    pub(in crate::daemon) fn runtime(
        &self,
    ) -> &Arc<crate::daemon::workspaces::attachments::WorkspaceAttachmentsRuntime> {
        &self.runtime
    }
}

pub(in crate::daemon) type WorkspacePrimaryBranchRefreshFuture =
    Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>>;
pub(crate) type WorkspacePrimaryBranchRefreshEffect =
    Arc<dyn Fn(Worktree) -> WorkspacePrimaryBranchRefreshFuture + Send + Sync>;

#[derive(Clone)]
pub struct WorkspacePrimaryBranchHandle {
    global_store: Store,
    workspace_stores: ProtectedWorkspaceStoreLookup,
    refresh_vcs_snapshot: WorkspacePrimaryBranchRefreshEffect,
}

impl WorkspacePrimaryBranchHandle {
    pub(in crate::daemon) fn new(
        global_store: Store,
        workspace_stores: ProtectedWorkspaceStoreLookup,
        refresh_vcs_snapshot: WorkspacePrimaryBranchRefreshEffect,
    ) -> Self {
        Self {
            global_store,
            workspace_stores,
            refresh_vcs_snapshot,
        }
    }

    pub(in crate::daemon) fn global_store(&self) -> &Store {
        &self.global_store
    }

    pub(in crate::daemon) async fn existing_workspace_store(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Store, crate::daemon::WorkspaceStoreAccessError> {
        self.workspace_stores
            .existing_workspace_store(workspace_id)
            .await
    }

    pub(in crate::daemon) async fn refresh_vcs_snapshot(
        &self,
        worktree: Worktree,
    ) -> anyhow::Result<()> {
        (self.refresh_vcs_snapshot)(worktree).await
    }
}

#[derive(Clone)]
pub struct WorkspaceExecutionConfigHandle {
    global_store: Store,
    workspace_stores: ProtectedWorkspaceStoreLookup,
    data_root: PathBuf,
}

impl WorkspaceExecutionConfigHandle {
    pub(in crate::daemon) fn new(
        global_store: Store,
        workspace_stores: ProtectedWorkspaceStoreLookup,
        data_root: PathBuf,
    ) -> Self {
        Self {
            global_store,
            workspace_stores,
            data_root,
        }
    }

    pub(in crate::daemon) fn global_store(&self) -> &Store {
        &self.global_store
    }

    pub(in crate::daemon) fn data_root(&self) -> &Path {
        &self.data_root
    }

    pub(in crate::daemon) async fn existing_workspace_store(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Store, crate::daemon::WorkspaceStoreAccessError> {
        self.workspace_stores
            .existing_workspace_store(workspace_id)
            .await
    }
}

#[derive(Clone)]
pub struct WorkspaceHarnessContainerHandle {
    global_store: Store,
    workspace_stores: ProtectedWorkspaceStoreLookup,
    daemon_url: String,
    harness: Arc<HarnessRuntimeManager>,
}

impl WorkspaceHarnessContainerHandle {
    pub(in crate::daemon) fn new(
        global_store: Store,
        workspace_stores: ProtectedWorkspaceStoreLookup,
        daemon_url: String,
        harness: Arc<HarnessRuntimeManager>,
    ) -> Self {
        Self {
            global_store,
            workspace_stores,
            daemon_url,
            harness,
        }
    }

    pub(in crate::daemon) fn global_store(&self) -> &Store {
        &self.global_store
    }

    pub(in crate::daemon) async fn store_for_workspace(
        &self,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<Store> {
        self.workspace_stores
            .store_for_workspace(workspace_id)
            .await
    }

    pub(in crate::daemon) fn daemon_url(&self) -> &str {
        &self.daemon_url
    }

    pub(in crate::daemon) fn harness(&self) -> &HarnessRuntimeManager {
        self.harness.as_ref()
    }
}

#[derive(Clone)]
pub struct WorkspaceWorktreeHandle {
    global_store: Store,
    workspace_stores: ProtectedWorkspaceStoreLookup,
    data_root: PathBuf,
}

impl WorkspaceWorktreeHandle {
    pub(in crate::daemon) fn new(
        global_store: Store,
        workspace_stores: ProtectedWorkspaceStoreLookup,
        data_root: PathBuf,
    ) -> Self {
        Self {
            global_store,
            workspace_stores,
            data_root,
        }
    }

    pub(in crate::daemon) fn global_store(&self) -> &Store {
        &self.global_store
    }

    pub(in crate::daemon) async fn store_for_workspace(
        &self,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<Store> {
        self.workspace_stores
            .store_for_workspace(workspace_id)
            .await
    }

    pub(in crate::daemon) async fn existing_workspace_store(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Store, crate::daemon::WorkspaceStoreAccessError> {
        self.workspace_stores
            .existing_workspace_store(workspace_id)
            .await
    }

    pub(in crate::daemon) fn data_root(&self) -> &Path {
        &self.data_root
    }
}

#[derive(Clone)]
pub struct WorkspaceProviderModelPreferenceHandle {
    launch: Arc<ProviderWorkspaceLaunchRuntime>,
}

impl WorkspaceProviderModelPreferenceHandle {
    pub(in crate::daemon) fn new(launch: Arc<ProviderWorkspaceLaunchRuntime>) -> Self {
        Self { launch }
    }

    pub(in crate::daemon) fn launch(&self) -> &ProviderWorkspaceLaunchRuntime {
        self.launch.as_ref()
    }
}
