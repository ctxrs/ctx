use chrono::Utc;
use ctx_history_core::{EventRole, EventType};
use serde_json::Value;

use crate::provider::normalization::{
    provider_json_text, provider_timestamp_millis, provider_value_text,
};

use super::rows::NanoClawMessageRow;

#[derive(Debug)]
pub(crate) struct NanoClawCoreEvent {
    pub(crate) event_type: EventType,
    pub(crate) role: Option<EventRole>,
    pub(crate) occurred_at: chrono::DateTime<Utc>,
}

/// Provider-owned normalization for NativePath Core projection.
pub(super) fn nanoclaw_core_event(
    message: &NanoClawMessageRow,
    fallback: chrono::DateTime<Utc>,
) -> (NanoClawCoreEvent, String) {
    let occurred_at = provider_timestamp_millis(message.timestamp, fallback);
    let content = message
        .content
        .as_deref()
        .map(provider_json_text)
        .unwrap_or(Value::Null);
    let text = provider_value_text(&content).unwrap_or_else(|| {
        format!(
            "NanoClaw {}",
            message.kind.as_deref().unwrap_or(message.source)
        )
    });
    let role = if message.source == "inbound" {
        Some(EventRole::User)
    } else {
        Some(EventRole::Assistant)
    };
    let event_type = EventType::Message;
    let event = NanoClawCoreEvent {
        event_type,
        role,
        occurred_at,
    };
    (event, text)
}
