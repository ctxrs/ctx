use anyhow::anyhow;
use ctx_core::ids::{TaskId, WorkspaceId};
use ctx_core::models::Task;
use ctx_core::redaction;
use ctx_store::Store;

#[derive(Debug, Clone)]
pub struct CreateTaskRecordInput {
    pub task_id: Option<TaskId>,
    pub title: String,
    pub description: Option<String>,
}

#[derive(Debug)]
pub enum TaskRecordCreateError {
    NotFound(String),
    Conflict(String),
    Internal(anyhow::Error),
}

#[derive(Debug)]
pub struct PersistedTaskRecord {
    pub task: Task,
    pub created_in_this_request: bool,
}

pub async fn load_existing_task_record_for_request(
    store: &Store,
    workspace_id: WorkspaceId,
    indexed_workspace_id: Option<WorkspaceId>,
    request: &CreateTaskRecordInput,
) -> Result<Option<Task>, TaskRecordCreateError> {
    let Some(task_id) = request.task_id else {
        return Ok(None);
    };
    let Some(indexed_workspace_id) = indexed_workspace_id else {
        return Ok(None);
    };
    if indexed_workspace_id != workspace_id {
        return Err(task_id_conflict());
    }
    let existing = store
        .get_task(task_id)
        .await
        .map_err(internal_store_error)?;
    let Some(existing) = existing else {
        return Err(TaskRecordCreateError::Internal(anyhow!(
            "task index exists but task missing"
        )));
    };
    validate_requested_task_identity(&existing, workspace_id, request)?;
    Ok(Some(existing))
}

pub async fn persist_task_record_for_request(
    store: &Store,
    workspace_id: WorkspaceId,
    existing_task: Option<Task>,
    request: &CreateTaskRecordInput,
) -> Result<PersistedTaskRecord, TaskRecordCreateError> {
    let (task, created_in_this_request) = match existing_task {
        Some(existing) => (existing, false),
        None => match request.task_id {
            Some(task_id) => {
                let result = store
                    .create_task_with_id_result(
                        workspace_id,
                        task_id,
                        request.title.clone(),
                        request.description.clone(),
                    )
                    .await
                    .map_err(internal_store_error)?;
                (result.task, result.created)
            }
            None => (
                store
                    .create_task(
                        workspace_id,
                        request.title.clone(),
                        request.description.clone(),
                    )
                    .await
                    .map_err(internal_store_error)?,
                true,
            ),
        },
    };
    validate_requested_task_identity(&task, workspace_id, request)?;
    Ok(PersistedTaskRecord {
        task,
        created_in_this_request,
    })
}

pub async fn reload_or_retry_task_record_for_request(
    store: &Store,
    workspace_id: WorkspaceId,
    persisted: PersistedTaskRecord,
    request: &CreateTaskRecordInput,
) -> Result<PersistedTaskRecord, TaskRecordCreateError> {
    let task = store
        .get_task_with_activity(persisted.task.id)
        .await
        .map_err(internal_store_error)?;
    if let Some(task) = task {
        return Ok(PersistedTaskRecord {
            task,
            created_in_this_request: persisted.created_in_this_request,
        });
    }
    if persisted.created_in_this_request {
        return Err(task_not_found());
    }
    let Some(task_id) = request.task_id else {
        return Err(task_not_found());
    };
    let retry = store
        .create_task_with_id_result(
            workspace_id,
            task_id,
            request.title.clone(),
            request.description.clone(),
        )
        .await
        .map_err(internal_store_error)?;
    if !retry.created {
        validate_requested_task_identity(&retry.task, workspace_id, request)?;
    }
    Ok(PersistedTaskRecord {
        task: retry.task,
        created_in_this_request: retry.created,
    })
}

fn validate_requested_task_identity(
    task: &Task,
    workspace_id: WorkspaceId,
    request: &CreateTaskRecordInput,
) -> Result<(), TaskRecordCreateError> {
    if request.task_id.is_none() {
        return Ok(());
    }
    if task.workspace_id != workspace_id || !task_request_matches(task, request) {
        return Err(task_id_conflict());
    }
    Ok(())
}

fn task_request_matches(task: &Task, request: &CreateTaskRecordInput) -> bool {
    task.title == request.title && task.description.as_deref() == request.description.as_deref()
}

fn internal_store_error(error: impl std::fmt::Display) -> TaskRecordCreateError {
    TaskRecordCreateError::Internal(anyhow!(
        "{}",
        redaction::redact_sensitive(&error.to_string())
    ))
}

fn task_id_conflict() -> TaskRecordCreateError {
    TaskRecordCreateError::Conflict("task id already exists".to_string())
}

fn task_not_found() -> TaskRecordCreateError {
    TaskRecordCreateError::NotFound("task not found".to_string())
}
