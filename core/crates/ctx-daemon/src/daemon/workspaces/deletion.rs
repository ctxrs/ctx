use std::path::PathBuf;
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use ctx_core::ids::{SessionId, WorkspaceId};
use ctx_core::models::{Workspace, Worktree};
use ctx_merge_queue::MergeQueueRuntime;
use ctx_provider_runtime::ProviderRuntime;
use ctx_session_runtime::runtime::SessionLifecycleHost;
use ctx_store::{Store, StoreManager};
use ctx_workspace_active_snapshot::WorkspaceActiveSnapshotHub;
use ctx_workspace_runtime::HarnessRuntimeManager;

use crate::daemon::state::{
    ProtectedWorkspaceStoreLookup, SessionRuntime, WorkspaceActiveHeadsCache,
    WorkspaceActiveSnapshotCache, WorkspaceFileCompletionsCache,
};

use super::vcs_hooks::{cleanup_worktree_hooks_with_host, WorkspaceVcsHookHost};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceDeleteError {
    NotFound,
    Internal,
}

#[derive(Clone)]
pub(in crate::daemon) struct WorkspaceDeletionRuntime {
    data_root: PathBuf,
    stores: StoreManager,
    global_store: Store,
    sessions: Arc<SessionRuntime>,
    active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
    workspace_active_snapshot_cache: WorkspaceActiveSnapshotCache,
    workspace_active_heads_cache: WorkspaceActiveHeadsCache,
    workspace_file_completions_cache: WorkspaceFileCompletionsCache,
    harness: Arc<HarnessRuntimeManager>,
    session_lifecycle: WorkspaceDeletionSessionLifecycleHost,
    vcs_hooks: Arc<WorkspaceVcsHookHost>,
    #[cfg(test)]
    fail_after_begin_for_test: Arc<AtomicBool>,
}

pub(in crate::daemon) struct WorkspaceDeletionRuntimeDeps {
    pub(in crate::daemon) data_root: PathBuf,
    pub(in crate::daemon) daemon_url: String,
    pub(in crate::daemon) stores: StoreManager,
    pub(in crate::daemon) global_store: Store,
    pub(in crate::daemon) sessions: Arc<SessionRuntime>,
    pub(in crate::daemon) active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
    pub(in crate::daemon) workspace_active_snapshot_cache: WorkspaceActiveSnapshotCache,
    pub(in crate::daemon) workspace_active_heads_cache: WorkspaceActiveHeadsCache,
    pub(in crate::daemon) workspace_file_completions_cache: WorkspaceFileCompletionsCache,
    pub(in crate::daemon) harness: Arc<HarnessRuntimeManager>,
    pub(in crate::daemon) providers: Arc<ProviderRuntime>,
    pub(in crate::daemon) merge_queue: Arc<MergeQueueRuntime>,
}

impl WorkspaceDeletionRuntime {
    pub(in crate::daemon) fn new(deps: WorkspaceDeletionRuntimeDeps) -> Self {
        let WorkspaceDeletionRuntimeDeps {
            data_root,
            daemon_url,
            stores,
            global_store,
            sessions,
            active_snapshot,
            workspace_active_snapshot_cache,
            workspace_active_heads_cache,
            workspace_file_completions_cache,
            harness,
            providers,
            merge_queue,
        } = deps;
        let session_lifecycle = WorkspaceDeletionSessionLifecycleHost::new(
            global_store.clone(),
            Arc::clone(&active_snapshot),
            providers,
        );
        let workspace_stores =
            ProtectedWorkspaceStoreLookup::new(stores.clone(), Arc::clone(&sessions), merge_queue);
        let vcs_hooks = Arc::new(WorkspaceVcsHookHost::new(
            data_root.clone(),
            daemon_url,
            global_store.clone(),
            workspace_stores,
            Arc::clone(&harness),
        ));
        Self {
            data_root,
            stores,
            global_store,
            sessions,
            active_snapshot,
            workspace_active_snapshot_cache,
            workspace_active_heads_cache,
            workspace_file_completions_cache,
            harness,
            session_lifecycle,
            vcs_hooks,
            #[cfg(test)]
            fail_after_begin_for_test: Arc::new(AtomicBool::new(false)),
        }
    }

    #[cfg(test)]
    pub(in crate::daemon) fn fail_next_delete_after_begin_for_test(&self) {
        self.fail_after_begin_for_test.store(true, Ordering::SeqCst);
    }

