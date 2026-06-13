use super::persistence::{persist_created_session, PersistCreatedSession};
use super::*;

#[path = "loaded/prepared.rs"]
mod prepared;

use prepared::{prepare_loaded_session_request, PreparedLoadedSessionRequest};

pub(super) async fn create_session_for_loaded_task_inner(
    handles: &TaskSessionHandles,
    store: Store,
    task: Task,
    workspace: Workspace,
    input: CreateTaskSessionInput,
) -> Result<Session, TaskSessionCreateError> {
    let PreparedLoadedSessionRequest {
        run_id_header,
        provider_id,
        session_id,
        parent_session_id,
        relationship,
        requested_relationship,
        worktree_id,
        created_worktree_id,
        execution_environment,
        model_id,
        reasoning_effort,
        preferred_model_id,
    } = prepare_loaded_session_request(handles, &store, &task, &workspace, &input).await?;

    if let Some(existing) = resolve_existing_requested_session(ExistingRequestedSession {
        handles,
        store: &store,
        task: &task,
        workspace: &workspace,
        requested_session_id: session_id,
        created_worktree_id,
        identity: SessionCreationIdentity {
            task_id: task.id,
            workspace_id: task.workspace_id,
            worktree_id,
            execution_environment,
            provider_id: &provider_id,
            model_id: &model_id,
            reasoning_effort: reasoning_effort.as_deref(),
            parent_session_id,
            relationship: relationship.as_deref(),
        },
        remember_model_preference: input.remember_model_preference,
        preferred_model_id: &preferred_model_id,
    })
    .await?
    {
        return Ok(existing);
    }

    if let Ok(Some(worktree)) = store.get_worktree(worktree_id).await {
        if let Err(e) = handles
            .admission
            .ensure_task_commit_hook(&workspace, &worktree, task.id)
            .await
        {
            tracing::warn!(
                task_id = %task.id.0,
                worktree_id = %worktree.id.0,
                "failed to configure vcs hooks: {e:#}"
            );
        }
    }

    let session = persist_created_session(PersistCreatedSession {
        handles,
        store: &store,
        task: &task,
        workspace: &workspace,
        requested_session_id: session_id,
        created_worktree_id,
        worktree_id,
        execution_environment,
        provider_id: &provider_id,
        model_id: &model_id,
        reasoning_effort: reasoning_effort.as_deref(),
        parent_session_id,
        relationship: relationship.as_deref(),
        requested_relationship: requested_relationship.as_deref(),
    })
    .await?;

    seed_initial_prompt(
        handles,
        &store,
        &session,
        InitialPromptSeed {
            prompt: input.initial_prompt,
            message_id: input.initial_message_id,
            turn_id: input.initial_turn_id,
            run_id_header: run_id_header.clone(),
        },
    )
    .await?;

    if input.remember_model_preference {
        if let Err(error) = handles
            .admission
            .update_workspace_provider_preferred_model_id(
                task.workspace_id,
                &provider_id,
                Some(preferred_model_id),
            )
            .await
        {
            tracing::warn!(
                session_id = %session.id.0,
                workspace_id = %task.workspace_id.0,
                provider_id = provider_id,
                "failed to persist workspace provider model preference: {error:#}"
            );
        }
    }

    emit_session_started_observability(handles, &session, &task).await;

    Ok(session)
}
