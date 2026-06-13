use tokio::sync::{mpsc, oneshot, watch};

use ctx_providers::events::NormalizedEvent;

use crate::daemon::scheduler::TurnStartProgress;

pub(super) struct TurnRuntimeChannels {
    pub(super) ev_tx: mpsc::Sender<NormalizedEvent>,
    pub(super) ev_rx: mpsc::Receiver<NormalizedEvent>,
    pub(super) events_done_tx: oneshot::Sender<()>,
    pub(super) events_done_rx: oneshot::Receiver<()>,
    pub(super) start_progress_tx: watch::Sender<TurnStartProgress>,
    pub(super) start_progress_rx: watch::Receiver<TurnStartProgress>,
}

impl TurnRuntimeChannels {
    pub(super) fn new() -> Self {
        let (ev_tx, ev_rx) = mpsc::channel::<NormalizedEvent>(128);
        let (events_done_tx, events_done_rx) = oneshot::channel();
        let (start_progress_tx, start_progress_rx) = watch::channel(TurnStartProgress::Pending);
        Self {
            ev_tx,
            ev_rx,
            events_done_tx,
            events_done_rx,
            start_progress_tx,
            start_progress_rx,
        }
    }
}
