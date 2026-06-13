use std::sync::Arc;

use anyhow::Result;
use serde_json::json;

use super::terminal_state::reconcile_turn_terminal_state;
use crate::daemon::DaemonState;
use ctx_core::ids::{RunId, SessionId, TurnId};
use ctx_core::models::{SessionEventType, SessionTurnStatus};
use ctx_core::session_projection::resolve_turn_terminal_state;

pub async fn reconcile_turn_failed_on_provider_exit(
    state: &Arc<DaemonState>,
    session_id: SessionId,
    run_id: Option<RunId>,
    turn_id: TurnId,
    fallback_reason: &str,
) -> Result<()> {
    let store = state.store_for_session(session_id).await?;
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
        return Ok(());
    }

    let mut events = store
        .list_session_events_for_turn(session_id, turn_id, false)
        .await?;
    if resolve_turn_terminal_state(&events).is_some() {
        return reconcile_turn_terminal_state(state, session_id, run_id, turn_id, fallback_reason)
            .await;
    }

    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        events = store
            .list_session_events_for_turn(session_id, turn_id, false)
            .await?;
        if resolve_turn_terminal_state(&events).is_some() {
            return reconcile_turn_terminal_state(
                state,
                session_id,
                run_id,
                turn_id,
                fallback_reason,
            )
            .await;
        }
    }

    let message_id = turn.user_message_id.map(|id| id.0);
    let persisted = store
        .persist_turn_terminal_events(
            session_id,
            run_id,
            turn_id,
            vec![(
                SessionEventType::TurnFinished,
                json!({
                    "message_id": message_id,
                    "status": "failed",
                    "message": "provider exited without emitting a terminal event",
                    "reason": fallback_reason,
                    "kind": "provider_exit_without_terminal_event",
                }),
            )],
        )
        .await?;
    for event in persisted {
        state.session_publication.publish_event(event).await;
    }
    Ok(())
}
