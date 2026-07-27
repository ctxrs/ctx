use chrono::{DateTime, Utc};
use ctx_history_core::{EventRole, EventType};
use serde_json::{json, Value};

use crate::provider::normalization::{
    provider_block_event_type, provider_block_text, provider_capped_json,
    provider_explicit_result_value_text, provider_message_id, provider_message_parts,
    provider_policy_body, provider_policy_event_text, provider_result_identifier_evidence,
    provider_result_outcome_evidence, provider_role_from_message,
};
use crate::{PROVIDER_MAX_PREVIEW_CHARS, ROVODEV_SOURCE_FORMAT};

use super::source::RovoDevSessionSource;

pub(super) fn rovodev_event_type(message: &Value, role_text: Option<&str>) -> EventType {
    if role_text.is_some_and(|role| {
        matches!(
            role.trim().to_ascii_lowercase().as_str(),
            "tool" | "tool_result" | "tool-result" | "tool_use_result" | "function_result"
        )
    }) {
        EventType::ToolOutput
    } else {
        provider_block_event_type(message, role_text)
    }
}

#[derive(Debug)]
pub(super) struct RovoDevCoreEvent {
    pub(super) provider_event_index: u64,
    pub(super) provider_event_hash: String,
    pub(super) cursor: String,
    pub(super) event_type: EventType,
    pub(super) role: Option<EventRole>,
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) payload: Value,
    pub(super) metadata: Value,
}

impl RovoDevCoreEvent {
    pub(super) fn estimated_bytes(&self) -> usize {
        self.provider_event_hash
            .len()
            .saturating_add(self.cursor.len())
            .saturating_add(serde_json::to_vec(&self.payload).map_or(0, |payload| payload.len()))
            .saturating_add(serde_json::to_vec(&self.metadata).map_or(0, |metadata| metadata.len()))
            .saturating_add(512)
    }
}

pub(super) fn rovodev_event(
    event_index: u64,
    message: &Value,
    occurred_at: DateTime<Utc>,
    source: &RovoDevSessionSource,
) -> RovoDevCoreEvent {
    let role_text = message
        .get("role")
        .or_else(|| message.get("kind"))
        .or_else(|| message.get("type"))
        .and_then(Value::as_str);
    let event_type = rovodev_event_type(message, role_text);
    let text = provider_block_text(message).unwrap_or_default();
    let body = message.clone();
    let retained_text = provider_policy_event_text(event_type, &text, &body);
    let retained_body = provider_policy_body(event_type, &body);
    let result_evidence = provider_result_identifier_evidence(event_type, &text, &body);
    let result_outcome = provider_result_outcome_evidence(event_type, &body);
    RovoDevCoreEvent {
        provider_event_index: event_index,
        provider_event_hash: provider_message_id(message, event_index),
        cursor: format!(
            "{}:{}",
            source.context_path.display(),
            provider_message_id(message, event_index)
        ),
        event_type,
        role: Some(provider_role_from_message(message, role_text)),
        occurred_at,
        payload: json!({
            "text": retained_text.text,
            "text_retention": retained_text.retention.as_json(),
            "result_evidence": result_evidence,
            "result_outcome": result_outcome,
            "source_format": ROVODEV_SOURCE_FORMAT,
            "body": provider_capped_json(&retained_body, PROVIDER_MAX_PREVIEW_CHARS),
        }),
        metadata: json!({
            "source": ROVODEV_SOURCE_FORMAT,
            "source_format": ROVODEV_SOURCE_FORMAT,
            "message_id": provider_message_id(message, event_index),
            "role": role_text,
            "kind": message.get("kind").and_then(Value::as_str),
            "part_count": provider_message_parts(message).map(|parts| parts.len()),
        }),
    }
}

/// Extracts exact Rovo Dev result-part content without the shared display
/// normalizer's `tool result` fallback.
pub(crate) fn rovodev_result_content(message: &Value) -> Option<String> {
    let role_text = message
        .get("role")
        .or_else(|| message.get("kind"))
        .or_else(|| message.get("type"))
        .and_then(Value::as_str);
    if rovodev_event_type(message, role_text) != EventType::ToolOutput {
        return None;
    }

    if let Some(parts) = provider_message_parts(message) {
        let mut results = Vec::new();
        for part in parts {
            let kind = part
                .get("type")
                .or_else(|| part.get("kind"))
                .and_then(Value::as_str);
            if !matches!(
                kind,
                Some("tool_result" | "tool-result" | "tool_use_result" | "function_result")
            ) {
                continue;
            }
            if let Some(text) = part
                .get("content")
                .or_else(|| part.get("result"))
                .or_else(|| part.get("output"))
                .and_then(provider_explicit_result_value_text)
            {
                results.push(text);
            }
        }
        return (!results.is_empty()).then(|| results.join("\n"));
    }

    ["content", "result", "output"]
        .into_iter()
        .find_map(|field| {
            message
                .get(field)
                .and_then(provider_explicit_result_value_text)
        })
}
