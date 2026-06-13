use super::*;
use ctx_daemon::daemon::SessionTitleModelModeHandle;

pub(crate) async fn set_session_model(
    State(state): State<SessionTitleModelModeHandle>,
    Path(id): Path<String>,
    Json(req): Json<SetSessionModelRouteRequest>,
) -> Result<Json<SetSessionModelRouteResponse>, ApiErr> {
    state
        .set_session_model_for_route(SessionRouteParams::new(id), req)
        .await
        .map(Json)
        .map_err(session_title_model_mode_api_error)
}
