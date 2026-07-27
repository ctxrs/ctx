use chrono::{DateTime, Utc};
use ctx_history_core::{EventRole, EventType, Fidelity};
use serde_json::{json, Value};

use crate::common::time::parse_rfc3339_utc;
use crate::provider::normalization::{
    provider_capped_json, provider_explicit_result_value_text, provider_policy_body,
    provider_policy_event_text, provider_result_identifier_evidence,
    provider_result_outcome_evidence, provider_role, provider_value_text,
};
use crate::PROVIDER_MAX_PREVIEW_CHARS;

use super::dialect::TaskJsonProviderSpec;

pub(crate) fn task_json_string_field(value: &Value, fields: &[&str]) -> Option<String> {
    fields
        .iter()
        .find_map(|field| value.get(*field).and_then(Value::as_str))
        .filter(|text| !text.trim().is_empty())
        .map(str::to_owned)
}

pub(crate) fn task_json_time_field(value: &Value, fields: &[&str]) -> Option<DateTime<Utc>> {
    for field in fields {
        let Some(value) = value.get(*field) else {
            continue;
        };
        if let Some(text) = value.as_str() {
            if let Some(parsed) = parse_rfc3339_utc(text) {
                return Some(parsed);
            }
            if let Ok(number) = text.parse::<i64>() {
                if let Some(parsed) = task_json_timestamp_number(number) {
                    return Some(parsed);
                }
            }
        }
        if let Some(number) = value.as_i64().and_then(task_json_timestamp_number) {
            return Some(number);
        }
    }
    None
}

