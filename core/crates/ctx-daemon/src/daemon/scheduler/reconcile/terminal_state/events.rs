pub(super) use ctx_run_scheduler::terminal_events::fallback_interrupted_turn_events;

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::json;

    use ctx_core::ids::{RunId, SessionEventId, SessionId, TurnId};
    use ctx_core::models::{SessionEvent, SessionEventType, SessionTurnStatus};
    use ctx_core::session_projection::resolve_turn_terminal_state;

    fn event(
        seq: i64,
        event_type: SessionEventType,
        payload_json: serde_json::Value,
    ) -> SessionEvent {
        SessionEvent {
            seq,
            id: SessionEventId::new(),
            session_id: SessionId::new(),
            run_id: Some(RunId::new()),
            turn_id: Some(TurnId::new()),
            event_type,
            payload_json,
            transient: false,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn turn_finished_status_overrides_completed_default() {
        let events = vec![event(
            4,
            SessionEventType::TurnFinished,
            json!({"status": "interrupted"}),
        )];

        let resolved = resolve_turn_terminal_state(&events).expect("resolved state");
        assert_eq!(resolved.status, SessionTurnStatus::Interrupted);
        assert_eq!(resolved.end_seq, Some(4));
    }

    #[test]
    fn turn_finished_without_status_does_not_resolve_terminal_state() {
        let events = vec![
            event(3, SessionEventType::Done, json!({})),
            event(4, SessionEventType::TurnFinished, json!({})),
        ];

        assert!(resolve_turn_terminal_state(&events).is_none());
    }

    #[test]
    fn turn_finished_keeps_done_metrics() {
        let events = vec![
            event(
                2,
                SessionEventType::Done,
                json!({"context_window": {"context_tokens_estimate": 42}}),
            ),
            event(
                3,
                SessionEventType::TurnFinished,
                json!({"status": "completed"}),
            ),
        ];

        let resolved = resolve_turn_terminal_state(&events).expect("resolved state");
        assert_eq!(resolved.status, SessionTurnStatus::Completed);
        assert_eq!(
            resolved.metrics,
            Some(json!({"context_tokens_estimate": 42}))
        );
    }
}
