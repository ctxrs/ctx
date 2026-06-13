use super::*;
use ctx_daemon::daemon::TerminalRouteHandle;
use ctx_route_contracts::terminals::{
    TerminalRouteError, TerminalRouteErrorKind, TerminalStreamRouteParams,
};

mod queue;
mod socket;

#[cfg(test)]
pub(super) use queue::{
    queue_terminal_ws_message, queue_terminal_ws_tail_resync_if_requested, TerminalWsQueueOutcome,
};

pub(in crate::api) async fn terminal_stream_ws(
    State(state): State<TerminalRouteHandle>,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    ws: WebSocketUpgrade,
) -> Result<Response, StatusCode> {
    let admission = state
        .admit_terminal_stream_for_route(TerminalStreamRouteParams::new(
            id,
            params.get("token").cloned(),
            params.get("tail").cloned(),
        ))
        .await
        .map_err(terminal_stream_route_status)?;

    let session = admission.session;
    let tail_bytes = admission.tail_bytes;
    Ok(ws.on_upgrade(move |socket| async move {
        socket::handle_terminal_socket(socket, session, tail_bytes).await;
    }))
}

fn terminal_stream_route_status(error: TerminalRouteError) -> StatusCode {
    match error.kind() {
        TerminalRouteErrorKind::BadRequest => StatusCode::BAD_REQUEST,
        TerminalRouteErrorKind::Unauthorized => StatusCode::UNAUTHORIZED,
        TerminalRouteErrorKind::NotFound => StatusCode::NOT_FOUND,
        TerminalRouteErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
