use super::*;

pub(in crate::api) async fn delete_workspace(
    State(workspaces): State<WorkspaceDeletionHandle>,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    workspaces
        .delete_workspace_for_route(WorkspaceRouteParams::new(id))
        .await
        .map_err(|error| workspace_route_status(&error))?;
    Ok(StatusCode::NO_CONTENT)
}
