use super::*;

pub(crate) async fn get_session_state(
    State(state): State<SessionReadModelsHandle>,
    Path(id): Path<String>,
) -> Result<Json<SessionStateRouteResponse>, StatusCode> {
    state
        .load_session_state_for_route(SessionRouteParams::new(id))
        .await
        .map(Json)
        .map_err(session_read_model_status)
}
