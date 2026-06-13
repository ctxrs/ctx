use super::*;
use ctx_route_contracts::workspaces::CreateWorkspaceRequest;

pub(in crate::api) async fn create_workspace(
    State(registry): State<WorkspaceRegistryHandle>,
    Json(req): Json<CreateWorkspaceRequest>,
) -> Result<Json<WorkspaceRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    registry
        .create_workspace_for_request(req)
        .await
        .map(Json)
        .map_err(workspace_route_api_error)
}
