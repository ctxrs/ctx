use super::*;

#[derive(Debug, Serialize)]
pub(crate) struct McpContextResponse {
    session_id: String,
    workspace_id: String,
    worktree_id: String,
    capabilities: Vec<&'static str>,
}

pub(crate) async fn get_mcp_context(
    Extension(mcp_auth): Extension<ctx_mcp_auth::McpAuthContext>,
) -> Json<McpContextResponse> {
    Json(McpContextResponse {
        session_id: mcp_auth.session_id.0.to_string(),
        workspace_id: mcp_auth.workspace_id.0.to_string(),
        worktree_id: mcp_auth.worktree_id.0.to_string(),
        capabilities: mcp_auth.capabilities.names(),
    })
}
