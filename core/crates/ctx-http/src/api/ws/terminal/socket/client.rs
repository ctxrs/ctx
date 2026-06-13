use std::sync::Arc;

use axum::extract::ws::{Message as WsMessage, WebSocket};
use ctx_transport_runtime::terminals::{
    TerminalClientMessage, TerminalServerMessage, TerminalStreamSession,
};
use futures::stream::SplitStream;
use futures::StreamExt;
use tokio::sync::mpsc;

use super::super::queue::{queue_terminal_ws_message, TerminalWsQueueOutcome};

pub(super) async fn handle_terminal_client_messages(
    mut ws_rx: SplitStream<WebSocket>,
    session: Arc<TerminalStreamSession>,
    event_tx: mpsc::Sender<WsMessage>,
) {
    while let Some(Ok(msg)) = ws_rx.next().await {
        match msg {
            WsMessage::Binary(data) => {
                session.write_input(data);
            }
            WsMessage::Text(text) => {
                if let Ok(parsed) = serde_json::from_str::<TerminalClientMessage>(&text) {
                    match parsed {
                        TerminalClientMessage::Resize { cols, rows } => {
                            let _ = session.resize_terminal(cols, rows);
                        }
                        TerminalClientMessage::Input { data } => {
                            session.write_input(data.into_bytes());
                        }
                        TerminalClientMessage::Ping => {
                            let payload = serde_json::to_string(&TerminalServerMessage::Pong)
                                .unwrap_or_else(|_| "{\"type\":\"pong\"}".to_string());
                            if matches!(
                                queue_terminal_ws_message(&event_tx, WsMessage::Text(payload)),
                                TerminalWsQueueOutcome::Closed
                            ) {
                                break;
                            }
                        }
                    }
                } else {
                    session.write_input(text.into_bytes());
                }
            }
            WsMessage::Close(_) => break,
            WsMessage::Ping(payload) => {
                if matches!(
                    queue_terminal_ws_message(&event_tx, WsMessage::Pong(payload)),
                    TerminalWsQueueOutcome::Closed
                ) {
                    break;
                }
            }
            WsMessage::Pong(_) => {}
        }
    }
}
