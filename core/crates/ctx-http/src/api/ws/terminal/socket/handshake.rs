use axum::extract::ws::{Message as WsMessage, WebSocket};
use ctx_transport_runtime::terminals::{TerminalServerMessage, TerminalStreamInitialSnapshot};

pub(super) async fn send_initial_terminal_snapshot(
    socket: &mut WebSocket,
    snapshot: &TerminalStreamInitialSnapshot,
) {
    let status_payload = serde_json::to_string(&TerminalServerMessage::Status {
        status: snapshot.status.clone(),
        exit_code: snapshot.exit_code,
    })
    .unwrap_or_else(|_| "{\"type\":\"status\",\"status\":\"running\"}".to_string());
    let _ = socket.send(WsMessage::Text(status_payload)).await;

    if !snapshot.output_tail.is_empty() {
        let _ = socket
            .send(WsMessage::Binary(snapshot.output_tail.clone()))
            .await;
    }
}
