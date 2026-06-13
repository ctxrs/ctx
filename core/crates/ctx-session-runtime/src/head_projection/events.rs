use ctx_core::models::{SessionEvent, SessionEventType};

pub fn event_context_window(event: &SessionEvent) -> Option<serde_json::Value> {
    event.payload_json.get("context_window").cloned()
}

pub fn is_session_gap_notice(event: &SessionEvent) -> bool {
    matches!(event.event_type, SessionEventType::Notice)
        && event
            .payload_json
            .get("kind")
            .and_then(|value| value.as_str())
            .is_some_and(|kind| kind == "session_gap")
}
