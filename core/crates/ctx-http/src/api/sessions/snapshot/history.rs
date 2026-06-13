use super::*;

pub(crate) async fn get_session_history(
    State(state): State<SessionReadModelsHandle>,
    Path(id): Path<String>,
    Query(q): Query<SessionHistoryRouteQuery>,
) -> Result<Json<SessionHistoryRouteResponse>, StatusCode> {
    state
        .load_session_history_page_for_route(SessionRouteParams::new(id), q)
        .await
        .map(Json)
        .map_err(session_read_model_status)
}

pub(crate) async fn list_session_turn_tools(
    State(state): State<SessionReadModelsHandle>,
    Path((id, turn_id)): Path<(String, String)>,
) -> Result<Json<SessionTurnToolsRouteResponse>, StatusCode> {
    state
        .list_session_turn_tools_for_route(SessionTurnToolsRouteParams::new(id, turn_id))
        .await
        .map(Json)
        .map_err(session_read_model_status)
}
