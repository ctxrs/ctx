use super::*;
use ctx_session_service::session_creation::{
    default_session_id_for_existing_primary, DefaultSessionSeed,
};

#[path = "default_session_flow/effects.rs"]
mod effects;

use effects::{emit_task_upsert, rollback_new_task_after_default_session_failure};

pub(super) async fn ensure_default_session_for_task(
    handles: &TaskCreationHandles,
    store: Store,
    workspace: Workspace,
    task: Task,
    requested_default_session: Option<CreateTaskSessionInput>,
    default_session_plan: Option<DefaultSessionPlan>,
    created_task_in_this_request: bool,
) -> Result<Task, TaskCreateError> {
    if let Some(primary_session_id) = task.primary_session_id {
        if let Some(mut default_session_input) = requested_default_session {
            default_session_input.id = Some(default_session_id_for_existing_primary(
                default_session_input.id.as_deref(),
                primary_session_id,
            ));
            handles
                .session_admission
                .create_session_for_loaded_task_locked(
                    store.clone(),
                    task.clone(),
                    workspace.clone(),
                    default_session_input,
                )
                .await
                .map_err(|_| TaskCreateError::DefaultSessionConflict)?;
        }
        emit_task_upsert(handles, task.id).await;
        return Ok(task);
    }

    let default_session_result = if let Some(default_session_input) = requested_default_session {
        handles
            .session_admission
            .create_session_for_loaded_task_locked(
                store.clone(),
                task.clone(),
                workspace.clone(),
                default_session_input,
            )
            .await
    } else {
        let (execution_environment, provider_id, model_id, reasoning_effort) =
            match default_session_plan {
                Some(plan) => plan,
                None => {
                    match preflight_default_session_creation(handles, &store, &workspace).await {
                        Ok(plan) => plan,
                        Err(err) => {
                            if created_task_in_this_request {
                                rollback_new_task_after_default_session_failure(
                                    handles, &store, &workspace, task.id,
                                )
                                .await;
                            }
                            return Err(err);
                        }
                    }
                }
            };
        handles
            .session_admission
            .create_session_for_loaded_task_locked(
                store.clone(),
                task.clone(),
                workspace.clone(),
                crate::daemon::tasks::CreateTaskSessionInput::from_default_seed(
                    DefaultSessionSeed {
                        provider_id,
                        model_id,
                        reasoning_effort,
                        execution_environment,
                    },
                ),
            )
            .await
    };
    if let Err(error) = default_session_result {
        if created_task_in_this_request {
            rollback_new_task_after_default_session_failure(handles, &store, &workspace, task.id)
                .await;
        }
        return Err(TaskCreateError::DefaultSessionFailed(error));
    }

    let task = match store
        .get_task_with_activity(task.id)
        .await
        .map_err(TaskCreateError::internal)?
    {
        Some(task) => task,
        None => {
            return Err(TaskCreateError::NotFound("task not found".to_string()));
        }
    };

    emit_task_upsert(handles, task.id).await;
    Ok(task)
}
