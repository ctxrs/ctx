use super::collect::collect_turns_by_statuses;
use super::*;

pub async fn reconcile_running_turns_with_reason(
    state: &Arc<DaemonState>,
    fallback_reason: &str,
) -> Result<()> {
    let (_, running_turns) = collect_turns_by_statuses(
        state,
        &[SessionTurnStatus::Starting, SessionTurnStatus::Running],
    )
    .await?;

    for (_, turn) in running_turns {
        if let Err(err) = reconcile_turn_terminal_state(
            state,
            turn.session_id,
            turn.run_id,
            turn.turn_id,
            fallback_reason,
        )
        .await
        {
            tracing::warn!(
                session_id = %turn.session_id.0,
                turn_id = %turn.turn_id.0,
                err = %err,
                "failed to reconcile running turn after daemon restart"
            );
        }
    }

    Ok(())
}

pub async fn reconcile_running_turns(state: &Arc<DaemonState>) -> Result<()> {
    reconcile_running_turns_with_reason(state, "daemon_restart").await
}
