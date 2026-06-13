use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::models::{
    SessionActivityState, SessionEvent, SessionEventType, SessionTurnFailure, SessionTurnStatus,
};

#[derive(Debug, Clone, PartialEq)]
pub struct TurnTerminalState {
    pub status: SessionTurnStatus,
    pub end_seq: Option<i64>,
    pub metrics: Option<Value>,
    pub failure: Option<SessionTurnFailure>,
    pub updated_at: DateTime<Utc>,
}

pub fn derive_activity_from_status(
    last_status: Option<SessionTurnStatus>,
    has_running_turn: bool,
) -> SessionActivityState {
    let has_starting_turn = matches!(last_status, Some(SessionTurnStatus::Starting));
    SessionActivityState {
        is_working: has_running_turn || has_starting_turn,
        last_turn_status: last_status,
    }
}

pub fn turn_status_from_finished_payload(payload: &Value) -> Option<SessionTurnStatus> {
    match payload.get("status").and_then(Value::as_str) {
        Some("completed") => Some(SessionTurnStatus::Completed),
        Some("failed" | "error") => Some(SessionTurnStatus::Failed),
        Some("interrupted") => Some(SessionTurnStatus::Interrupted),
        Some("queued") => Some(SessionTurnStatus::Queued),
        Some("starting") => Some(SessionTurnStatus::Starting),
        Some("running") => Some(SessionTurnStatus::Running),
        _ => None,
    }
}

pub fn terminal_status_from_finished_payload(payload: &Value) -> Option<SessionTurnStatus> {
    match turn_status_from_finished_payload(payload)? {
        status @ (SessionTurnStatus::Completed
        | SessionTurnStatus::Failed
        | SessionTurnStatus::Interrupted) => Some(status),
        SessionTurnStatus::Queued | SessionTurnStatus::Starting | SessionTurnStatus::Running => {
            None
        }
    }
}

pub fn turn_status_from_event(event: &SessionEvent) -> Option<SessionTurnStatus> {
    match event.event_type {
        SessionEventType::TurnQueued => Some(SessionTurnStatus::Queued),
        SessionEventType::TurnStarted => Some(SessionTurnStatus::Running),
        SessionEventType::TurnInterrupted => Some(SessionTurnStatus::Interrupted),
        SessionEventType::TurnFinished => turn_status_from_finished_payload(&event.payload_json),
        _ => None,
    }
}

fn is_terminal_event(event_type: &SessionEventType) -> bool {
    matches!(
        event_type,
        SessionEventType::TurnFinished | SessionEventType::TurnInterrupted
    )
}

fn latest_done_metrics(events: &[SessionEvent]) -> Option<Value> {
    events
        .iter()
        .rev()
        .find(|event| matches!(event.event_type, SessionEventType::Done))
        .and_then(|event| event.payload_json.get("context_window"))
        .cloned()
}

fn payload_string(payload: &Value, key: &str) -> Option<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

pub fn turn_failure_from_finished_payload(payload: &Value) -> Option<SessionTurnFailure> {
    if turn_status_from_finished_payload(payload) != Some(SessionTurnStatus::Failed) {
        return None;
    }

    let message = payload_string(payload, "message")
        .or_else(|| payload_string(payload, "error"))
        .or_else(|| payload_string(payload, "reason"));
    let details = payload
        .get("details")
        .filter(|value| !value.is_null())
        .cloned();
    let kind = payload_string(payload, "kind");
    let reason = payload_string(payload, "reason");
    let provider = payload_string(payload, "provider");
    let provider_id =
        payload_string(payload, "provider_id").or_else(|| payload_string(payload, "providerId"));

    let failure = SessionTurnFailure {
        message,
        details,
        kind,
        reason,
        provider,
        provider_id,
    };

    if failure.message.is_none()
        && failure.details.is_none()
        && failure.kind.is_none()
        && failure.reason.is_none()
        && failure.provider.is_none()
        && failure.provider_id.is_none()
    {
        None
    } else {
        Some(failure)
    }
}

