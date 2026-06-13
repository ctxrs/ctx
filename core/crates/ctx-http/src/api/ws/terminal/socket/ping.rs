use std::time::Duration;

use axum::extract::ws::Message as WsMessage;
use tokio::sync::mpsc;

use super::super::queue::{queue_terminal_ws_message, TerminalWsQueueOutcome};

pub(super) async fn send_terminal_pings(event_tx: mpsc::Sender<WsMessage>, interval: Duration) {
    let mut interval = tokio::time::interval(interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        if matches!(
            queue_terminal_ws_message(&event_tx, WsMessage::Ping(Vec::new())),
            TerminalWsQueueOutcome::Closed
        ) {
            break;
        }
    }
}
