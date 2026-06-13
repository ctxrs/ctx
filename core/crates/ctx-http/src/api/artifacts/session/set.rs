use super::*;
use ctx_daemon::daemon::SessionArtifactsHandle;
use ctx_route_contracts::sessions::SessionRouteParams;
use ctx_session_artifacts::route_contract::{
    SessionArtifactsRouteResponse, SetSessionArtifactsRouteRequest,
};

pub(in crate::api) async fn set_session_artifacts(
    State(state): State<SessionArtifactsHandle>,
    mcp_auth: Option<Extension<ctx_mcp_auth::McpAuthContext>>,
    Path(id): Path<String>,
    Json(req): Json<SetSessionArtifactsRouteRequest>,
) -> Result<Json<SessionArtifactsRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    let artifacts = state
        .set_session_artifacts_for_route_params(
            SessionRouteParams::new(id),
            mcp_auth.map(|Extension(auth)| auth),
            req,
        )
        .await
        .map_err(session_artifact_api_error)?;

    Ok(Json(artifacts))
}
