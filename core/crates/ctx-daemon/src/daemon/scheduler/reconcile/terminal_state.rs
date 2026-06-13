use std::sync::Arc;

use anyhow::Result;

use crate::daemon::DaemonState;
use ctx_core::ids::{RunId, SessionId, TurnId};
use ctx_core::models::{SessionEvent, SessionTurnStatus};
use ctx_core::session_projection::resolve_turn_terminal_state;
use ctx_store::Store;

mod events;

use events::fallback_interrupted_turn_events;

#[async_trait::async_trait]
pub(in crate::daemon) trait TerminalStateReconcileHost: Send + Sync {
    async fn store_for_session(&self, session_id: SessionId) -> Result<Store>;

    async fn publish_event(&self, event: SessionEvent);

    async fn set_running(&self, session_id: SessionId, running: bool);
}

#[async_trait::async_trait]
impl TerminalStateReconcileHost for Arc<DaemonState> {
    async fn store_for_session(&self, session_id: SessionId) -> Result<Store> {
        DaemonState::store_for_session(self.as_ref(), session_id).await
    }

    async fn publish_event(&self, event: SessionEvent) {
        self.session_publication.publish_event(event).await;
    }

    async fn set_running(&self, session_id: SessionId, running: bool) {
        self.task_session_cleanup
            .set_running(session_id, running)
            .await;
    }
}

pub async fn reconcile_turn_terminal_state(
    state: &Arc<DaemonState>,
    session_id: SessionId,
    run_id: Option<RunId>,
    turn_id: TurnId,
    fallback_reason: &str,
) -> Result<()> {
    reconcile_turn_terminal_state_with_host(state, session_id, run_id, turn_id, fallback_reason)
        .await
}

pub(in crate::daemon) async fn reconcile_turn_terminal_state_with_host<H>(
    host: &H,
    session_id: SessionId,
    run_id: Option<RunId>,
    turn_id: TurnId,
    fallback_reason: &str,
) -> Result<()>
where
    H: TerminalStateReconcileHost + ?Sized,
{
    let store = host.store_for_session(session_id).await?;
    let turn = store.get_session_turn(session_id, turn_id).await?;
    let Some(turn) = turn else {
        return Ok(());
    };
    if matches!(
        turn.status,
        SessionTurnStatus::Queued
            | SessionTurnStatus::Completed
            | SessionTurnStatus::Failed
            | SessionTurnStatus::Interrupted
    ) {
        host.set_running(session_id, false).await;
        return Ok(());
    }

    let events = store
        .list_session_events_for_turn(session_id, turn_id, false)
        .await?;
    if resolve_turn_terminal_state(&events).is_some() {
        let _ = store
            .repair_session_turn_projection_from_events(session_id, turn_id)
            .await;
        host.set_running(session_id, false).await;
        return Ok(());
    }

    let persisted = store
        .persist_turn_terminal_events(
            session_id,
            run_id,
            turn_id,
            fallback_interrupted_turn_events(turn.user_message_id, fallback_reason),
        )
        .await?;
    for event in persisted {
        host.publish_event(event).await;
    }
    host.set_running(session_id, false).await;
    Ok(())
}
