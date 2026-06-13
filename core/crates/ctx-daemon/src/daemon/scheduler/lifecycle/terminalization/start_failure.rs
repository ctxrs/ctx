use super::*;

fn has_terminal_event(event_type: &SessionEventType) -> bool {
    matches!(
        event_type,
        SessionEventType::Done | SessionEventType::TurnInterrupted | SessionEventType::TurnFinished
    )
}

pub async fn finalize_start_failure_if_needed(
    lifecycle: &WorkerLifecycleHost,
    session_id: SessionId,
    run_id: Option<RunId>,
    turn_id: TurnId,
    message_id: MessageId,
    error_message: &str,
) {
    let Ok(store) = lifecycle.store_for_session(session_id).await else {
        return;
    };
    let turn = store
        .get_session_turn(session_id, turn_id)
        .await
        .ok()
        .flatten();
    if turn.as_ref().is_some_and(|turn| {
        matches!(
            turn.status,
            SessionTurnStatus::Completed
                | SessionTurnStatus::Failed
                | SessionTurnStatus::Interrupted
        )
    }) {
        return;
    }

    if let Ok(events) = store
        .list_session_events_for_turn(session_id, turn_id, false)
        .await
    {
        if events
            .iter()
            .any(|event| has_terminal_event(&event.event_type))
        {
            let _ = store
                .repair_session_turn_projection_from_events(session_id, turn_id)
                .await;
            lifecycle.set_running(session_id, false).await;
            return;
        }
    }

    let _ = finalize_failed_turn_with_host(
        lifecycle,
        session_id,
        run_id,
        turn_id,
        message_id,
        FailedTurnTerminalization {
            message: error_message,
            reason: Some("start_failed"),
            details: None,
            kind: Some(json!("start_failed")),
        },
    )
    .await;
}
