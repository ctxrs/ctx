use super::*;

pub(super) async fn load_create_task_workspace(
    handles: &TaskCreationHandles,
    workspace_id: WorkspaceId,
) -> Result<(Workspace, Store), CreateTaskApiError> {
    let ctx = handles
        .creation
        .load_workspace_context(workspace_id)
        .await
        .map_err(TaskCreateError::internal)?
        .ok_or_else(|| TaskCreateError::NotFound("workspace not found".to_string()))?;
    Ok((ctx.workspace, ctx.store))
}
