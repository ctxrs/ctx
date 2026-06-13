use std::sync::atomic::{AtomicBool, Ordering};

use axum::extract::ws::Message as WsMessage;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::api::ws) enum TerminalWsQueueOutcome {
    Enqueued,
    Dropped,
    Closed,
}

pub(in crate::api::ws) fn queue_terminal_ws_message(
    event_tx: &tokio::sync::mpsc::Sender<WsMessage>,
    msg: WsMessage,
) -> TerminalWsQueueOutcome {
    match event_tx.try_send(msg) {
        Ok(()) => TerminalWsQueueOutcome::Enqueued,
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => TerminalWsQueueOutcome::Dropped,
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => TerminalWsQueueOutcome::Closed,
    }
}

pub(super) fn queue_terminal_ws_tail_snapshot(
    event_tx: &tokio::sync::mpsc::Sender<WsMessage>,
    snapshot: Vec<u8>,
) -> TerminalWsQueueOutcome {
    if snapshot.is_empty() {
        return TerminalWsQueueOutcome::Enqueued;
    }
    queue_terminal_ws_message(event_tx, WsMessage::Binary(snapshot))
}

pub(super) fn request_terminal_ws_tail_resync(needs_tail_resync: &AtomicBool) {
    needs_tail_resync.store(true, Ordering::Release);
}

pub(in crate::api::ws) fn queue_terminal_ws_tail_resync_if_requested(
    event_tx: &tokio::sync::mpsc::Sender<WsMessage>,
    tail_snapshot: impl FnOnce() -> Vec<u8>,
    needs_tail_resync: &AtomicBool,
) -> Option<TerminalWsQueueOutcome> {
    if !needs_tail_resync.swap(false, Ordering::AcqRel) {
        return None;
    }
    let outcome = queue_terminal_ws_tail_snapshot(event_tx, tail_snapshot());
    if matches!(outcome, TerminalWsQueueOutcome::Dropped) {
        request_terminal_ws_tail_resync(needs_tail_resync);
    }
    Some(outcome)
}
