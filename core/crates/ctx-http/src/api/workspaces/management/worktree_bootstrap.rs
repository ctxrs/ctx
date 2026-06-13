use super::*;

pub(in crate::api) async fn get_worktree_bootstrap_config(
    State(config): State<WorkspacePromptBootstrapConfigHandle>,
    Path(id): Path<String>,
) -> Result<Json<WorkspaceWorktreeBootstrapConfigRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    config
        .worktree_bootstrap_config_for_route_params(WorkspaceRouteParams::new(id))
        .await
        .map_err(workspace_route_api_error)
        .map(Json)
}

pub(in crate::api) async fn update_worktree_bootstrap_config(
    State(config): State<WorkspacePromptBootstrapConfigHandle>,
    Path(id): Path<String>,
    Json(req): Json<UpdateWorktreeBootstrapConfigRequest>,
) -> Result<Json<WorkspaceConfigUpdateResult>, (StatusCode, Json<ApiErrorResp>)> {
    config
        .update_worktree_bootstrap_config_for_route_params(WorkspaceRouteParams::new(id), req)
        .await
        .map_err(workspace_route_api_error)
        .map(Json)
}
