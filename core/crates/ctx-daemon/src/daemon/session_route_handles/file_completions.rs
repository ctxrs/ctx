use std::path::{Path, PathBuf};
use std::sync::Arc;

use ctx_core::ids::{SessionId, WorkspaceId};
use ctx_observability::perf_telemetry::PerfTelemetry;
use ctx_store::Store;
use ctx_workspace_runtime::HarnessRuntimeManager;

use crate::daemon::state::{
    ProtectedWorkspaceStoreLookup, SessionStoreLookup, WorktreeFileCompletionsCache,
};

#[derive(Clone)]
pub struct SessionFileCompletionsHandle {
    global_store: Store,
    session_stores: SessionStoreLookup,
    workspace_stores: ProtectedWorkspaceStoreLookup,
    worktree_file_completions_cache: WorktreeFileCompletionsCache,
    perf_telemetry: PerfTelemetry,
    data_root: PathBuf,
    daemon_url: String,
    harness: Arc<HarnessRuntimeManager>,
}

pub(in crate::daemon) struct SessionFileCompletionsHandleParts {
    pub(in crate::daemon) global_store: Store,
    pub(in crate::daemon) session_stores: SessionStoreLookup,
    pub(in crate::daemon) workspace_stores: ProtectedWorkspaceStoreLookup,
    pub(in crate::daemon) worktree_file_completions_cache: WorktreeFileCompletionsCache,
    pub(in crate::daemon) perf_telemetry: PerfTelemetry,
    pub(in crate::daemon) data_root: PathBuf,
    pub(in crate::daemon) daemon_url: String,
    pub(in crate::daemon) harness: Arc<HarnessRuntimeManager>,
}

impl SessionFileCompletionsHandle {
    pub(in crate::daemon) fn new(parts: SessionFileCompletionsHandleParts) -> Self {
        Self {
            global_store: parts.global_store,
            session_stores: parts.session_stores,
            workspace_stores: parts.workspace_stores,
            worktree_file_completions_cache: parts.worktree_file_completions_cache,
            perf_telemetry: parts.perf_telemetry,
            data_root: parts.data_root,
            daemon_url: parts.daemon_url,
            harness: parts.harness,
        }
    }

    pub(in crate::daemon) fn global_store(&self) -> &Store {
        &self.global_store
    }

    pub(in crate::daemon) async fn existing_session_store(
        &self,
        session_id: SessionId,
    ) -> Result<Store, crate::daemon::SessionStoreAccessError> {
        self.session_stores.existing_session_store(session_id).await
    }

    pub(in crate::daemon) async fn store_for_workspace(
        &self,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<Store> {
        self.workspace_stores
            .store_for_workspace(workspace_id)
            .await
    }

    pub(in crate::daemon) fn worktree_file_completions_cache(
        &self,
    ) -> &WorktreeFileCompletionsCache {
        &self.worktree_file_completions_cache
    }

    pub(in crate::daemon) fn perf_telemetry(&self) -> &PerfTelemetry {
        &self.perf_telemetry
    }

    pub(in crate::daemon) fn data_root(&self) -> &Path {
        &self.data_root
    }

    pub(in crate::daemon) fn daemon_url(&self) -> &str {
        &self.daemon_url
    }

    pub(in crate::daemon) fn harness(&self) -> &HarnessRuntimeManager {
        self.harness.as_ref()
    }
}
