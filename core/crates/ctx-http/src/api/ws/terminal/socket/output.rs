use std::sync::{atomic::AtomicBool, Arc};

use axum::extract::ws::Message as WsMessage;
use ctx_transport_runtime::terminals::{
    TerminalStreamOutputReceiver, TerminalStreamOutputRecv, TerminalStreamSession,
};
use tokio::sync::mpsc;

use super::super::queue::{
    queue_terminal_ws_message, queue_terminal_ws_tail_resync_if_requested,
    queue_terminal_ws_tail_snapshot, request_terminal_ws_tail_resync, TerminalWsQueueOutcome,
};

pub(super) async fn forward_terminal_output(
    mut output_rx: TerminalStreamOutputReceiver,
    event_tx: mpsc::Sender<WsMessage>,
    session: Arc<TerminalStreamSession>,
    snapshot_tail: usize,
    needs_tail_resync: Arc<AtomicBool>,
) {
    loop {
        match output_rx.recv().await {
            TerminalStreamOutputRecv::Bytes(bytes) => {
                if let Some(outcome) = queue_terminal_ws_tail_resync_if_requested(
                    &event_tx,
                    || session.output_tail(snapshot_tail),
                    needs_tail_resync.as_ref(),
                ) {
                    match outcome {
                        TerminalWsQueueOutcome::Enqueued => continue,
                        TerminalWsQueueOutcome::Dropped => {
                            tracing::debug!(
                                "dropping terminal tail resync for slow websocket consumer"
                            );
                            continue;
                        }
                        TerminalWsQueueOutcome::Closed => break,
                    }
                }
                match queue_terminal_ws_message(&event_tx, WsMessage::Binary(bytes)) {
                    TerminalWsQueueOutcome::Enqueued => {}
                    TerminalWsQueueOutcome::Dropped => {
                        request_terminal_ws_tail_resync(needs_tail_resync.as_ref());
                        tracing::debug!("dropping terminal output for slow websocket consumer");
                    }
                    TerminalWsQueueOutcome::Closed => break,
                }
            }
            TerminalStreamOutputRecv::Lagged => {
                match queue_terminal_ws_tail_snapshot(&event_tx, session.output_tail(snapshot_tail))
                {
                    TerminalWsQueueOutcome::Enqueued => {}
                    TerminalWsQueueOutcome::Dropped => {
                        request_terminal_ws_tail_resync(needs_tail_resync.as_ref());
                        tracing::debug!(
                            "dropping terminal tail resync for slow websocket consumer"
                        );
                    }
                    TerminalWsQueueOutcome::Closed => break,
                }
                continue;
            }
            TerminalStreamOutputRecv::Closed => break,
        }
    }
}
