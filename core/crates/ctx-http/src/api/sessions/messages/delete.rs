use super::*;

pub(crate) async fn delete_session_message(
    State(state): State<SessionMessageCommandHandle>,
    Path((session_id, id)): Path<(String, String)>,
) -> Result<StatusCode, StatusCode> {
    state
        .delete_session_message_for_route(DeleteSessionMessageRouteParams::new(session_id, id))
        .await
        .map_err(session_message_bare_status)?;
    Ok(StatusCode::NO_CONTENT)
}
