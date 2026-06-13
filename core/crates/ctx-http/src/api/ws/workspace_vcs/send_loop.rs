use std::sync::Arc;

use axum::extract::ws::{Message as WsMessage, WebSocket};
use futures::SinkExt;

use super::buffer::VcsPendingBuffer;
use super::metrics::VcsStreamMetrics;

pub(super) fn spawn_workspace_vcs_send_loop(
    sender: futures::stream::SplitSink<WebSocket, WsMessage>,
    pending: Arc<VcsPendingBuffer>,
    metrics: Arc<VcsStreamMetrics>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut sender = sender;
        loop {
            if let Some(message) = pending.pop().await {
                let Ok(text) = serde_json::to_string(&message) else {
                    break;
                };
                if sender.send(WsMessage::Text(text)).await.is_err() {
                    break;
                }
                metrics.message_sent(&message);
                continue;
            }
            pending.wait_for_message().await;
        }
    })
}
