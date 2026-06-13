#[cfg(test)]
use std::sync::Arc;

use anyhow::Result;

use ctx_core::ids::{MessageId, RunId, SessionId, TurnId};
use ctx_core::models::{RunStatus, SessionEventType};
use ctx_providers::adapters::{ProviderTurnOutcome, ProviderTurnStatus};

#[cfg(test)]
use crate::daemon::DaemonState;

use super::super::persistence::SchedulerPersistenceHost;
use super::persistence::persist_terminal_events_with_host;
use super::types::{FailedTurnTerminalization, InterruptedTurnTerminalization};

use self::events::{
    completed_turn_finished_event, failed_turn_finished_event, interrupted_turn_events,
};

mod events;

#[cfg(test)]
pub async fn finalize_completed_turn(
    state: &Arc<DaemonState>,
    session_id: SessionId,
    run_id: Option<RunId>,
    turn_id: TurnId,
    message_id: MessageId,
) -> Result<()> {
    finalize_completed_turn_with_host(state, session_id, run_id, turn_id, message_id).await
}

pub(in crate::daemon::scheduler) async fn finalize_completed_turn_with_host<H>(
    host: &H,
    session_id: SessionId,
    run_id: Option<RunId>,
    turn_id: TurnId,
    message_id: MessageId,
) -> Result<()>
where
    H: SchedulerPersistenceHost + ?Sized,
{
    persist_terminal_events_with_host(
        host,
        session_id,
        run_id,
        turn_id,
        RunStatus::Completed,
        &[
            SessionEventType::ThoughtChunk,
            SessionEventType::ContextWindowUpdate,
        ],
        vec![completed_turn_finished_event(message_id)],
    )
    .await
}

pub(in crate::daemon::scheduler) async fn finalize_interrupted_turn_with_host<H>(
    host: &H,
    session_id: SessionId,
    run_id: Option<RunId>,
    turn_id: TurnId,
    message_id: MessageId,
    interruption: InterruptedTurnTerminalization<'_>,
) -> Result<()>
where
    H: SchedulerPersistenceHost + ?Sized,
{
    persist_terminal_events_with_host(
        host,
        session_id,
        run_id,
        turn_id,
        RunStatus::Cancelled,
        &[
            SessionEventType::AssistantChunk,
            SessionEventType::ThoughtChunk,
            SessionEventType::ContextWindowUpdate,
        ],
        interrupted_turn_events(message_id, interruption),
    )
    .await
}

#[cfg(test)]
pub async fn finalize_failed_turn(
    state: &Arc<DaemonState>,
    session_id: SessionId,
    run_id: Option<RunId>,
    turn_id: TurnId,
    message_id: MessageId,
    failure: FailedTurnTerminalization<'_>,
) -> Result<()> {
    finalize_failed_turn_with_host(state, session_id, run_id, turn_id, message_id, failure).await
}

pub(in crate::daemon::scheduler) async fn finalize_failed_turn_with_host<H>(
    host: &H,
    session_id: SessionId,
    run_id: Option<RunId>,
    turn_id: TurnId,
    message_id: MessageId,
    failure: FailedTurnTerminalization<'_>,
) -> Result<()>
where
    H: SchedulerPersistenceHost + ?Sized,
{
    persist_terminal_events_with_host(
        host,
        session_id,
        run_id,
        turn_id,
        RunStatus::Failed,
        &[
            SessionEventType::AssistantChunk,
            SessionEventType::ThoughtChunk,
            SessionEventType::ContextWindowUpdate,
        ],
        vec![failed_turn_finished_event(message_id, failure)],
    )
    .await
}

pub(in crate::daemon::scheduler) async fn finalize_provider_outcome_with_host<H>(
    host: &H,
    session_id: SessionId,
    run_id: Option<RunId>,
    turn_id: TurnId,
    message_id: MessageId,
    outcome: ProviderTurnOutcome,
) -> Result<()>
where
    H: SchedulerPersistenceHost + ?Sized,
{
    match outcome.status {
        ProviderTurnStatus::Completed => {
            finalize_completed_turn_with_host(host, session_id, run_id, turn_id, message_id).await
        }
        ProviderTurnStatus::Interrupted => {
            finalize_interrupted_turn_with_host(
                host,
                session_id,
                run_id,
                turn_id,
                message_id,
                InterruptedTurnTerminalization {
                    reason: outcome.reason.as_deref().unwrap_or("interrupted"),
                    provider_cancelled: outcome.provider_cancelled.unwrap_or(false),
                    emit_interrupt_event: !outcome.terminal_event_emitted,
                },
            )
            .await
        }
        ProviderTurnStatus::Failed => {
            finalize_failed_turn_with_host(
                host,
                session_id,
                run_id,
                turn_id,
                message_id,
                FailedTurnTerminalization {
                    message: outcome.message.as_deref().unwrap_or("provider turn failed"),
                    reason: outcome.reason.as_deref(),
                    details: outcome.details,
                    kind: outcome.kind,
                },
            )
            .await
        }
    }
}
