use super::super::protocol::{CrpEvent, KnownCrpEvent};

pub(super) fn event_turn_id(event: &CrpEvent) -> Option<&str> {
    match event {
        CrpEvent::Known(event) => known_event_turn_id(event.as_ref()),
        CrpEvent::Unknown { turn_id, .. } => turn_id.as_deref(),
    }
}

pub(super) fn event_matches_session(event: &CrpEvent, session_id: &str) -> bool {
    match event {
        CrpEvent::Known(event) => known_event_session_id(event.as_ref()) == Some(session_id),
        CrpEvent::Unknown { session_id: id, .. } => id.as_deref() == Some(session_id),
    }
}

fn known_event_turn_id(event: &KnownCrpEvent) -> Option<&str> {
    match event {
        KnownCrpEvent::SessionGap { turn_id, .. }
        | KnownCrpEvent::SessionNotice { turn_id, .. } => turn_id.as_deref(),
        KnownCrpEvent::TurnStarted { turn_id, .. }
        | KnownCrpEvent::MessageDelta { turn_id, .. }
        | KnownCrpEvent::MessageFinal { turn_id, .. }
        | KnownCrpEvent::ReasoningSummary { turn_id, .. }
        | KnownCrpEvent::ReasoningTrace { turn_id, .. }
        | KnownCrpEvent::ReasoningTraceFinal { turn_id, .. }
        | KnownCrpEvent::TurnContextWindowUpdated { turn_id, .. }
        | KnownCrpEvent::ToolStarted { turn_id, .. }
        | KnownCrpEvent::ToolOutputDelta { turn_id, .. }
        | KnownCrpEvent::ToolCompleted { turn_id, .. }
        | KnownCrpEvent::TurnCompleted { turn_id, .. } => Some(turn_id.as_str()),
        KnownCrpEvent::SessionOpened { .. } | KnownCrpEvent::ModelsList { .. } => None,
    }
}

fn known_event_session_id(event: &KnownCrpEvent) -> Option<&str> {
    match event {
        KnownCrpEvent::SessionOpened { session_id, .. }
        | KnownCrpEvent::TurnStarted { session_id, .. }
        | KnownCrpEvent::MessageDelta { session_id, .. }
        | KnownCrpEvent::MessageFinal { session_id, .. }
        | KnownCrpEvent::ReasoningSummary { session_id, .. }
        | KnownCrpEvent::ReasoningTrace { session_id, .. }
        | KnownCrpEvent::ReasoningTraceFinal { session_id, .. }
        | KnownCrpEvent::TurnContextWindowUpdated { session_id, .. }
        | KnownCrpEvent::ToolStarted { session_id, .. }
        | KnownCrpEvent::ToolOutputDelta { session_id, .. }
        | KnownCrpEvent::ToolCompleted { session_id, .. }
        | KnownCrpEvent::TurnCompleted { session_id, .. }
        | KnownCrpEvent::SessionGap { session_id, .. }
        | KnownCrpEvent::SessionNotice { session_id, .. } => Some(session_id.as_str()),
        KnownCrpEvent::ModelsList { .. } => None,
    }
}
