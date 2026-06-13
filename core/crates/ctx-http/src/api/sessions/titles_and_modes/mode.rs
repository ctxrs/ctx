use super::*;
use ctx_daemon::daemon::SessionTitleModelModeHandle;

pub(crate) async fn set_session_mode(
    State(state): State<SessionTitleModelModeHandle>,
    Path(id): Path<String>,
    Json(req): Json<SetSessionModeRouteRequest>,
) -> Result<StatusCode, StatusCode> {
    state
        .set_session_mode_for_route(SessionRouteParams::new(id), req)
        .await
        .map_err(session_title_model_mode_bare_status)?;

    Ok(StatusCode::OK)
}
