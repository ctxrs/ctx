use ctx_core::models::{
    SessionActivityState, SessionEvent, SessionEventType, SessionTurn, SessionTurnStatus,
};

use super::turns::terminal_status_from_finished_event;

pub fn derive_summary_activity(event: &SessionEvent) -> Option<SessionActivityState> {
    match event.event_type {
        SessionEventType::TurnQueued => Some(SessionActivityState {
            is_working: false,
            last_turn_status: Some(SessionTurnStatus::Queued),
        }),
        SessionEventType::TurnStarted => Some(SessionActivityState {
            is_working: true,
            last_turn_status: Some(SessionTurnStatus::Running),
        }),
        SessionEventType::TurnInterrupted => Some(SessionActivityState {
            is_working: false,
            last_turn_status: Some(SessionTurnStatus::Interrupted),
        }),
        SessionEventType::TurnFinished => {
            terminal_status_from_finished_event(event).map(|status| SessionActivityState {
                is_working: false,
                last_turn_status: Some(status),
            })
        }
        _ => None,
    }
}

pub fn activity_from_turn(turn: &SessionTurn) -> SessionActivityState {
    match turn.status {
        SessionTurnStatus::Queued => SessionActivityState {
            is_working: false,
            last_turn_status: Some(SessionTurnStatus::Queued),
        },
        SessionTurnStatus::Starting => SessionActivityState {
            is_working: true,
            last_turn_status: Some(SessionTurnStatus::Starting),
        },
        SessionTurnStatus::Running => SessionActivityState {
            is_working: true,
            last_turn_status: Some(SessionTurnStatus::Running),
        },
        SessionTurnStatus::Completed => SessionActivityState {
            is_working: false,
            last_turn_status: Some(SessionTurnStatus::Completed),
        },
        SessionTurnStatus::Interrupted => SessionActivityState {
            is_working: false,
            last_turn_status: Some(SessionTurnStatus::Interrupted),
        },
        SessionTurnStatus::Failed => SessionActivityState {
            is_working: false,
            last_turn_status: Some(SessionTurnStatus::Failed),
        },
    }
}
