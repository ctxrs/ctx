use anyhow::Result;
use ctx_core::ids::{TaskId, WorkspaceId};
use ctx_core::models::{
    Session, Task, WorkspaceArchivedPage, WorkspaceIndexCursor, WorkspaceTaskSummary,
};
use ctx_store::Store;

pub async fn list_workspace_tasks(store: &Store, workspace_id: WorkspaceId) -> Result<Vec<Task>> {
    store.list_tasks(workspace_id).await
}

pub async fn list_workspace_archived_page(
    store: &Store,
    workspace_id: WorkspaceId,
    cursor: Option<WorkspaceIndexCursor>,
    limit: i64,
    archived_rev: i64,
) -> Result<WorkspaceArchivedPage> {
    let (tasks, next_cursor): (Vec<WorkspaceTaskSummary>, Option<WorkspaceIndexCursor>) = store
        .list_workspace_archived_page(workspace_id, cursor, limit)
        .await?;
    let (_, total_archived) = store.workspace_task_counts(workspace_id).await?;

    Ok(WorkspaceArchivedPage {
        workspace_id,
        archived_rev,
        tasks,
        next_cursor,
        total_archived,
    })
}

pub async fn list_task_sessions(store: &Store, task_id: TaskId) -> Result<Vec<Session>> {
    store.list_sessions_for_task(task_id).await
}