    pub(in crate::daemon) async fn delete_workspace(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<(), WorkspaceDeleteError> {
        let workspace = self
            .global_store
            .get_workspace(workspace_id)
            .await
            .map_err(|_| WorkspaceDeleteError::Internal)?
            .ok_or(WorkspaceDeleteError::NotFound)?;
        let worktrees = self.worktrees_for_delete(workspace_id).await;

        self.stores.begin_workspace_delete(workspace_id).await;
        #[cfg(test)]
        let delete_result = if self.fail_after_begin_for_test.swap(false, Ordering::SeqCst) {
            Err(WorkspaceDeleteError::Internal)
        } else {
            self.delete_workspace_after_begin(&workspace, &worktrees)
                .await
        };
        #[cfg(not(test))]
        let delete_result = self
            .delete_workspace_after_begin(&workspace, &worktrees)
            .await;
        self.stores.finish_workspace_delete(workspace_id).await;
        delete_result?;

        if let Err(err) =
            ctx_worktree_vcs_service::cleanup_workspace_hooks(&self.data_root, workspace_id).await
        {
            tracing::warn!(
                workspace_id = %workspace_id.0,
                "failed to remove vcs hooks: {err:#}"
            );
        }

        let workspace_db_dir = self
            .data_root
            .join("db")
            .join("workspaces")
            .join(workspace_id.0.to_string());
        let _ = tokio::fs::remove_dir_all(workspace_db_dir).await;
        Ok(())
    }

    async fn worktrees_for_delete(&self, workspace_id: WorkspaceId) -> Vec<Worktree> {
        match self.stores.workspace(workspace_id).await {
            Ok(store) => store.list_worktrees(workspace_id).await.unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    async fn delete_workspace_after_begin(
        &self,
        workspace: &Workspace,
        worktrees: &[Worktree],
    ) -> Result<(), WorkspaceDeleteError> {
        let workspace_id = workspace.id;
        for worktree in worktrees {
            if let Err(err) =
                cleanup_worktree_hooks_with_host(self.vcs_hooks.as_ref(), workspace, worktree).await
            {
                tracing::warn!(
                    workspace_id = %workspace_id.0,
                    worktree_id = %worktree.id.0,
                    "failed to remove vcs hooks: {err:#}"
                );
            }
        }
        self.cleanup_workspace_runtime(workspace_id).await;
        self.stores
            .evict_workspace_and_wait_closed(workspace_id)
            .await;
        self.global_store
            .delete_workspace_indexes(workspace_id)
            .await
            .map_err(|_| WorkspaceDeleteError::Internal)?;
        self.global_store
            .delete_workspace(workspace_id)
            .await
            .map_err(|_| WorkspaceDeleteError::Internal)?;
        Ok(())
    }

    async fn cleanup_workspace_runtime(&self, workspace_id: WorkspaceId) {
        // Best-effort: workspace deletion should attempt to clean up its harness container + volume,
        // but must not fail deletion if the sandbox container runtime is unavailable.
        let _ = self.harness.stop_container(workspace_id).await;
        let _ = self.harness.remove_workspace_volume(workspace_id).await;

        let session_ids = self
            .sessions
            .cached_session_ids_for_workspace(workspace_id)
            .await;
        for session_id in session_ids {
            self.sessions
                .cleanup_session_with_host(&self.session_lifecycle, session_id)
                .await;
        }
        {
            let mut cache = self.workspace_active_snapshot_cache.lock().await;
            cache.remove(&workspace_id);
        }
        {
            let mut cache = self.workspace_active_heads_cache.lock().await;
            cache.remove(&workspace_id);
        }
        {
            let mut cache = self.workspace_file_completions_cache.lock().await;
            cache.remove(&workspace_id);
        }
        self.active_snapshot.remove_workspace(workspace_id).await;
        self.stores.evict_workspace(workspace_id).await;
    }
}

#[derive(Clone)]
struct WorkspaceDeletionSessionLifecycleHost {
    global_store: Store,
    active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
    providers: Arc<ProviderRuntime>,
}

impl WorkspaceDeletionSessionLifecycleHost {
    fn new(
        global_store: Store,
        active_snapshot: Arc<WorkspaceActiveSnapshotHub>,
        providers: Arc<ProviderRuntime>,
    ) -> Self {
        Self {
            global_store,
            active_snapshot,
            providers,
        }
    }
}

#[async_trait]
impl SessionLifecycleHost for WorkspaceDeletionSessionLifecycleHost {
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
            self.active_snapshot
                .remove_session_with_workspace_hint(workspace_id, session_id)
                .await;
        } else {
            self.active_snapshot.remove_session(session_id).await;
        }
    }
}
