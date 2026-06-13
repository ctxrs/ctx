use super::super::*;

pub(crate) async fn cancel_session(
    State(state): State<SessionControlHandle>,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    state
        .cancel_session_for_route(SessionRouteParams::new(id))
        .await
        .map_err(session_control_bare_status)?;
    Ok(StatusCode::OK)
}

pub(crate) async fn interrupt_session(
    State(state): State<SessionControlHandle>,
    Path(id): Path<String>,
) -> Result<StatusCode, StatusCode> {
    let request_started = std::time::Instant::now();
    state
        .interrupt_session_for_route(SessionRouteParams::new(id), request_started)
        .await
        .map_err(session_control_bare_status)?;
    Ok(StatusCode::OK)
}
