use std::time::Duration;

use ctx_core::models::{MessageDelivery, SessionTurn, SessionTurnStatus};

#[cfg(test)]
use ctx_core::models::{SessionEvent, SessionEventType};
#[cfg(test)]
use ctx_core::session_projection::turn_status_from_finished_payload;

pub fn subagent_status_from_turn_status(status: SessionTurnStatus) -> &'static str {
    match status {
        SessionTurnStatus::Completed => "completed",
        SessionTurnStatus::Interrupted => "interrupted",
        SessionTurnStatus::Failed => "failed",
        SessionTurnStatus::Starting | SessionTurnStatus::Running | SessionTurnStatus::Queued => {
            "running"
        }
    }
}

pub fn subagent_terminal_status_from_turn_status(
    status: SessionTurnStatus,
) -> Option<&'static str> {
    match status {
        SessionTurnStatus::Completed => Some("completed"),
        SessionTurnStatus::Interrupted => Some("interrupted"),
        SessionTurnStatus::Failed => Some("failed"),
        SessionTurnStatus::Starting | SessionTurnStatus::Running | SessionTurnStatus::Queued => {
            None
        }
    }
}

pub fn turn_status_has_input_backlog(status: &SessionTurnStatus) -> bool {
    matches!(
        status,
        SessionTurnStatus::Queued | SessionTurnStatus::Starting | SessionTurnStatus::Running
    )
}

pub fn agent_terminal_result_status(status: SessionTurnStatus) -> Option<&'static str> {
    match status {
        SessionTurnStatus::Completed => Some("completed"),
        SessionTurnStatus::Interrupted => Some("interrupted"),
        SessionTurnStatus::Failed => Some("failed"),
        SessionTurnStatus::Queued | SessionTurnStatus::Starting | SessionTurnStatus::Running => {
            None
        }
    }
}

pub fn agent_delivery_label(delivery: &MessageDelivery) -> &'static str {
    match delivery {
        MessageDelivery::Immediate => "immediate",
        MessageDelivery::Queued => "queued",
    }
}

pub fn is_active_turn_status(status: &SessionTurnStatus) -> bool {
    matches!(
        status,
        SessionTurnStatus::Queued | SessionTurnStatus::Starting | SessionTurnStatus::Running
    )
}

pub fn agent_active_state(status: SessionTurnStatus) -> &'static str {
    match status {
        SessionTurnStatus::Queued => "queued",
        SessionTurnStatus::Starting => "starting",
        SessionTurnStatus::Running => "running",
        SessionTurnStatus::Completed
        | SessionTurnStatus::Interrupted
        | SessionTurnStatus::Failed => "waiting_input",
    }
}

pub fn agent_health(
    active_turn: Option<&SessionTurn>,
    inactivity_timeout: Duration,
) -> &'static str {
    let Some(turn) = active_turn else {
        return "healthy";
    };

    let stalled_after = inactivity_timeout.max(Duration::from_millis(1));
    let slow_after = stalled_after.checked_div(2).unwrap_or(stalled_after);
    let age = chrono::Utc::now()
        .signed_duration_since(turn.updated_at)
        .to_std()
        .unwrap_or_default();
    if age >= stalled_after {
        "stalled"
    } else if !slow_after.is_zero() && age >= slow_after {
        "slow"
    } else {
        "healthy"
    }
}

#[cfg(test)]
fn subagent_terminal_status_from_event(event: &SessionEvent) -> Option<&'static str> {
    match event.event_type {
        SessionEventType::Done => Some("completed"),
        SessionEventType::TurnInterrupted => Some("interrupted"),
        SessionEventType::TurnFinished => turn_status_from_finished_payload(&event.payload_json)
            .and_then(subagent_terminal_status_from_turn_status),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use chrono::Utc;
    use serde_json::json;

    use ctx_core::ids::{RunId, SessionEventId, SessionId, TurnId};

    fn terminal_event(status: &str) -> SessionEvent {
        SessionEvent {
            seq: 1,
            id: SessionEventId::new(),
            session_id: SessionId::new(),
            run_id: Some(RunId::new()),
            turn_id: Some(TurnId::new()),
            event_type: SessionEventType::TurnFinished,
            payload_json: json!({ "status": status }),
            transient: false,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn subagent_terminal_status_uses_turn_finished_failed_payload() {
        assert_eq!(
            subagent_terminal_status_from_event(&terminal_event("failed")),
            Some("failed")
        );
    }

    #[test]
    fn subagent_terminal_status_uses_turn_finished_interrupted_payload() {
        assert_eq!(
            subagent_terminal_status_from_event(&terminal_event("interrupted")),
            Some("interrupted")
        );
    }

    #[test]
    fn input_backlog_includes_starting_turns() {
        assert!(turn_status_has_input_backlog(&SessionTurnStatus::Starting));
        assert!(turn_status_has_input_backlog(&SessionTurnStatus::Running));
        assert!(turn_status_has_input_backlog(&SessionTurnStatus::Queued));
        assert!(!turn_status_has_input_backlog(
            &SessionTurnStatus::Completed
        ));
    }
}
