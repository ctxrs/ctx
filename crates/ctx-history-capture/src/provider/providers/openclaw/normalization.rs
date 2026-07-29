use chrono::{DateTime, Utc};
use ctx_history_core::{EventRole, EventType};
use serde_json::{json, Value};

use crate::{
    provider::normalization::{
        provider_policy_event_text, provider_result_identifier_evidence,
        provider_result_outcome_evidence, provider_role, provider_value_text,
    },
    OPENCLAW_SOURCE_FORMAT,
};

pub(crate) struct OpenClawEventFact {
    pub(crate) provider_event_index: u64,
    pub(crate) provider_event_hash: Option<String>,
    pub(crate) cursor: String,
    pub(crate) event_type: EventType,
    pub(crate) role: Option<EventRole>,
    pub(crate) occurred_at: DateTime<Utc>,
    pub(crate) lexical_text: String,
    pub(crate) payload: Value,
    pub(crate) metadata: Value,
}

pub(crate) fn event(
    _provider_session_id: &str,
    event_index: u64,
    line_number: usize,
    row: &Value,
    occurred_at: DateTime<Utc>,
) -> OpenClawEventFact {
    event_fact(event_index, line_number, row, occurred_at)
}

pub(super) fn event_fact(
    event_index: u64,
    line_number: usize,
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
    let retained_text = provider_policy_event_text(event_type, &text, row);
    OpenClawEventFact {
        provider_event_index: event_index,
        provider_event_hash: row.get("id").and_then(Value::as_str).map(str::to_owned),
        cursor: format!("line:{line_number}"),
        event_type,
        role,
        occurred_at,
        lexical_text: text.clone(),
        payload: json!({
            "text": retained_text.text,
            "text_retention": retained_text.retention.as_json(),
            "result_evidence": provider_result_identifier_evidence(event_type, &text, row),
            "result_outcome": provider_result_outcome_evidence(event_type, row),
            "source_format": OPENCLAW_SOURCE_FORMAT,
        }),
        metadata: json!({
            "source": "openclaw_jsonl",
            "source_format": OPENCLAW_SOURCE_FORMAT,
            "row_type": row_type,
            "message_id": row.get("id").and_then(Value::as_str),
            "parent_id": row.get("parentId").or_else(|| row.get("parent_id")).cloned(),
        }),
    }
}
