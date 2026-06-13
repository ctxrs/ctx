use super::*;
use axum::extract::Extension;

pub(crate) async fn mcp_send_input(
    State(state): State<SessionSubagentMcpControlHandle>,
    mcp_auth: Option<Extension<ctx_mcp_auth::McpAuthContext>>,
    Path(id): Path<String>,
    Json(req): Json<SendInputRouteRequest>,
) -> Result<Json<SendInputRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    state
        .send_input_for_mcp_route(
            SessionRouteParams::new(id),
            mcp_session_route_context(mcp_auth),
            req,
        )
        .await
        .map_err(subagent_api_error)
        .map(Json)
}

pub(crate) async fn mcp_archive_agent(
    State(state): State<SessionSubagentMcpControlHandle>,
    mcp_auth: Option<Extension<ctx_mcp_auth::McpAuthContext>>,
    Path(id): Path<String>,
    Json(req): Json<ArchiveAgentRouteRequest>,
) -> Result<Json<ArchiveAgentRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    state
        .archive_agent_for_mcp_route(
            SessionRouteParams::new(id),
            mcp_session_route_context(mcp_auth),
            req,
        )
        .await
        .map_err(subagent_api_error)
        .map(Json)
}

pub(crate) async fn mcp_list_agents(
    State(state): State<SessionSubagentMcpReadHandle>,
    mcp_auth: Option<Extension<ctx_mcp_auth::McpAuthContext>>,
    Path(id): Path<String>,
) -> Result<Json<ListAgentsRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    state
        .list_agents_for_mcp_route(
            SessionRouteParams::new(id),
            mcp_session_route_context(mcp_auth),
        )
        .await
        .map_err(subagent_api_error)
        .map(Json)
}

pub(crate) async fn mcp_get_agent(
    State(state): State<SessionSubagentMcpReadHandle>,
    mcp_auth: Option<Extension<ctx_mcp_auth::McpAuthContext>>,
    Path(id): Path<String>,
    Json(req): Json<GetAgentRouteRequest>,
) -> Result<Json<GetAgentRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    state
        .get_agent_for_mcp_route(
            SessionRouteParams::new(id),
            mcp_session_route_context(mcp_auth),
            req,
        )
        .await
        .map_err(subagent_api_error)
        .map(Json)
}

pub(crate) async fn mcp_interrupt_agent(
    State(state): State<SessionSubagentMcpControlHandle>,
    mcp_auth: Option<Extension<ctx_mcp_auth::McpAuthContext>>,
    Path(id): Path<String>,
    Json(req): Json<InterruptAgentRouteRequest>,
) -> Result<Json<InterruptAgentRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    state
        .interrupt_agent_for_mcp_route(
            SessionRouteParams::new(id),
            mcp_session_route_context(mcp_auth),
            req,
        )
        .await
        .map_err(subagent_api_error)
        .map(Json)
}

pub(crate) async fn mcp_wait_agent(
    State(state): State<SessionSubagentMcpReadHandle>,
    mcp_auth: Option<Extension<ctx_mcp_auth::McpAuthContext>>,
    Path(id): Path<String>,
    Json(req): Json<WaitAgentRouteRequest>,
) -> Result<Json<WaitAgentRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    state
        .wait_agent_for_mcp_route(
            SessionRouteParams::new(id),
            mcp_session_route_context(mcp_auth),
            req,
        )
        .await
        .map_err(subagent_api_error)
        .map(Json)
}

fn mcp_session_route_context(
    mcp_auth: Option<Extension<ctx_mcp_auth::McpAuthContext>>,
) -> Option<ctx_mcp_auth::McpAuthContext> {
    mcp_auth.map(|Extension(auth)| auth)
}
