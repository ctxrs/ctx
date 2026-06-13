use std::time::Duration;

use serde_json::json;
use tokio::sync::oneshot;

use ctx_core::ids::{MessageId, RunId, SessionId, TurnId};
use ctx_core::models::{SessionEventType, SessionTurnStatus};
use ctx_providers::adapters::{ProviderTurnOutcome, RunHandle};

use crate::daemon::scheduler::host::WorkerLifecycleHost;

use super::super::persistence::SchedulerPersistenceHost;
use super::super::terminal::{
    finalize_failed_turn_with_host, finalize_provider_outcome_with_host, FailedTurnTerminalization,
};
use super::RunningTurn;

const PROVIDER_OUTCOME_WAIT_TIMEOUT: Duration = Duration::from_secs(2);
const TURN_EVENT_LOOP_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

mod provider_turns;
mod retry;
mod start_failure;

pub use provider_turns::{fail_starting_turn, handle_provider_exit, handle_provider_stall};
pub(super) use retry::finalize_provider_outcome_required;
pub use start_failure::finalize_start_failure_if_needed;

pub(super) async fn wait_for_turn_event_loop(
    session_id: SessionId,
    run_id: RunId,
    turn_id: TurnId,
    events_done: oneshot::Receiver<()>,
) {
    if tokio::time::timeout(TURN_EVENT_LOOP_DRAIN_TIMEOUT, events_done)
        .await
        .is_err()
    {
        tracing::warn!(
            session_id = %session_id.0,
            run_id = %run_id.0,
            turn_id = %turn_id.0,
            drain_timeout_ms = TURN_EVENT_LOOP_DRAIN_TIMEOUT.as_millis(),
            "turn event loop did not drain before terminal fallback; finalizing from scheduler outcome"
        );
    }
}

fn abort_provider(handle: &mut RunHandle) {
    if let Some(abort) = handle.abort.take() {
        abort.abort();
    }
}

fn provider_protocol_violation(reason: &str, message: &str) -> ProviderTurnOutcome {
    ProviderTurnOutcome::protocol_violation(reason, message)
}

pub(super) async fn wait_for_provider_outcome(
    handle: &mut RunHandle,
    closed_fallback: ProviderTurnOutcome,
    timeout_fallback: ProviderTurnOutcome,
) -> ProviderTurnOutcome {
    match tokio::time::timeout(PROVIDER_OUTCOME_WAIT_TIMEOUT, &mut handle.outcome).await {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(_)) => {
            abort_provider(handle);
            closed_fallback
        }
        Err(_) => {
            abort_provider(handle);
            timeout_fallback
        }
    }
}
