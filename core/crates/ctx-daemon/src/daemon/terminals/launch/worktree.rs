use ctx_core::ids::{SessionId, TaskId, WorkspaceId, WorktreeId};
use ctx_core::models::Worktree;

use super::{TerminalLaunchError, TerminalLaunchHost};

pub(super) async fn resolve_terminal_worktree(
    host: &TerminalLaunchHost,
    workspace_id: WorkspaceId,
    worktree_id: Option<WorktreeId>,
    session_id: Option<SessionId>,
    task_id: Option<TaskId>,
) -> Result<Option<Worktree>, TerminalLaunchError> {
    if let Some(worktree_id) = worktree_id {
        return host
            .load_explicit_terminal_worktree(workspace_id, worktree_id)
            .await
            .map(Some);
    }
    if session_id.is_some() || task_id.is_some() {
        return infer_terminal_worktree(host, workspace_id, session_id, task_id).await;
    }
    Ok(None)
}

pub async fn infer_terminal_worktree(
    host: &TerminalLaunchHost,
    workspace_id: WorkspaceId,
    session_id: Option<SessionId>,
    task_id: Option<TaskId>,
) -> Result<Option<Worktree>, TerminalLaunchError> {
    if let Some(session_id) = session_id {
        return host
            .load_terminal_session_worktree(workspace_id, session_id)
            .await
            .map(Some);
    }

    if let Some(task_id) = task_id {
        return host
            .load_terminal_task_worktree(workspace_id, task_id)
            .await
            .map(Some);
    }

    Ok(host
        .default_terminal_worktree_candidate(workspace_id)
        .await
        .unwrap_or(None))
}
