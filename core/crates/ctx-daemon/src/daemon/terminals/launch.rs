use std::path::{Path, PathBuf};
use std::sync::Arc;

use ctx_core::ids::{SessionId, TaskId, WorkspaceId, WorktreeId};
use ctx_core::models::{TerminalSession, Workspace, Worktree};
use ctx_settings_model::{ExecutionMode, ExecutionSettings};
use ctx_store::Store;
use ctx_transport_runtime::terminal_launch::{container_terminal_env, TerminalLaunchError};
use ctx_transport_runtime::terminals::{TerminalCreateRequest, TerminalManager};
use ctx_workspace_runtime::HarnessRuntimeManager;
use ctx_worktree_data_plane::{apply_data_plane_to_execution_settings, workspace_data_plane};

use crate::daemon::ProtectedWorkspaceStoreLookup;

mod container;
mod data_plane;
mod paths;
mod worktree;

use self::container::prepare_terminal_container_launch;
use self::data_plane::resolve_terminal_worktree_data_plane;
use self::paths::resolve_terminal_paths;
#[cfg(test)]
pub use self::worktree::infer_terminal_worktree;
use self::worktree::resolve_terminal_worktree;

#[derive(Debug)]
pub struct CreateTerminalLaunchRequest {
    pub workspace_id: WorkspaceId,
    pub task_id: Option<TaskId>,
    pub session_id: Option<SessionId>,
    pub worktree_id: Option<WorktreeId>,
    pub cwd: Option<String>,
    pub shell: Option<String>,
}

#[derive(Clone)]
pub(in crate::daemon) struct TerminalLaunchHost {
    global_store: Store,
    workspace_stores: ProtectedWorkspaceStoreLookup,
    data_root: PathBuf,
    daemon_url: String,
    harness: Arc<HarnessRuntimeManager>,
    terminals: Arc<TerminalManager>,
}

impl TerminalLaunchHost {
    pub(in crate::daemon) fn new(
        global_store: Store,
        workspace_stores: ProtectedWorkspaceStoreLookup,
        data_root: PathBuf,
        daemon_url: String,
        harness: Arc<HarnessRuntimeManager>,
        terminals: Arc<TerminalManager>,
    ) -> Self {
        Self {
            global_store,
            workspace_stores,
            data_root,
            daemon_url,
            harness,
            terminals,
        }
    }

    pub(super) fn data_root(&self) -> &Path {
        &self.data_root
    }

    pub(super) fn daemon_url(&self) -> &str {
        &self.daemon_url
    }

    pub(super) fn harness(&self) -> &HarnessRuntimeManager {
        self.harness.as_ref()
    }

