use ctx_core::ids::TaskId;
use ctx_core::models::Workspace;
use ctx_store::Store;

use super::*;

pub(super) async fn rollback_new_task_after_default_session_failure(
    handles: &TaskCreationHandles,
    store: &Store,
    workspace: &Workspace,
    task_id: TaskId,
) {
    let task = match store.get_task(task_id).await {
        Ok(Some(task)) => task,
        Ok(None) => return,
        Err(err) => {
            tracing::warn!(
                task_id = %task_id.0,
                "failed to load task while rolling back brand-new task: {err:#}"
            );
            return;
        }
    };
    match handles
        .creation
        .delete_loaded_task_with_cleanup(store, workspace, &task)
        .await
    {
        Ok(()) | Err(crate::daemon::tasks::TaskLifecycleError::NotFound) => {}
        Err(error) => {
            tracing::warn!(
                task_id = %task_id.0,
                ?error,
                "failed to rollback brand-new task after default-session creation failure"
            );
        }
    }
}

pub(super) async fn emit_task_upsert(handles: &TaskCreationHandles, task_id: TaskId) {
    if let Err(e) = handles
        .session_admission
        .emit_workspace_task_upsert(task_id)
        .await
    {
        tracing::warn!(task_id = %task_id.0, "workspace active snapshot refresh failed: {e:?}");
    }
}
