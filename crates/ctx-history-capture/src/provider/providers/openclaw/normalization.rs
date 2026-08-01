use chrono::{DateTime, Utc};
use ctx_history_core::{EventRole, EventType};
use serde_json::Value;

use crate::provider::normalization::{provider_role, provider_value_text};

pub(crate) struct OpenClawEventFact {
    pub(crate) provider_event_index: u64,
    pub(crate) provider_event_hash: Option<String>,
    pub(crate) event_type: EventType,
    pub(crate) role: Option<EventRole>,
    pub(crate) occurred_at: DateTime<Utc>,
    pub(crate) lexical_text: String,
}

pub(super) fn event_fact(
    event_index: u64,
    _line_number: usize,
    row: &Value,
    occurred_at: DateTime<Utc>,
) -> OpenClawEventFact {
    let row_type = row.get("type").and_then(Value::as_str).unwrap_or("message");
    let message = row.get("message").unwrap_or(row);
    let role = message
        .get("role")
        .or_else(|| row.get("role"))
        .and_then(Value::as_str)
        .map(|role| provider_role(Some(role)));
    let event_type = match row_type {
        "message" => match role {
            Some(EventRole::Tool) => EventType::ToolOutput,
            _ => EventType::Message,
        },
        "leaf" | "compaction" | "custom" => EventType::Notice,
        _ => EventType::Notice,
    };
    let text = message
        .get("content")
        .or_else(|| message.get("text"))
        .or_else(|| message.get("output"))
        .and_then(provider_value_text)
        .unwrap_or_default();
    OpenClawEventFact {
        provider_event_index: event_index,
        provider_event_hash: row.get("id").and_then(Value::as_str).map(str::to_owned),
        event_type,
        role,
        occurred_at,
        lexical_text: text,
    }
}
