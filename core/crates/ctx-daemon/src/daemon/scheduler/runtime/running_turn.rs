use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant as TokioInstant;

use ctx_core::ids::{MessageId, RunId, TurnId};
use ctx_core::models::Session;
use ctx_providers::adapters::{ProviderAdapter, RunHandle};
use ctx_providers::events::NormalizedEvent;

use crate::daemon::scheduler::lifecycle::{RunningTurn, TurnStartProgress};

pub(super) struct RunningTurnParts<'a> {
    pub(super) adapter: Arc<dyn ProviderAdapter>,
    pub(super) handle: RunHandle,
    pub(super) run_id: RunId,
    pub(super) turn_id: TurnId,
    pub(super) message_id: MessageId,
    pub(super) session: &'a Session,
    pub(super) full_model_id: &'a str,
    pub(super) execution_environment_label: &'a str,
    pub(super) session_root_kind: &'a str,
    pub(super) event_tx: mpsc::Sender<NormalizedEvent>,
    pub(super) events_done_rx: oneshot::Receiver<()>,
    pub(super) start_progress_rx: watch::Receiver<TurnStartProgress>,
    pub(super) start_deadline_duration: Duration,
    pub(super) mcp_token: Option<String>,
}

pub(super) fn build_running_turn(parts: RunningTurnParts<'_>) -> RunningTurn {
    RunningTurn {
        adapter: parts.adapter,
        handle: parts.handle,
        run_id: parts.run_id,
        turn_id: parts.turn_id,
        message_id: parts.message_id,
        provider_id: parts.session.provider_id.clone(),
        model_id: parts.full_model_id.to_string(),
        execution_environment_label: parts.execution_environment_label.to_string(),
        session_root_kind: parts.session_root_kind.to_string(),
        event_tx: parts.event_tx,
        events_done: Some(parts.events_done_rx),
        start_progress: parts.start_progress_rx,
        start_deadline: TokioInstant::now() + parts.start_deadline_duration,
        mcp_token: parts.mcp_token,
    }
}
