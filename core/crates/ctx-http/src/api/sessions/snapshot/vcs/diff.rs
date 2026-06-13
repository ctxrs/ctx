use super::*;

pub(crate) async fn get_session_diff(
    State(state): State<SessionVcsHandle>,
    Path(id): Path<String>,
    Query(q): Query<SessionVcsRouteQuery>,
) -> Result<Json<SessionVcsDiffRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    state
        .get_session_vcs_diff_for_route(SessionRouteParams::new(id), q)
        .await
        .map(Json)
        .map_err(session_vcs_api_error)
}

pub(crate) async fn get_session_diff_summary(
    State(state): State<SessionVcsHandle>,
    Path(id): Path<String>,
    Query(q): Query<SessionVcsRouteQuery>,
) -> Result<Json<SessionVcsDiffSummaryRouteResponse>, (StatusCode, Json<ApiErrorResp>)> {
    state
        .get_session_vcs_diff_summary_for_route(SessionRouteParams::new(id), q)
        .await
        .map(Json)
        .map_err(session_vcs_api_error)
}