    async fn load_workspace(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Workspace, TerminalLaunchError> {
        self.global_store
            .get_workspace(workspace_id)
            .await
            .map_err(|_| internal_error("failed to load workspace"))?
            .ok_or_else(|| not_found("workspace not found"))
    }

    async fn effective_execution_settings(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<ExecutionSettings, TerminalLaunchError> {
        let store = self
            .workspace_stores
            .store_for_workspace(workspace_id)
            .await
            .map_err(|_| internal_error("failed to load execution settings"))?;
        ctx_settings_service::effective_execution_settings(&self.global_store, &store)
            .await
            .map_err(|_| internal_error("failed to load execution settings"))
    }

    async fn load_explicit_terminal_worktree(
        &self,
        workspace_id: WorkspaceId,
        worktree_id: WorktreeId,
    ) -> Result<Worktree, TerminalLaunchError> {
        let store = self
            .workspace_stores
            .store_for_worktree(worktree_id)
            .await
            .map_err(|_| not_found("worktree not found"))?;
        let worktree = store
            .get_worktree(worktree_id)
            .await
            .map_err(|_| internal_error("failed to load worktree"))?
            .ok_or_else(|| not_found("worktree not found"))?;
        if worktree.workspace_id != workspace_id {
            return Err(not_found("worktree not found"));
        }
        Ok(worktree)
    }

    async fn load_terminal_session_worktree(
        &self,
        workspace_id: WorkspaceId,
        session_id: SessionId,
    ) -> Result<Worktree, TerminalLaunchError> {
        let session_workspace_id = self
            .global_store
            .get_workspace_id_for_session(session_id)
            .await
            .map_err(|_| not_found("session not found"))?
            .ok_or_else(|| not_found("session not found"))?;
        if session_workspace_id != workspace_id {
            return Err(not_found("session not found"));
        }
        let store = self
            .workspace_stores
            .store_for_workspace(session_workspace_id)
            .await
            .map_err(|_| not_found("session not found"))?;
        let session = store
            .get_session(session_id)
            .await
            .map_err(|_| internal_error("failed to load session"))?
            .ok_or_else(|| not_found("session not found"))?;
        if session.workspace_id != workspace_id {
            return Err(not_found("session not found"));
        }
        let worktree = store
            .get_worktree(session.worktree_id)
            .await
            .map_err(|_| internal_error("failed to load worktree"))?
            .ok_or_else(|| not_found("worktree not found"))?;
        if worktree.workspace_id != workspace_id {
            return Err(not_found("worktree not found"));
        }
        Ok(worktree)
    }

    async fn load_terminal_task_worktree(
        &self,
        workspace_id: WorkspaceId,
        task_id: TaskId,
    ) -> Result<Worktree, TerminalLaunchError> {
        let store = self
            .workspace_stores
            .store_for_task(task_id)
            .await
            .map_err(|_| not_found("task not found"))?;
        let task = store
            .get_task(task_id)
            .await
            .map_err(|_| internal_error("failed to load task"))?
            .ok_or_else(|| not_found("task not found"))?;
        if task.workspace_id != workspace_id {
            return Err(not_found("task not found"));
        }
        let primary_worktree_id = task
            .primary_worktree_id
            .ok_or_else(|| not_found("worktree not found"))?;
        let worktree = store
            .get_worktree(primary_worktree_id)
            .await
            .map_err(|_| internal_error("failed to load worktree"))?
            .ok_or_else(|| not_found("worktree not found"))?;
        if worktree.workspace_id != workspace_id {
            return Err(not_found("worktree not found"));
        }
        Ok(worktree)
    }

    async fn default_terminal_worktree_candidate(
        &self,
        workspace_id: WorkspaceId,
    ) -> anyhow::Result<Option<Worktree>> {
        let store = self
            .workspace_stores
            .store_for_workspace(workspace_id)
            .await?;
        let worktrees = store.list_worktrees(workspace_id).await?;
        Ok(worktrees.into_iter().last())
    }

    async fn create_terminal(
        &self,
        request: TerminalCreateRequest,
    ) -> Result<TerminalSession, TerminalLaunchError> {
        let session = self
            .terminals
            .create(request)
            .await
            .map_err(|e| internal_error(format!("failed to create terminal: {e}")))?;
        Ok(session.snapshot())
    }
}

pub(super) async fn create_workspace_terminal(
    host: &TerminalLaunchHost,
    req: CreateTerminalLaunchRequest,
) -> Result<TerminalSession, TerminalLaunchError> {
    let workspace_id = req.workspace_id;
    let workspace = host.load_workspace(workspace_id).await?;

    let effective = host.effective_execution_settings(workspace_id).await?;
    let worktree = resolve_terminal_worktree(
        host,
        workspace_id,
        req.worktree_id,
        req.session_id,
        req.task_id,
    )
    .await?;
    let worktree_data_plane = if let Some(worktree) = worktree.as_ref() {
        Some(
            resolve_terminal_worktree_data_plane(host, worktree)
                .await
                .map_err(|_| internal_error("failed to resolve worktree data plane"))?,
        )
    } else if matches!(effective.mode, ExecutionMode::Sandbox) {
        Some(workspace_data_plane(&workspace, effective.mode.clone()))
    } else {
        None
    };
    let effective = worktree_data_plane
        .as_ref()
        .map(|data_plane| {
            apply_data_plane_to_execution_settings(&effective, data_plane)
                .map_err(|_| internal_error("failed to apply worktree data plane"))
        })
        .transpose()?
        .unwrap_or(effective);
    let container_mode = matches!(effective.mode, ExecutionMode::Sandbox);
    let paths = resolve_terminal_paths(
        &workspace,
        worktree.as_ref(),
        worktree_data_plane.as_ref(),
        container_mode,
        req.cwd.as_deref(),
        req.shell.as_deref(),
    )
    .await?;

    let (cwd, native_container, shared_vm_container) = prepare_terminal_container_launch(
        host,
        &workspace,
        worktree.as_ref(),
        &effective,
        workspace_id,
        &paths.cwd,
        paths.container_cwd_authority_root.as_deref(),
    )
    .await?;
    host.create_terminal(TerminalCreateRequest {
        workspace_id,
        task_id: req.task_id,
        session_id: req.session_id,
        worktree_id: worktree.as_ref().map(|wt| wt.id),
        cwd,
        shell: paths.shell,
        cols: None,
        rows: None,
        env: if container_mode {
            container_terminal_env()
        } else {
            Default::default()
        },
        native_container,
        shared_vm_container,
    })
    .await
}

pub(super) fn not_found(error: impl Into<String>) -> TerminalLaunchError {
    TerminalLaunchError::not_found(error)
}

pub(super) fn internal_error(error: impl Into<String>) -> TerminalLaunchError {
    TerminalLaunchError::internal(error)
}

#[cfg(test)]
mod tests;
