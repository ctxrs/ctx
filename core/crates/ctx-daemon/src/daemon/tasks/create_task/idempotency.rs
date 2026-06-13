use ctx_task_service::creation::{
    load_existing_task_record_for_request, persist_task_record_for_request,
    reload_or_retry_task_record_for_request, CreateTaskRecordInput, PersistedTaskRecord,
};

use super::*;

pub(super) async fn persist_task_for_request(
    store: &Store,
    ws_id: WorkspaceId,
    existing_task: Option<Task>,
    request: &CreateTaskInput,
) -> Result<PersistedTaskRecord, CreateTaskApiError> {
    let request = task_record_input(request);
    persist_task_record_for_request(store, ws_id, existing_task, &request)
        .await
        .map_err(Into::into)
}

pub(super) async fn upsert_workspace_task_index(
    handles: &TaskCreationHandles,
    task_id: TaskId,
    ws_id: WorkspaceId,
) {
    if let Err(e) = handles
        .creation
        .upsert_workspace_task_index(task_id, ws_id)
        .await
    {
        tracing::warn!(task_id = %task_id.0, "failed to update task index: {e:?}");
    }
}

pub(super) async fn reload_or_retry_task_for_request(
    store: &Store,
    ws_id: WorkspaceId,
    persisted: PersistedTaskRecord,
    request: &CreateTaskInput,
) -> Result<PersistedTaskRecord, CreateTaskApiError> {
    let request = task_record_input(request);
    reload_or_retry_task_record_for_request(store, ws_id, persisted, &request)
        .await
        .map_err(Into::into)
}

pub(super) async fn load_existing_task_for_request(
    handles: &TaskCreationHandles,
    store: &Store,
    ws_id: WorkspaceId,
    request: &CreateTaskInput,
) -> Result<Option<Task>, CreateTaskApiError> {
    let request = task_record_input(request);
    let indexed_workspace_id = match request.task_id {
        Some(task_id) => handles
            .session_admission
            .get_workspace_id_for_task(task_id)
            .await
            .map_err(TaskCreateError::internal)?,
        None => None,
    };
    load_existing_task_record_for_request(store, ws_id, indexed_workspace_id, &request)
        .await
        .map_err(Into::into)
}

fn task_record_input(input: &CreateTaskInput) -> CreateTaskRecordInput {
    CreateTaskRecordInput {
        task_id: input.task_id,
        title: input.title.clone(),
        description: input.description.clone(),
    }
}
