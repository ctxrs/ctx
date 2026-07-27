use chrono::{DateTime, Utc};
use ctx_history_core::{EventRole, EventType, Fidelity};
use serde_json::{json, Value};

use crate::provider::normalization::{
    provider_capped_json, provider_policy_body, provider_policy_event_text,
    provider_result_identifier_evidence, provider_result_outcome_evidence, provider_role,
};
use crate::provider::providers::task_json::{task_json_string_field, task_json_time_field};
use crate::PROVIDER_MAX_PREVIEW_CHARS;

use super::{TRAE_CN_INPUT_HISTORY_KEY, TRAE_STATE_VSCDB_SOURCE_FORMAT};

#[derive(Debug, Clone)]
pub(super) struct TraeEventInput {
    pub(super) provider_event_index: u64,
    pub(super) native_message_id: String,
    pub(super) role: Option<String>,
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) text: String,
    pub(super) raw_message: Value,
}

pub(super) struct TraeCoreEvent {
    pub(super) provider_event_index: u64,
    pub(super) provider_event_hash: String,
    pub(super) cursor: String,
    pub(super) event_type: EventType,
    pub(super) role: Option<EventRole>,
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) fidelity: Fidelity,
    pub(super) idempotency_key: String,
    pub(super) payload: Value,
    pub(super) metadata: Value,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn trae_event_from_owned_message(
    provider_session_id: &str,
    workspace_id: &str,
    chat_key: &str,
    message: Value,
    message_index: usize,
    fallback_time: DateTime<Utc>,
) -> Option<TraeEventInput> {
    let text = trae_message_text(&message)?;
    if text.trim().is_empty() {
        return None;
    }
    let native_message_id = task_json_string_field(
        &message,
        &[
            "id",
            "messageId",
            "message_id",
            "uuid",
            "requestId",
            "responseId",
        ],
    )
    .unwrap_or_else(|| format!("{workspace_id}:{provider_session_id}:{chat_key}:{message_index}"));
    let occurred_at = task_json_time_field(
        &message,
        &["createdAt", "created_at", "timestamp", "time", "date"],
    )
    .unwrap_or(fallback_time);
    let mut role = task_json_string_field(&message, &["role", "type", "sender"]);
    if chat_key == TRAE_CN_INPUT_HISTORY_KEY && role.is_none() {
        role = Some("user".to_owned());
    }
    Some(TraeEventInput {
        provider_event_index: u64::try_from(message_index).unwrap_or(u64::MAX),
        native_message_id,
        role,
        occurred_at,
        text,
        raw_message: message,
    })
}

pub(super) fn trae_message_text(message: &Value) -> Option<String> {
    for field in [
        "content",
        "inputText",
        "text",
        "message",
        "summary",
        "answer",
        "query",
        "parsedQuery",
        "output",
        "result",
        "error",
    ] {
        if let Some(text) = message.get(field).and_then(trae_content_text) {
            return Some(text);
        }
    }
    message
        .pointer("/data/summary")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

pub(super) fn trae_content_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.trim().to_owned()),
        Value::Array(items) => {
            let parts = items
                .iter()
                .filter_map(trae_content_text)
                .filter(|text| !text.trim().is_empty())
                .collect::<Vec<_>>();
            (!parts.is_empty()).then(|| parts.join("\n"))
        }
        Value::Object(map) => {
            for field in [
                "text", "content", "value", "summary", "output", "result", "error",
            ] {
                if let Some(text) = map.get(field).and_then(trae_content_text) {
                    return Some(text);
                }
            }
            None
        }
        _ => None,
    }
}

pub(super) fn trae_session_metadata_preview(session: &Value) -> Value {
    provider_policy_body(EventType::Notice, &trae_session_metadata_source(session))
}

fn trae_session_metadata_source(session: &Value) -> Value {
    let Value::Object(object) = session else {
        return session.clone();
    };
    let mut preview = serde_json::Map::new();
    for (key, value) in object {
        if !["messages", "chatMessages", "bubbles", "items"].contains(&key.as_str()) {
            preview.insert(key.clone(), value.clone());
        }
    }
    Value::Object(preview)
}

pub(super) fn trae_core_event(
    provider_session_id: &str,
    workspace_id: &str,
    chat_key: &str,
    event: &TraeEventInput,
) -> TraeCoreEvent {
    let event_type = EventType::Message;
    let retained_text = provider_policy_event_text(event_type, &event.text, &event.raw_message);
    let result_evidence =
        provider_result_identifier_evidence(event_type, &event.text, &event.raw_message);
    let result_outcome = provider_result_outcome_evidence(event_type, &event.raw_message);
    let event_id = format!("{provider_session_id}:{}", event.native_message_id);
    TraeCoreEvent {
        provider_event_index: event.provider_event_index,
        provider_event_hash: event_id.clone(),
        cursor: format!("{chat_key}:{event_id}"),
        event_type,
        role: Some(provider_role(event.role.as_deref())),
        occurred_at: event.occurred_at,
        fidelity: Fidelity::Partial,
        idempotency_key: format!("provider-event:trae:{TRAE_STATE_VSCDB_SOURCE_FORMAT}:{event_id}"),
        payload: json!({
            "event_id": event_id,
            "native_workspace_id": workspace_id,
            "native_message_id": event.native_message_id,
            "text": retained_text.text,
            "text_retention": retained_text.retention.as_json(),
            "result_evidence": result_evidence,
            "result_outcome": result_outcome,
            "body": provider_capped_json(&provider_policy_body(event_type, &event.raw_message), PROVIDER_MAX_PREVIEW_CHARS),
        }),
        metadata: json!({
            "source": "trae_state_vscdb_itemtable",
            "source_format": TRAE_STATE_VSCDB_SOURCE_FORMAT,
            "chat_key": chat_key,
            "native_message_id": event.native_message_id,
            "role": event.role,
            "model": task_json_string_field(&event.raw_message, &["model", "modelType", "model_id"]),
        }),
    }
}
