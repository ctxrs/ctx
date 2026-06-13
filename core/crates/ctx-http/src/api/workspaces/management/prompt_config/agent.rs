use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;

use crate::api::errors::ApiErrorResp;
use crate::api::workspaces::{
    workspace_route_api_error, AgentSystemPromptConfigRouteResponse,
    UpdateAgentSystemPromptConfigRouteRequest, WorkspacePromptBootstrapConfigHandle,
    WorkspacePromptConfigRouteParams,
};

pub(in crate::api) async fn get_agent_system_prompt(
    State(config): State<WorkspacePromptBootstrapConfigHandle>,
    Path(id): Path<String>,
) -> Result<Json<AgentSystemPromptConfigRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    config
        .agent_system_prompt_config_for_route(WorkspacePromptConfigRouteParams::new(id))
        .await
        .map_err(workspace_route_api_error)
        .map(Json)
}

pub(in crate::api) async fn update_agent_system_prompt(
    State(config): State<WorkspacePromptBootstrapConfigHandle>,
    Path(id): Path<String>,
    Json(req): Json<UpdateAgentSystemPromptConfigRouteRequest>,
) -> Result<Json<AgentSystemPromptConfigRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    config
        .update_agent_system_prompt_config_for_route(WorkspacePromptConfigRouteParams::new(id), req)
        .await
        .map_err(workspace_route_api_error)
        .map(Json)
}
