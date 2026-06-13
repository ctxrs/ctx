use super::*;
use axum::extract::Extension;

pub(crate) async fn mcp_spawn_agent(
    State(state): State<SessionSubagentMcpControlHandle>,
    mcp_auth: Option<Extension<ctx_mcp_auth::McpAuthContext>>,
    Path(id): Path<String>,
    Json(req): Json<SpawnAgentRouteRequest>,
) -> Result<Json<SpawnAgentRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    state
        .spawn_agent_for_mcp_route(
            SessionRouteParams::new(id),
            mcp_auth.map(|Extension(auth)| auth),
            req,
        )
        .await
        .map_err(subagent_api_error)
        .map(Json)
}
