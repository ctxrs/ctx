use super::*;

pub(crate) async fn get_session_git_status(
    State(state): State<SessionVcsHandle>,
    Path(id): Path<String>,
) -> Result<Json<SessionVcsGitStatusRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    state
        .get_session_vcs_git_status_for_route(SessionRouteParams::new(id))
        .await
        .map(Json)
        .map_err(session_vcs_api_error)
}
