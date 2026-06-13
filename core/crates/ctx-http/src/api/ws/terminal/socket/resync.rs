use std::sync::{atomic::AtomicBool, Arc};
use std::time::Duration;

use axum::extract::ws::Message as WsMessage;
use ctx_transport_runtime::terminals::TerminalStreamSession;
use tokio::sync::mpsc;

use super::super::queue::{queue_terminal_ws_tail_resync_if_requested, TerminalWsQueueOutcome};

pub(super) async fn resync_terminal_tail_when_requested(
    event_tx: mpsc::Sender<WsMessage>,
    session: Arc<TerminalStreamSession>,
    snapshot_tail: usize,
    needs_tail_resync: Arc<AtomicBool>,
    interval: Duration,
) {
    let mut interval = tokio::time::interval(interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        match queue_terminal_ws_tail_resync_if_requested(
            &event_tx,
            || session.output_tail(snapshot_tail),
            needs_tail_resync.as_ref(),
        ) {
            Some(TerminalWsQueueOutcome::Dropped) => {
                tracing::debug!("dropping terminal tail resync for slow websocket consumer");
            }
            Some(TerminalWsQueueOutcome::Closed) => break,
            Some(TerminalWsQueueOutcome::Enqueued) | None => {}
        }
    }
}
