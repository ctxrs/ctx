use super::*;

pub(super) struct CreateSessionRecord<'a> {
    pub(super) store: &'a Store,
    pub(super) task_id: TaskId,
    pub(super) workspace_id: WorkspaceId,
    pub(super) requested_session_id: Option<SessionId>,
    pub(super) worktree_id: WorktreeId,
    pub(super) execution_environment: ExecutionEnvironment,
    pub(super) provider_id: &'a str,
    pub(super) model_id: &'a str,
    pub(super) reasoning_effort: Option<&'a str>,
    pub(super) parent_session_id: Option<SessionId>,
    pub(super) relationship: Option<&'a str>,
    pub(super) cleanup: CreatedWorktreeCleanup<'a>,
}

pub(super) async fn create_session_record(
    request: CreateSessionRecord<'_>,
) -> Result<Session, TaskSessionCreateError> {
    let CreateSessionRecord {
        store,
        task_id,
        workspace_id,
        requested_session_id,
        worktree_id,
        execution_environment,
        provider_id,
        model_id,
        reasoning_effort,
        parent_session_id,
        relationship,
        cleanup,
    } = request;

    let result = if let Some(session_id) = requested_session_id {
        store
            .create_session_with_id_and_reasoning_effort(
                session_id,
                task_id,
                workspace_id,
                worktree_id,
                execution_environment,
                provider_id.to_string(),
                model_id.to_string(),
                reasoning_effort.map(str::to_string),
                "implementer".to_string(),
                parent_session_id,
                relationship.map(str::to_string),
                None,
            )
            .await
    } else {
        store
            .create_session_with_reasoning_effort(
                task_id,
                workspace_id,
                worktree_id,
                execution_environment,
                provider_id.to_string(),
                model_id.to_string(),
                reasoning_effort.map(str::to_string),
                "implementer".to_string(),
                parent_session_id,
                relationship.map(str::to_string),
                None,
            )
            .await
    };

    match result {
        Ok(session) => Ok(session),
        Err(error) => {
            cleanup.cleanup_orphaned_worktree().await;
            Err(TaskSessionCreateError::Internal(error))
        }
    }
}

pub(super) struct CreatedWorktreeCleanup<'a> {
    pub(super) handles: &'a TaskSessionHandles,
    pub(super) store: &'a Store,
    pub(super) workspace: &'a Workspace,
    pub(super) task_id: TaskId,
    pub(super) created_worktree_id: Option<WorktreeId>,
}

impl CreatedWorktreeCleanup<'_> {
    async fn cleanup_orphaned_worktree(&self) {
        if let Some(created_worktree_id) = self.created_worktree_id {
            cleanup_orphaned_provisioned_worktree(
                self.handles,
                self.store,
                self.workspace,
                self.task_id,
                created_worktree_id,
            )
            .await;
        }
    }
}
