use super::*;

pub(super) struct ExistingRequestedSession<'a> {
    pub(super) handles: &'a TaskSessionHandles,
    pub(super) store: &'a Store,
    pub(super) task: &'a Task,
    pub(super) workspace: &'a Workspace,
    pub(super) requested_session_id: Option<SessionId>,
    pub(super) created_worktree_id: Option<WorktreeId>,
    pub(super) identity: SessionCreationIdentity<'a>,
    pub(super) remember_model_preference: bool,
    pub(super) preferred_model_id: &'a str,
}

pub(super) async fn resolve_existing_requested_session(
    request: ExistingRequestedSession<'_>,
) -> Result<Option<Session>, TaskSessionCreateError> {
    let ExistingRequestedSession {
        handles,
        store,
        task,
        workspace,
        requested_session_id,
        created_worktree_id,
        identity,
        remember_model_preference,
        preferred_model_id,
    } = request;
    let Some(session_id) = requested_session_id else {
        return Ok(None);
    };
    let existing_ws = handles
        .admission
        .get_workspace_id_for_session(session_id)
        .await
        .map_err(TaskSessionCreateError::Internal)?;
    let Some(existing_ws) = existing_ws else {
        return Ok(None);
    };
    if existing_ws != task.workspace_id {
        cleanup_created_worktree(handles, store, workspace, task.id, created_worktree_id).await;
        return Err(TaskSessionCreateError::Conflict);
    }

    let existing = store
        .get_session(session_id)
        .await
        .map_err(TaskSessionCreateError::Internal)?;
    let Some(existing) = existing else {
        cleanup_created_worktree(handles, store, workspace, task.id, created_worktree_id).await;
        return Err(TaskSessionCreateError::Internal(anyhow::anyhow!(
            "session index exists but session row missing"
        )));
    };
    if !session_matches_creation_identity(&existing, identity) {
        cleanup_created_worktree(handles, store, workspace, task.id, created_worktree_id).await;
        return Err(TaskSessionCreateError::Conflict);
    }

    handles.admission.remember_session_meta(&existing).await;
    if remember_model_preference {
        if let Err(error) = handles
            .admission
            .update_workspace_provider_preferred_model_id(
                task.workspace_id,
                identity.provider_id,
                Some(preferred_model_id.to_string()),
            )
            .await
        {
            tracing::warn!(
                session_id = %existing.id.0,
                workspace_id = %task.workspace_id.0,
                provider_id = identity.provider_id,
                "failed to persist workspace provider model preference: {error:#}"
            );
        }
    }

    Ok(Some(existing))
}

async fn cleanup_created_worktree(
    handles: &TaskSessionHandles,
    store: &Store,
    workspace: &Workspace,
    task_id: TaskId,
    created_worktree_id: Option<WorktreeId>,
) {
    if let Some(created_worktree_id) = created_worktree_id {
        cleanup_orphaned_provisioned_worktree(
            handles,
            store,
            workspace,
            task_id,
            created_worktree_id,
        )
        .await;
    }
}
