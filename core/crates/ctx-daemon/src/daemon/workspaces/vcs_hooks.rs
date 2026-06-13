#[path = "vcs_hooks/host.rs"]
mod host;
#[path = "vcs_hooks/sandbox.rs"]
mod sandbox;

use anyhow::Result;
use ctx_core::ids::{TaskId, WorkspaceId};
use ctx_core::models::{Workspace, Worktree};
use ctx_worktree_vcs_service::VcsHooksHost;

use crate::daemon::DaemonState;

pub(in crate::daemon) use host::WorkspaceVcsHookHost;

pub async fn ensure_task_commit_hook(
    state: &DaemonState,
    workspace: &Workspace,
    worktree: &Worktree,
    task_id: TaskId,
) -> Result<()> {
    ctx_worktree_vcs_service::ensure_task_commit_hook(state, workspace, worktree, task_id).await
}

pub async fn cleanup_worktree_hooks(
    state: &DaemonState,
    workspace: &Workspace,
    worktree: &Worktree,
) -> Result<()> {
    ctx_worktree_vcs_service::cleanup_worktree_hooks(state, workspace, worktree).await
}

pub async fn cleanup_worktree_hooks_with_host<H: VcsHooksHost>(
    host: &H,
    workspace: &Workspace,
    worktree: &Worktree,
) -> Result<()> {
    ctx_worktree_vcs_service::cleanup_worktree_hooks(host, workspace, worktree).await
}

pub async fn cleanup_workspace_hooks(state: &DaemonState, workspace_id: WorkspaceId) -> Result<()> {
    ctx_worktree_vcs_service::cleanup_workspace_hooks(&state.core.data_root, workspace_id).await
}

#[cfg(test)]
mod tests;
