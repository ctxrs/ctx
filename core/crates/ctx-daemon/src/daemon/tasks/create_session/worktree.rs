use super::*;
use create::create_session_execution_worktree;

#[path = "worktree/create.rs"]
mod create;

pub(super) struct SessionWorktreeResolution {
    pub(super) worktree_id: WorktreeId,
    pub(super) created_worktree_id: Option<WorktreeId>,
    pub(super) execution_environment: ExecutionEnvironment,
}

pub(super) async fn resolve_session_worktree_for_task(
    handles: &TaskSessionHandles,
    store: &Store,
    task: &Task,
    workspace: &Workspace,
    requested_worktree_id: Option<&str>,
    requested_execution_environment: Option<ExecutionEnvironment>,
) -> Result<SessionWorktreeResolution, TaskSessionCreateError> {
    let workspace_effective = handles
        .admission
        .effective_execution_settings(workspace.id)
        .await
        .map_err(TaskSessionCreateError::Internal)?;
    let mut existing_worktree = None;
    let worktree_id = if let Some(worktree_id) = requested_worktree_id {
        let worktree_id = WorktreeId(
            uuid::Uuid::parse_str(worktree_id).map_err(|_| TaskSessionCreateError::BadRequest)?,
        );
        existing_worktree = Some(
            handles
                .admission
                .resolve_existing_worktree_execution(store, workspace, worktree_id)
                .await
                .map_err(|_| TaskSessionCreateError::NotFound)?,
        );
        worktree_id
    } else if let Some(primary) = task.primary_worktree_id {
        existing_worktree = Some(
            handles
                .admission
                .resolve_existing_worktree_execution(store, workspace, primary)
                .await
                .map_err(TaskSessionCreateError::Internal)?,
        );
        primary
    } else {
        create_session_execution_worktree(handles, store, task, workspace, &workspace_effective)
            .await?
    };
    let created_worktree_id = existing_worktree.is_none().then_some(worktree_id);
    let execution_environment = if let Some(existing) = existing_worktree.as_ref() {
        let persisted = existing.execution_environment();
        if let Some(requested) = requested_execution_environment {
            if requested != persisted {
                return Err(TaskSessionCreateError::BadRequest);
            }
        }
        persisted
    } else {
        let effective_execution_environment =
            execution_environment_from_settings(&workspace_effective);
        match requested_execution_environment {
            Some(requested) => {
                if requested != effective_execution_environment {
                    if let Some(created_worktree_id) = created_worktree_id {
                        cleanup_orphaned_provisioned_worktree(
                            handles,
                            store,
                            workspace,
                            task.id,
                            created_worktree_id,
                        )
                        .await;
                    }
                    return Err(TaskSessionCreateError::BadRequest);
                }
                requested
            }
            None => effective_execution_environment,
        }
    };

    Ok(SessionWorktreeResolution {
        worktree_id,
        created_worktree_id,
        execution_environment,
    })
}
