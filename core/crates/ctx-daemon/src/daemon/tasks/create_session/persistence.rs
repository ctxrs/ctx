use super::*;
use record::{create_session_record, CreateSessionRecord, CreatedWorktreeCleanup};

#[path = "persistence/record.rs"]
mod record;

pub(super) struct PersistCreatedSession<'a> {
    pub(super) handles: &'a TaskSessionHandles,
    pub(super) store: &'a Store,
    pub(super) task: &'a Task,
    pub(super) workspace: &'a Workspace,
    pub(super) requested_session_id: Option<SessionId>,
    pub(super) created_worktree_id: Option<WorktreeId>,
    pub(super) worktree_id: WorktreeId,
    pub(super) execution_environment: ExecutionEnvironment,
    pub(super) provider_id: &'a str,
    pub(super) model_id: &'a str,
    pub(super) reasoning_effort: Option<&'a str>,
    pub(super) parent_session_id: Option<SessionId>,
    pub(super) relationship: Option<&'a str>,
    pub(super) requested_relationship: Option<&'a str>,
}

pub(super) async fn persist_created_session(
    request: PersistCreatedSession<'_>,
) -> Result<Session, TaskSessionCreateError> {
    let PersistCreatedSession {
        handles,
        store,
        task,
        workspace,
        requested_session_id,
        created_worktree_id,
        worktree_id,
        execution_environment,
        provider_id,
        model_id,
        reasoning_effort,
        parent_session_id,
        relationship,
        requested_relationship,
    } = request;

    let session = create_session_record(CreateSessionRecord {
        store,
        task_id: task.id,
        workspace_id: task.workspace_id,
        requested_session_id,
        worktree_id,
        execution_environment,
        provider_id,
        model_id,
        reasoning_effort,
        parent_session_id,
        relationship,
        cleanup: CreatedWorktreeCleanup {
            handles,
            store,
            workspace,
            task_id: task.id,
            created_worktree_id,
        },
    })
    .await?;

    if let Some(session_id) = requested_session_id {
        if session.id != session_id
            || !session_matches_creation_identity(
                &session,
                SessionCreationIdentity {
                    task_id: task.id,
                    workspace_id: task.workspace_id,
                    worktree_id,
                    execution_environment,
                    provider_id,
                    model_id,
                    reasoning_effort,
                    parent_session_id,
                    relationship: requested_relationship,
                },
            )
        {
            return Err(TaskSessionCreateError::Conflict);
        }
    }

    handles.admission.remember_session_meta(&session).await;
    if let Err(e) = retry_global_index_write(|| async {
        handles
            .admission
            .upsert_workspace_session_index(session.id, task.workspace_id)
            .await
    })
    .await
    {
        tracing::warn!(session_id = %session.id.0, "failed to update session index: {e:?}");
        return Err(TaskSessionCreateError::Internal(e));
    }

    if session.parent_session_id.is_none() && session.relationship.is_none() {
        let _ = store
            .set_task_primary_session(task.id, session.id, worktree_id)
            .await;
    }

    if let Err(error) = store.project_session_to_work(session.id).await {
        tracing::warn!(
            session_id = %session.id.0,
            task_id = %task.id.0,
            workspace_id = %task.workspace_id.0,
            "failed to project created ADE session into Work records: {error:#}"
        );
    }

    Ok(session)
}
