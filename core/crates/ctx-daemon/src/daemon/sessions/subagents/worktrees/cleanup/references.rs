use ctx_core::models::Session;

pub(super) async fn archived_worktree_has_other_references(
    store: &ctx_store::Store,
    parent: &Session,
    child: &Session,
) -> bool {
    let sharing_sessions = match store
        .list_all_sessions_for_worktree(child.worktree_id)
        .await
    {
        Ok(sessions) => sessions,
        Err(error) => {
            tracing::warn!(
                parent_session_id = %parent.id.0,
                child_session_id = %child.id.0,
                worktree_id = %child.worktree_id.0,
                "failed to load archived subagent worktree session references: {error:#}"
            );
            return true;
        }
    };
    if sharing_sessions
        .iter()
        .any(|session| session.id != child.id)
    {
        tracing::warn!(
            parent_session_id = %parent.id.0,
            child_session_id = %child.id.0,
            worktree_id = %child.worktree_id.0,
            "archived subagent worktree is still referenced by another session"
        );
        return true;
    }

    let other_tasks = match store
        .count_tasks_for_worktree(child.worktree_id, Some(child.task_id))
        .await
    {
        Ok(count) => count,
        Err(error) => {
            tracing::warn!(
                parent_session_id = %parent.id.0,
                child_session_id = %child.id.0,
                worktree_id = %child.worktree_id.0,
                "failed to load archived subagent worktree task references: {error:#}"
            );
            return true;
        }
    };
    if other_tasks > 0 {
        tracing::warn!(
            parent_session_id = %parent.id.0,
            child_session_id = %child.id.0,
            worktree_id = %child.worktree_id.0,
            other_tasks,
            "archived subagent worktree is still referenced by another task"
        );
        return true;
    }

    false
}