pub fn resolve_turn_terminal_state(events: &[SessionEvent]) -> Option<TurnTerminalState> {
    let (terminal_index, event) = events.iter().enumerate().rev().find(|(_, event)| {
        is_terminal_event(&event.event_type)
            && (matches!(event.event_type, SessionEventType::TurnInterrupted)
                || terminal_status_from_finished_payload(&event.payload_json).is_some())
    })?;

    match event.event_type {
        SessionEventType::TurnInterrupted => Some(TurnTerminalState {
            status: SessionTurnStatus::Interrupted,
            end_seq: Some(event.seq),
            metrics: None,
            failure: None,
            updated_at: event.created_at,
        }),
        SessionEventType::TurnFinished => {
            let status = terminal_status_from_finished_payload(&event.payload_json)?;
            let metrics = if status == SessionTurnStatus::Completed {
                latest_done_metrics(&events[..=terminal_index])
            } else {
                None
            };
            let failure = turn_failure_from_finished_payload(&event.payload_json);
            Some(TurnTerminalState {
                status,
                end_seq: Some(event.seq),
                metrics,
                failure,
                updated_at: event.created_at,
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    use crate::ids::{SessionEventId, SessionId, TurnId};
    use crate::models::{SessionEvent, SessionEventType, SessionTurnStatus};

    use super::{
        resolve_turn_terminal_state, terminal_status_from_finished_payload,
        turn_failure_from_finished_payload, turn_status_from_finished_payload,
    };

    fn event(
        seq: i64,
        turn_id: TurnId,
        event_type: SessionEventType,
        payload_json: serde_json::Value,
    ) -> SessionEvent {
        SessionEvent {
            seq,
            id: SessionEventId::new(),
            session_id: SessionId::new(),
            run_id: None,
            turn_id: Some(turn_id),
            event_type,
            payload_json,
            transient: false,
            created_at: Utc.timestamp_opt(seq, 0).unwrap(),
        }
    }

    #[test]
    fn turn_finished_status_parses_known_values() {
        assert_eq!(
            turn_status_from_finished_payload(&json!({ "status": "completed" })),
            Some(SessionTurnStatus::Completed)
        );
        assert_eq!(
            turn_status_from_finished_payload(&json!({ "status": "failed" })),
            Some(SessionTurnStatus::Failed)
        );
        assert_eq!(
            turn_status_from_finished_payload(&json!({ "status": "interrupted" })),
            Some(SessionTurnStatus::Interrupted)
        );
    }

    #[test]
    fn resolve_terminal_state_prefers_last_terminal_event() {
        let turn_id = TurnId::new();
        let events = vec![
            event(1, turn_id, SessionEventType::TurnStarted, json!({})),
            event(
                2,
                turn_id,
                SessionEventType::Done,
                json!({ "context_window": { "total_tokens": 42 } }),
            ),
            event(
                3,
                turn_id,
                SessionEventType::TurnFinished,
                json!({ "status": "completed" }),
            ),
        ];
        let terminal = resolve_turn_terminal_state(&events).expect("terminal state");
        assert_eq!(terminal.status, SessionTurnStatus::Completed);
        assert_eq!(terminal.end_seq, Some(3));
        assert_eq!(terminal.metrics, Some(json!({ "total_tokens": 42 })));
        assert!(terminal.failure.is_none());
    }

    #[test]
    fn failed_turn_finished_projects_failure_payload() {
        let turn_id = TurnId::new();
        let events = vec![
            event(1, turn_id, SessionEventType::TurnStarted, json!({})),
            event(
                2,
                turn_id,
                SessionEventType::TurnFinished,
                json!({
                    "status": "failed",
                    "message": "provider died",
                    "details": { "exit_code": 1 },
                    "kind": "provider_protocol_violation",
                    "provider_id": "codex",
                }),
            ),
        ];

        let terminal = resolve_turn_terminal_state(&events).expect("terminal state");
        let failure = terminal.failure.expect("failure projection");
        assert_eq!(terminal.status, SessionTurnStatus::Failed);
        assert_eq!(failure.message.as_deref(), Some("provider died"));
        assert_eq!(failure.details, Some(json!({ "exit_code": 1 })));
        assert_eq!(failure.kind.as_deref(), Some("provider_protocol_violation"));
        assert_eq!(failure.provider_id.as_deref(), Some("codex"));
    }

    #[test]
    fn non_failed_turn_finished_has_no_failure_projection() {
        assert!(turn_failure_from_finished_payload(&json!({
            "status": "completed",
            "message": "ignored"
        }))
        .is_none());
    }

    #[test]
    fn done_event_without_turn_finished_is_not_terminal() {
        let turn_id = TurnId::new();
        let events = vec![
            event(1, turn_id, SessionEventType::TurnStarted, json!({})),
            event(2, turn_id, SessionEventType::Done, json!({})),
        ];

        assert!(resolve_turn_terminal_state(&events).is_none());
    }

    #[test]
    fn turn_interrupted_without_turn_finished_resolves_terminal_state() {
        let turn_id = TurnId::new();
        let events = vec![
            event(1, turn_id, SessionEventType::TurnStarted, json!({})),
            event(
                2,
                turn_id,
                SessionEventType::TurnInterrupted,
                json!({"reason": "cancelled"}),
            ),
        ];

        let terminal = resolve_turn_terminal_state(&events).expect("terminal state");
        assert_eq!(terminal.status, SessionTurnStatus::Interrupted);
        assert_eq!(terminal.end_seq, Some(2));
        assert!(terminal.metrics.is_none());
        assert!(terminal.failure.is_none());
    }

    #[test]
    fn turn_finished_without_status_is_not_terminal() {
        let turn_id = TurnId::new();
        let events = vec![event(1, turn_id, SessionEventType::TurnFinished, json!({}))];

        assert!(resolve_turn_terminal_state(&events).is_none());
    }

    #[test]
    fn malformed_turn_finished_does_not_mask_prior_terminal_state() {
        let turn_id = TurnId::new();
        let events = vec![
            event(1, turn_id, SessionEventType::TurnStarted, json!({})),
            event(
                2,
                turn_id,
                SessionEventType::TurnFinished,
                json!({ "status": "failed", "message": "real failure" }),
            ),
            event(
                3,
                turn_id,
                SessionEventType::TurnFinished,
                json!({ "status": "not-a-terminal-status" }),
            ),
        ];

        let terminal = resolve_turn_terminal_state(&events).expect("terminal state");
        assert_eq!(terminal.status, SessionTurnStatus::Failed);
        assert_eq!(terminal.end_seq, Some(2));
        assert_eq!(
            terminal
                .failure
                .as_ref()
                .and_then(|failure| failure.message.as_deref()),
            Some("real failure")
        );
    }

    #[test]
    fn running_turn_finished_status_is_not_terminal() {
        assert_eq!(
            terminal_status_from_finished_payload(&json!({ "status": "running" })),
            None
        );
    }
}
