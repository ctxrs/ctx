use axum::extract::ws::{Message as WsMessage, WebSocket};
use futures::stream::SplitSink;
use futures::SinkExt;

pub(super) async fn forward_terminal_ws_messages(
    mut ws_tx: SplitSink<WebSocket, WsMessage>,
    mut event_rx: tokio::sync::mpsc::Receiver<WsMessage>,
) {
    while let Some(msg) = event_rx.recv().await {
        if ws_tx.send(msg).await.is_err() {
            break;
        }
    }
}
