use super::*;

pub(in crate::api) async fn get_workspace_active_snapshot(
    State(workspaces): State<WorkspaceActiveHandle>,
    Path(id): Path<String>,
) -> Result<Json<WorkspaceActiveSnapshotRouteResponse>, StatusCode> {
    let snapshot = workspaces
        .workspace_active_snapshot_for_route(WorkspaceRouteParams::new(id))
        .await
        .map_err(|error| workspace_route_status(&error))?;
    Ok(Json(snapshot))
}

pub(in crate::api) async fn get_workspace_active_heads(
    State(workspaces): State<WorkspaceActiveHandle>,
    Path(id): Path<String>,
) -> Result<Json<WorkspaceActiveHeadBatchRouteResponse>, StatusCode> {
    let heads = workspaces
        .workspace_active_heads_for_route(WorkspaceRouteParams::new(id))
        .await
        .map_err(|error| workspace_route_status(&error))?;
    Ok(Json(heads))
}