fn task_json_timestamp_number(value: i64) -> Option<DateTime<Utc>> {
    if value > 10_000_000_000 {
        DateTime::<Utc>::from_timestamp_millis(value)
    } else {
        DateTime::<Utc>::from_timestamp(value, 0)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct TaskJsonEventInput {
    pub(crate) source: &'static str,
    pub(crate) native_index: usize,
    pub(crate) raw: Value,
}

pub(super) struct TaskJsonEventDraft {
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

pub(super) fn task_json_event_draft(
    spec: TaskJsonProviderSpec,
    task_id: &str,
    input: TaskJsonEventInput,
    event_ordinal: usize,
    occurred_at: DateTime<Utc>,
) -> TaskJsonEventDraft {
    let event_type = task_json_event_type(&input.raw, input.source);
    let role = Some(task_json_event_role(&input.raw, input.source));
    let text = task_json_event_text(&input.raw, input.source, event_type);
    let retained_text = provider_policy_event_text(event_type, &text, &input.raw);
    let result_evidence = provider_result_identifier_evidence(event_type, &text, &input.raw);
    let result_outcome = provider_result_outcome_evidence(event_type, &input.raw);
    let native_id = task_json_string_field(&input.raw, &["id", "uuid", "messageId"])
        .unwrap_or_else(|| format!("{}-{}", input.source, input.native_index));
    let event_id = format!("{task_id}:{}:{native_id}", input.source);

    TaskJsonEventDraft {
        provider_event_index: event_ordinal as u64,
        provider_event_hash: event_id.clone(),
        cursor: event_id.clone(),
        event_type,
        role,
        occurred_at,
        fidelity: Fidelity::Imported,
        idempotency_key: format!(
            "provider-event:{}:{}:{event_id}",
            spec.provider.as_str(),
            spec.source_format
        ),
        payload: json!({
            "entry_type": task_json_entry_type(&input.raw, input.source),
            "event_id": event_id,
            "native_index": input.native_index,
            "text": retained_text.text,
            "text_retention": retained_text.retention.as_json(),
            "result_evidence": result_evidence,
            "result_outcome": result_outcome,
            "body": provider_capped_json(&provider_policy_body(event_type, &input.raw), PROVIDER_MAX_PREVIEW_CHARS),
        }),
        metadata: json!({
            "source": input.source,
            "source_format": spec.source_format,
            "native_index": input.native_index,
            "role": task_json_string_field(&input.raw, &["role"]),
            "model": task_json_model(&input.raw),
            "usage": task_json_usage(&input.raw),
        }),
    }
}

pub(crate) fn task_json_event_type(value: &Value, source: &str) -> EventType {
    if task_json_content_has(value, "tool_result") {
        return EventType::ToolOutput;
    }
    if task_json_content_has(value, "tool_use") {
        return EventType::ToolCall;
    }
    match source {
        "ui_messages" => match task_json_string_field(value, &["type", "say", "ask"]).as_deref() {
            Some("ask" | "say" | "user" | "assistant" | "text") => EventType::Message,
            Some("command" | "execute_command" | "shell") => EventType::CommandOutput,
            Some("completion_result" | "summary") => EventType::Summary,
            _ => EventType::Notice,
        },
        _ => match task_json_string_field(value, &["type", "role"]).as_deref() {
            Some("user" | "assistant" | "system") => EventType::Message,
            Some("tool_result") => EventType::ToolOutput,
            Some("tool_use") => EventType::ToolCall,
            Some("history_item" | "summary") => EventType::Summary,
            _ => EventType::Message,
        },
    }
}

fn task_json_event_role(value: &Value, source: &str) -> EventRole {
    if let Some(role) = task_json_string_field(value, &["role"]) {
        return provider_role(Some(&role));
    }
    if source == "ui_messages" {
        match task_json_string_field(value, &["type"]).as_deref() {
            Some("ask") => EventRole::User,
            Some("say") => EventRole::Assistant,
            _ => EventRole::Unknown,
        }
    } else {
        EventRole::Unknown
    }
}

pub(crate) fn task_json_event_text(value: &Value, source: &str, event_type: EventType) -> String {
    value
        .get("content")
        .or_else(|| value.pointer("/message/content"))
        .and_then(provider_value_text)
        .or_else(|| value.get("text").and_then(Value::as_str).map(str::to_owned))
        .or_else(|| value.get("message").and_then(provider_value_text))
        .or_else(|| {
            value
                .get("summary")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| {
            if event_type == EventType::Notice {
                format!("Task JSON event: {}", task_json_entry_type(value, source))
            } else {
                serde_json::to_string(value).unwrap_or_else(|_| source.to_owned())
            }
        })
}

/// Extracts explicit result bytes shared by the Cline and Roo task-JSON
/// dialects. The whole-record display fallback is intentionally excluded.
pub(crate) fn task_json_result_content(value: &Value, source: &str) -> Option<String> {
    if !matches!(
        task_json_event_type(value, source),
        EventType::ToolOutput | EventType::CommandOutput
    ) {
        return None;
    }

    let content = value
        .get("content")
        .or_else(|| value.pointer("/message/content"));
    if let Some(blocks) = content.and_then(Value::as_array) {
        let mut parts = Vec::new();
        for block in blocks {
            let kind = block
                .get("type")
                .or_else(|| block.get("kind"))
                .and_then(Value::as_str);
            if !matches!(
                kind,
                Some("tool_result" | "tool-result" | "tool_use_result" | "function_result")
            ) {
                continue;
            }
            if let Some(text) = block
                .get("content")
                .or_else(|| block.get("result"))
                .or_else(|| block.get("output"))
                .or_else(|| block.get("text"))
                .and_then(provider_explicit_result_value_text)
            {
                parts.push(text);
            }
        }
        if !parts.is_empty() {
            return Some(parts.join("\n"));
        }
        return None;
    }

    ["output", "result", "text", "content", "message"]
        .into_iter()
        .find_map(|field| {
            value
                .get(field)
                .and_then(provider_explicit_result_value_text)
        })
}

fn task_json_entry_type(value: &Value, source: &str) -> String {
    task_json_string_field(value, &["type", "say", "ask", "role"])
        .unwrap_or_else(|| source.to_owned())
}

fn task_json_content_has(value: &Value, expected: &str) -> bool {
    value
        .get("content")
        .or_else(|| value.pointer("/message/content"))
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .any(|block| block.get("type").and_then(Value::as_str) == Some(expected))
        })
        .unwrap_or(false)
}

fn task_json_model(value: &Value) -> Option<Value> {
    value
        .get("model")
        .or_else(|| value.pointer("/modelInfo/id"))
        .or_else(|| value.pointer("/metadata/model"))
        .cloned()
}

fn task_json_usage(value: &Value) -> Option<Value> {
    value
        .get("usage")
        .or_else(|| value.get("tokensUsed"))
        .or_else(|| value.pointer("/modelInfo/usage"))
        .cloned()
}
