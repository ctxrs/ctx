use super::*;

mod create;
mod delete;

pub(in crate::api) use create::create_workspace;
pub(in crate::api) use delete::delete_workspace;

pub(in crate::api) async fn list_workspaces(
    State(registry): State<WorkspaceRegistryHandle>,
) -> Result<Json<Vec<WorkspaceRouteResponse>>, StatusCode> {
    registry
        .list_workspaces_for_route()
        .await
        .map(Json)
        .map_err(|error| workspace_route_status(&error))
}

pub(in crate::api) async fn get_workspace(
    State(registry): State<WorkspaceRegistryHandle>,
    Path(id): Path<String>,
) -> Result<Json<WorkspaceRouteResponse>, StatusCode> {
    registry
        .get_workspace_for_route_params(WorkspaceRouteParams::new(id))
        .await
        .map(Json)
        .map_err(|error| workspace_route_status(&error))
}
