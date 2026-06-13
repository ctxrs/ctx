use super::*;

pub(crate) async fn apply_session_diff_patch(
    State(state): State<SessionVcsHandle>,
    Path(id): Path<String>,
    Json(req): Json<ApplySessionVcsDiffPatchRouteRequest>,
) -> Result<Json<SessionVcsDiffRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    state
        .apply_session_vcs_diff_patch_for_route(SessionRouteParams::new(id), req)
        .await
        .map(Json)
        .map_err(session_vcs_api_error)
}
