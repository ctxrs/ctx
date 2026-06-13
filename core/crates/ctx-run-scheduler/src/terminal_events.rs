use serde_json::{json, Value};

use ctx_core::ids::MessageId;
use ctx_core::models::SessionEventType;

pub struct InterruptedTurnTerminalization<'a> {
    pub reason: &'a str,
    pub provider_cancelled: bool,
    pub emit_interrupt_event: bool,
}

pub struct FailedTurnTerminalization<'a> {
    pub message: &'a str,
    pub reason: Option<&'a str>,
    pub details: Option<Value>,
    pub kind: Option<Value>,
}

pub fn completed_turn_finished_event(message_id: MessageId) -> (SessionEventType, Value) {
    (
        SessionEventType::TurnFinished,
        json!({
            "message_id": message_id.0,
            "status": "completed",
        }),
    )
}

pub fn interrupted_turn_events(
    message_id: MessageId,
    interruption: InterruptedTurnTerminalization<'_>,
) -> Vec<(SessionEventType, Value)> {
    let mut events = Vec::new();
    if interruption.emit_interrupt_event {
        events.push((
            SessionEventType::TurnInterrupted,
            json!({
                "reason": interruption.reason,
                "provider_cancelled": interruption.provider_cancelled,
                "status": "interrupted",
            }),
        ));
    }
    events.push((
        SessionEventType::TurnFinished,
        json!({
            "message_id": message_id.0,
            "status": "interrupted",
            "reason": interruption.reason,
            "provider_cancelled": interruption.provider_cancelled,
        }),
    ));
    events
}

pub fn failed_turn_finished_event(
    message_id: MessageId,
    failure: FailedTurnTerminalization<'_>,
) -> (SessionEventType, Value) {
    let mut finished = json!({
        "message_id": message_id.0,
        "status": "failed",
    });
    if let Some(obj) = finished.as_object_mut() {
        if let Some(reason) = failure.reason {
            obj.insert("reason".to_string(), json!(reason));
        }
        obj.insert("message".to_string(), json!(failure.message));
        if let Some(details) = failure.details {
            obj.insert("details".to_string(), details);
        }
        if let Some(kind) = failure.kind {
            obj.insert("kind".to_string(), kind);
        }
    }
    (SessionEventType::TurnFinished, finished)
}

pub fn fallback_interrupted_turn_events(
    message_id: Option<MessageId>,
    fallback_reason: &str,
) -> Vec<(SessionEventType, Value)> {
    vec![
        (
            SessionEventType::TurnInterrupted,
            json!({
                "reason": fallback_reason,
                "provider_cancelled": false,
                "status": "interrupted",
            }),
        ),
        (
            SessionEventType::TurnFinished,
            json!({
                "message_id": message_id.map(|id| id.0),
                "status": "interrupted",
                "reason": fallback_reason,
                "provider_cancelled": false,
            }),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_events_can_emit_interrupt_and_finished_payloads() {
        let message_id = MessageId::new();
        let events = interrupted_turn_events(
            message_id,
            InterruptedTurnTerminalization {
                reason: "cancelled",
                provider_cancelled: true,
                emit_interrupt_event: true,
            },
        );

        assert_eq!(events.len(), 2);
        assert!(matches!(events[0].0, SessionEventType::TurnInterrupted));
        assert!(matches!(events[1].0, SessionEventType::TurnFinished));
        let expected_message_id = message_id.0.to_string();
        assert_eq!(
            events[1].1["message_id"].as_str(),
            Some(expected_message_id.as_str())
        );
        assert_eq!(events[1].1["status"], "interrupted");
    }

    #[test]
    fn failed_event_preserves_optional_details() {
        let (_, payload) = failed_turn_finished_event(
            MessageId::new(),
            FailedTurnTerminalization {
                message: "failed",
                reason: Some("provider"),
                details: Some(json!({"code": "x"})),
                kind: Some(json!("runtime")),
            },
        );

        assert_eq!(payload["status"], "failed");
        assert_eq!(payload["message"], "failed");
        assert_eq!(payload["reason"], "provider");
        assert_eq!(payload["details"]["code"], "x");
        assert_eq!(payload["kind"], "runtime");
    }
}
