use ctx_core::models::SessionEventType;

use crate::adapters::{ProviderTurnOutcome, ProviderTurnStatus};
use crate::events::NormalizedEvent;

use super::super::super::protocol::{CrpEvent, KnownCrpEvent};

pub(super) fn is_sweep_only_status_notice(event: &CrpEvent) -> bool {
    matches!(
        event,
        CrpEvent::Known(event)
            if matches!(
                event.as_ref(),
                KnownCrpEvent::SessionNotice { code, .. }
                    if code == "session_status" || code == "session_status_failed"
            )
    )
}

pub(super) fn outcome_from_terminal_events(
    events: &[NormalizedEvent],
) -> Option<ProviderTurnOutcome> {
    events.iter().find_map(|event| match event.event_type {
        SessionEventType::Done => Some(ProviderTurnOutcome::completed()),
        SessionEventType::TurnFinished
            if event
                .payload_json
                .get("status")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|status| status == "failed" || status == "error") =>
        {
            let message = event
                .payload_json
                .get("message")
                .or_else(|| event.payload_json.get("error"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("crp_turn_error")
                .to_string();
            Some(ProviderTurnOutcome::failed_with_context(
                message,
                event
                    .payload_json
                    .get("reason")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned),
                event.payload_json.get("details").cloned(),
                event.payload_json.get("kind").cloned(),
                true,
            ))
        }
        SessionEventType::TurnInterrupted => Some(ProviderTurnOutcome {
            status: ProviderTurnStatus::Interrupted,
            message: None,
            reason: event
                .payload_json
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .map(ToOwned::to_owned),
            details: None,
            kind: None,
            provider_cancelled: event
                .payload_json
                .get("provider_cancelled")
                .and_then(serde_json::Value::as_bool),
            terminal_event_emitted: true,
        }),
        _ => None,
    })
}

pub(super) fn update_terminal_outcome(
    outcome: &mut Option<ProviderTurnOutcome>,
    events: &[NormalizedEvent],
    done: bool,
) {
    if outcome.is_some() {
        return;
    }

    if let Some(terminal) = outcome_from_terminal_events(events) {
        *outcome = Some(terminal);
    } else if done {
        *outcome = Some(ProviderTurnOutcome::protocol_violation(
            "provider_protocol_violation_no_terminal_outcome",
            "CRP turn ended without a mapped terminal event",
        ));
    }
}

pub(super) fn interrupted_outcome_without_event(
    reason: &str,
    provider_cancelled: bool,
) -> ProviderTurnOutcome {
    ProviderTurnOutcome {
        terminal_event_emitted: false,
        ..ProviderTurnOutcome::interrupted(reason, provider_cancelled)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn event(event_type: SessionEventType, payload_json: serde_json::Value) -> NormalizedEvent {
        NormalizedEvent {
            event_type,
            payload_json,
        }
    }

    #[test]
    fn outcome_from_terminal_events_prefers_first_terminal_event() {
        let outcome = outcome_from_terminal_events(&[
            event(
                SessionEventType::TurnInterrupted,
                json!({"reason": "cancelled", "provider_cancelled": true}),
            ),
            event(SessionEventType::Done, json!({})),
        ])
        .expect("terminal outcome");

        assert!(matches!(outcome.status, ProviderTurnStatus::Interrupted));
        assert_eq!(outcome.reason.as_deref(), Some("cancelled"));
        assert_eq!(outcome.provider_cancelled, Some(true));
    }

    #[test]
    fn outcome_from_terminal_events_prefers_first_failed_turn_finished() {
        let outcome = outcome_from_terminal_events(&[
            event(
                SessionEventType::TurnFinished,
                json!({
                    "status": "failed",
                    "message": "boom",
                    "reason": "crp_error",
                    "details": {"code": 42},
                    "kind": "provider_error",
                }),
            ),
            event(
                SessionEventType::TurnInterrupted,
                json!({"reason": "cancelled", "provider_cancelled": true}),
            ),
        ])
        .expect("terminal outcome");

        assert!(matches!(outcome.status, ProviderTurnStatus::Failed));
        assert_eq!(outcome.message.as_deref(), Some("boom"));
        assert_eq!(outcome.reason.as_deref(), Some("crp_error"));
        assert_eq!(outcome.details, Some(json!({"code": 42})));
        assert_eq!(outcome.kind, Some(json!("provider_error")));
    }

    #[test]
    fn update_terminal_outcome_does_not_override_existing_terminal_outcome() {
        let mut outcome = Some(ProviderTurnOutcome {
            status: ProviderTurnStatus::Interrupted,
            message: None,
            reason: Some("auth_required".to_string()),
            details: None,
            kind: None,
            provider_cancelled: None,
            terminal_event_emitted: true,
        });

        update_terminal_outcome(
            &mut outcome,
            &[event(SessionEventType::Done, json!({}))],
            true,
        );

        let outcome = outcome.expect("terminal outcome");
        assert!(matches!(outcome.status, ProviderTurnStatus::Interrupted));
        assert_eq!(outcome.reason.as_deref(), Some("auth_required"));
    }

    #[test]
    fn interrupted_outcome_without_event_requires_scheduler_terminal_event() {
        let outcome = interrupted_outcome_without_event("cancelled", true);

        assert!(matches!(outcome.status, ProviderTurnStatus::Interrupted));
        assert_eq!(outcome.reason.as_deref(), Some("cancelled"));
        assert_eq!(outcome.provider_cancelled, Some(true));
        assert!(!outcome.terminal_event_emitted);
    }
}
