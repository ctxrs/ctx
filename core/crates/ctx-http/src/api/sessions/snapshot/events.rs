use super::*;

pub(crate) async fn get_session_events(
    State(state): State<SessionReadModelsHandle>,
    Path(id): Path<String>,
    Query(q): Query<SessionEventsRouteQuery>,
) -> Result<Json<SessionEventsRouteResponse>, StatusCode> {
    state
        .list_session_events_page_for_route(SessionRouteParams::new(id), q)
        .await
        .map(Json)
        .map_err(session_read_model_status)
}
