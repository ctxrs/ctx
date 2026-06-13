use super::*;

pub(crate) async fn get_session_head(
    State(state): State<SessionReadModelsHandle>,
    Path(id): Path<String>,
    Query(q): Query<SessionHeadRouteQuery>,
) -> Result<Json<SessionHeadRouteResponse>, StatusCode> {
    state
        .session_head_for_route(SessionRouteParams::new(id), q)
        .await
        .map(Json)
        .map_err(session_read_model_status)
}
