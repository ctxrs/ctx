use super::*;

mod attachment_routes;
mod file_completions;
mod prompt_config;
mod provider_model_preferences;
mod worktree_bootstrap;

pub(in crate::api) use attachment_routes::{
    create_workspace_attachment, delete_workspace_attachment,
};
pub(in crate::api) use file_completions::workspace_file_completions;
pub(in crate::api) use prompt_config::*;
pub(in crate::api) use provider_model_preferences::*;
pub(in crate::api) use worktree_bootstrap::{
    get_worktree_bootstrap_config, update_worktree_bootstrap_config,
};

pub(in crate::api) async fn update_merge_queue_config(
    State(merge_queue_config): State<WorkspaceMergeQueueConfigHandle>,
    Path(id): Path<String>,
    Json(req): Json<UpdateWorkspaceMergeQueueConfigRequest>,
) -> Result<Json<WorkspaceConfigUpdateResult>, (StatusCode, Json<ApiErrorResp>)> {
    merge_queue_config
        .update_workspace_merge_queue_config_for_route_params(WorkspaceRouteParams::new(id), req)
        .await
        .map_err(workspace_route_api_error)
        .map(Json)
}

pub(in crate::api) async fn get_merge_queue_config(
    State(merge_queue_config): State<WorkspaceMergeQueueConfigHandle>,
    Path(id): Path<String>,
) -> Result<Json<WorkspaceMergeQueueConfigRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    merge_queue_config
        .workspace_merge_queue_config_for_route_params(WorkspaceRouteParams::new(id))
        .await
        .map_err(workspace_route_api_error)
        .map(Json)
}

pub(in crate::api) async fn get_workspace_primary_branch(
    State(primary_branch): State<WorkspacePrimaryBranchHandle>,
    Path(id): Path<String>,
) -> Result<Json<WorkspacePrimaryBranchSnapshot>, (StatusCode, Json<ApiErrorResp>)> {
    primary_branch
        .workspace_primary_branch_for_route_params(WorkspaceRouteParams::new(id))
        .await
        .map_err(workspace_route_api_error)
        .map(Json)
}

pub(in crate::api) async fn update_workspace_primary_branch(
    State(primary_branch): State<WorkspacePrimaryBranchHandle>,
    Path(id): Path<String>,
    Json(req): Json<UpdateWorkspacePrimaryBranchRequest>,
) -> Result<Json<WorkspacePrimaryBranchSnapshot>, (StatusCode, Json<ApiErrorResp>)> {
    primary_branch
        .update_workspace_primary_branch_for_route_params(WorkspaceRouteParams::new(id), req)
        .await
        .map_err(workspace_route_api_error)
        .map(Json)
}

pub(in crate::api) async fn get_execution_config(
    State(execution_config): State<WorkspaceExecutionConfigHandle>,
    Path(id): Path<String>,
) -> Result<Json<WorkspaceExecutionConfigRouteSnapshot>, (StatusCode, Json<ApiErrorResp>)> {
    execution_config
        .workspace_execution_config_for_route_params(WorkspaceRouteParams::new(id))
        .await
        .map_err(workspace_route_api_error)
        .map(Json)
}

pub(in crate::api) async fn update_execution_config(
    State(execution_config): State<WorkspaceExecutionConfigHandle>,
    Path(id): Path<String>,
    request: Request,
) -> Result<Json<WorkspaceConfigUpdateResult>, (StatusCode, Json<ApiErrorResp>)> {
    let params = WorkspaceRouteParams::new(id);
    execution_config
        .workspace_execution_config_update_target_for_route_params(&params)
        .await
        .map_err(workspace_route_api_error)?;
    let req: UpdateWorkspaceExecutionConfigRequest = parse_json_request(request).await?;
    execution_config
        .update_workspace_execution_config_for_route_params(params, req)
        .await
        .map_err(workspace_route_api_error)
        .map(Json)
}
