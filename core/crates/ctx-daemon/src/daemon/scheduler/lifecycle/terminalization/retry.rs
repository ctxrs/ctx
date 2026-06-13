use super::*;

const TURN_TERMINALIZATION_RETRY_LIMIT: usize = 3;
const TURN_TERMINALIZATION_RETRY_BASE_MS: u64 = 50;

async fn turn_finished_persisted(
    lifecycle: &WorkerLifecycleHost,
    session_id: SessionId,
    turn_id: TurnId,
) -> bool {
    let Ok(store) = lifecycle.store_for_session(session_id).await else {
        return false;
    };
    match store
        .list_session_events_for_turn(session_id, turn_id, false)
        .await
    {
        Ok(events) => events
            .iter()
            .any(|event| matches!(event.event_type, SessionEventType::TurnFinished)),
        Err(err) => {
            tracing::warn!(
                session_id = %session_id.0,
                turn_id = %turn_id.0,
                "failed to verify durable TurnFinished after terminalization: {err:#}"
            );
            false
        }
    }
}

pub(in crate::daemon::scheduler::lifecycle) async fn finalize_provider_outcome_required(
    lifecycle: &WorkerLifecycleHost,
    session_id: SessionId,
    run_id: Option<RunId>,
    turn_id: TurnId,
    message_id: MessageId,
    outcome: ProviderTurnOutcome,
) -> bool {
    for attempt in 0..=TURN_TERMINALIZATION_RETRY_LIMIT {
        match finalize_provider_outcome_with_host(
            lifecycle,
            session_id,
            run_id,
            turn_id,
            message_id,
            outcome.clone(),
        )
        .await
        {
            Ok(()) if turn_finished_persisted(lifecycle, session_id, turn_id).await => return true,
            Ok(()) => {
                tracing::warn!(
                    session_id = %session_id.0,
                    turn_id = %turn_id.0,
                    attempt,
                    "turn terminalization completed without durable TurnFinished"
                );
            }
            Err(err) => {
                tracing::warn!(
                    session_id = %session_id.0,
                    turn_id = %turn_id.0,
                    attempt,
                    "turn terminalization failed: {err:#}"
                );
            }
        }

        if attempt < TURN_TERMINALIZATION_RETRY_LIMIT {
            let backoff_ms =
                TURN_TERMINALIZATION_RETRY_BASE_MS.saturating_mul((attempt + 1) as u64);
            tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
        }
    }

    tracing::error!(
        session_id = %session_id.0,
        turn_id = %turn_id.0,
        "turn terminalization exhausted retries without durable TurnFinished"
    );
    false
}
