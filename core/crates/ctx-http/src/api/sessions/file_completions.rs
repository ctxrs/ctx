use super::*;

pub(crate) async fn session_file_completions(
    State(state): State<SessionFileCompletionsHandle>,
    Path(id): Path<String>,
    Query(q): Query<SessionFileCompletionsRouteQuery>,
) -> Result<Json<ctx_route_contracts::sessions::SessionFileCompletionsRouteResponse>, StatusCode> {
    state
        .complete_files_for_session_for_route(SessionRouteParams::new(id), q)
        .await
        .map(Json)
        .map_err(session_control_bare_status)
}
