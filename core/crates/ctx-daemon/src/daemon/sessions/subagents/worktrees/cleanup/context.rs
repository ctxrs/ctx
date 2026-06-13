use ctx_core::models::{SandboxBinding, Session, Workspace, Worktree};
use ctx_store::Store;

pub(super) struct ArchivedWorktreeCleanupContext {
    pub(super) worktree: Worktree,
    pub(super) workspace: Workspace,
    pub(super) sandbox_binding: Option<SandboxBinding>,
    pub(super) cleanup_failed: bool,
}

pub(super) async fn load_archived_worktree_cleanup_context(
    global_store: &Store,
    store: &ctx_store::Store,
    parent: &Session,
    child: &Session,
) -> Option<ArchivedWorktreeCleanupContext> {
    let worktree = match store.get_worktree(child.worktree_id).await {
        Ok(Some(worktree)) => worktree,
        Ok(None) => {
            tracing::warn!(
                parent_session_id = %parent.id.0,
                child_session_id = %child.id.0,
                worktree_id = %child.worktree_id.0,
                "archived subagent worktree metadata was missing during cleanup"
            );
            return None;
        }
        Err(error) => {
            tracing::warn!(
                parent_session_id = %parent.id.0,
                child_session_id = %child.id.0,
                worktree_id = %child.worktree_id.0,
                "failed to load archived subagent worktree metadata: {error:#}"
            );
            return None;
        }
    };
    let workspace = match global_store.get_workspace(child.workspace_id).await {
        Ok(Some(workspace)) => workspace,
        Ok(None) => {
            tracing::warn!(
                parent_session_id = %parent.id.0,
                child_session_id = %child.id.0,
                workspace_id = %child.workspace_id.0,
                "workspace not found while cleaning archived subagent worktree"
            );
            return None;
        }
        Err(error) => {
            tracing::warn!(
                parent_session_id = %parent.id.0,
                child_session_id = %child.id.0,
                workspace_id = %child.workspace_id.0,
                "failed to load workspace while cleaning archived subagent worktree: {error:#}"
            );
            return None;
        }
    };
    let mut cleanup_failed = false;
    let sandbox_binding = match store.get_sandbox_binding(worktree.id).await {
        Ok(binding) => binding,
        Err(error) => {
            tracing::warn!(
                parent_session_id = %parent.id.0,
                child_session_id = %child.id.0,
                worktree_id = %worktree.id.0,
                "failed to load archived subagent sandbox binding for cleanup: {error:#}"
            );
            cleanup_failed = true;
            None
        }
    };

    Some(ArchivedWorktreeCleanupContext {
        worktree,
        workspace,
        sandbox_binding,
        cleanup_failed,
    })
}
