use chrono::{DateTime, Utc};
use ctx_history_core::{EventRole, EventType};
use serde_json::{json, Value};

use crate::common::time::parse_rfc3339_utc;
use crate::{fnv1a64, CaptureError, Result};

pub(crate) fn provider_capped_json(value: &Value, max_chars: usize) -> Value {
    match value {
        Value::Null => Value::Null,
        Value::String(text) => {
            let (text, truncated) = provider_local_preview(text, max_chars);
            json!({ "text": text, "truncated": truncated })
        }
        _ => {
            let rendered = serde_json::to_string(value).unwrap_or_else(|_| value.to_string());
            let (json_text, truncated) = provider_local_preview(&rendered, max_chars);
            json!({ "json": json_text, "truncated": truncated })
        }
    }
}

pub(crate) fn provider_capped_json_value(value: &Value, max_string_chars: usize) -> Value {
    match value {
        Value::String(text) => {
            let (text, truncated) = provider_local_preview(text, max_string_chars);
            if truncated {
                json!({ "text": text, "truncated": true })
            } else {
                Value::String(text)
            }
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| provider_capped_json_value(item, max_string_chars))
                .collect(),
        ),
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        provider_capped_json_value(value, max_string_chars),
                    )
                })
                .collect(),
        ),
        _ => value.clone(),
    }
}

pub(crate) fn provider_nonnegative_i64_to_u64(value: i64, field: &'static str) -> Result<u64> {
    u64::try_from(value).map_err(|_| {
        CaptureError::InvalidPayload(format!("{field} must be nonnegative, got {value}"))
    })
}

pub(crate) fn provider_line_from_index(index: u64) -> usize {
    index.min(usize::MAX as u64) as usize
}

pub(crate) fn provider_timestamp_seconds_to_datetime(value: f64) -> Option<DateTime<Utc>> {
    if !value.is_finite() {
        return None;
    }
    let millis = if value.abs() > 1_000_000_000_000.0 {
        value.round()
    } else {
        (value * 1000.0).round()
    };
    if millis < i64::MIN as f64 || millis > i64::MAX as f64 {
        return None;
    }
    DateTime::<Utc>::from_timestamp_millis(millis as i64)
}

pub(crate) fn provider_timestamp_seconds(
    value: Option<f64>,
    fallback: DateTime<Utc>,
) -> DateTime<Utc> {
    value
        .and_then(provider_timestamp_seconds_to_datetime)
        .unwrap_or(fallback)
}

pub(crate) fn provider_required_timestamp_seconds(
    value: f64,
    field: &'static str,
) -> Result<DateTime<Utc>> {
    provider_timestamp_seconds_to_datetime(value).ok_or_else(|| {
        CaptureError::InvalidPayload(format!(
            "{field} is outside representable timestamp range: {value}"
        ))
    })
}

pub(crate) fn provider_timestamp_millis(
    value: Option<i64>,
    fallback: DateTime<Utc>,
) -> DateTime<Utc> {
    value
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        .unwrap_or(fallback)
}

pub(crate) fn provider_required_timestamp_millis(
    value: i64,
    field: &'static str,
) -> Result<DateTime<Utc>> {
    DateTime::<Utc>::from_timestamp_millis(value).ok_or_else(|| {
        CaptureError::InvalidPayload(format!(
            "{field} is outside representable timestamp range: {value}"
        ))
    })
}

pub(crate) fn provider_timestamp_value(
    value: Option<&Value>,
    fallback: DateTime<Utc>,
) -> DateTime<Utc> {
    match value {
        Some(Value::String(raw)) => parse_rfc3339_utc(raw)
            .or_else(|| {
                raw.parse::<f64>()
                    .ok()
                    .map(|ts| provider_timestamp_seconds(Some(ts), fallback))
            })
            .unwrap_or(fallback),
        Some(Value::Number(number)) => number
            .as_f64()
            .map(|ts| provider_timestamp_seconds(Some(ts), fallback))
            .unwrap_or(fallback),
        _ => fallback,
    }
}

pub(crate) fn text_id_index(seed: &str, offset: u64) -> u64 {
    offset.saturating_add(fnv1a64(seed.as_bytes()) & 0x0fff_ffff)
}

pub(crate) fn provider_json_text(raw: &str) -> Value {
    serde_json::from_str::<Value>(raw).unwrap_or_else(|_| Value::String(raw.to_owned()))
}

pub(crate) fn provider_value_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => {
            let mut parts = Vec::new();
            for block in blocks {
                if let Some(text) = block
                    .get("text")
                    .or_else(|| block.get("content"))
                    .or_else(|| block.get("output"))
                    .or_else(|| block.get("summary"))
                    .and_then(Value::as_str)
                {
                    parts.push(text.to_owned());
                    continue;
                }
                if let Some(kind) = block.get("type").and_then(Value::as_str) {
                    if matches!(
                        kind,
                        "tool_use" | "tool" | "toolCall" | "function_call" | "agent"
                    ) {
                        let name = block
                            .get("name")
                            .or_else(|| block.get("tool"))
                            .and_then(Value::as_str)
                            .unwrap_or("tool");
                        parts.push(format!("tool call: {name}"));
                    } else if kind == "tool_result" {
                        parts.push("tool result".to_owned());
                    }
                }
            }
            (!parts.is_empty()).then(|| parts.join("\n"))
        }
        Value::Object(_) => serde_json::to_string(value).ok(),
        Value::Number(_) | Value::Bool(_) => Some(value.to_string()),
        Value::Null => None,
    }
}

pub(crate) fn provider_role(value: Option<&str>) -> EventRole {
    match value {
        Some("user") => EventRole::User,
        Some("assistant") => EventRole::Assistant,
        Some("system" | "developer") => EventRole::System,
        Some("tool" | "toolResult" | "bashExecution") => EventRole::Tool,
        _ => EventRole::Unknown,
    }
}

pub(crate) fn capped_text(value: &str, max_chars: usize) -> (String, bool) {
    let mut out = String::new();
    let mut truncated = false;
    for (index, ch) in value.chars().enumerate() {
        if index >= max_chars {
            truncated = true;
            break;
        }
        out.push(ch);
    }
    (out, truncated)
}

pub(crate) fn provider_local_preview(value: &str, max_chars: usize) -> (String, bool) {
    capped_text(value, max_chars)
}

pub(crate) fn provider_string_field(value: &Value, fields: &[&str]) -> Option<String> {
    fields.iter().find_map(|field| {
        value
            .get(*field)
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .map(str::to_owned)
    })
}

pub(crate) fn provider_timestamp_from_fields(
    value: &Value,
    fields: &[&str],
) -> Option<DateTime<Utc>> {
    fields.iter().find_map(|field| {
        let raw = value.get(*field)?;
        match raw {
            Value::String(text) => parse_rfc3339_utc(text).or_else(|| {
                text.parse::<f64>()
                    .ok()
                    .and_then(provider_timestamp_seconds_to_datetime)
            }),
            Value::Number(number) => number
                .as_f64()
                .and_then(provider_timestamp_seconds_to_datetime),
            _ => None,
        }
    })
}

pub(crate) fn provider_message_id(value: &Value, fallback_index: u64) -> String {
    value
        .get("id")
        .or_else(|| value.get("message_id"))
        .or_else(|| value.get("messageId"))
        .or_else(|| value.get("request_id"))
        .or_else(|| value.get("requestId"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("message-{fallback_index}"))
}

pub(crate) fn provider_role_from_message(value: &Value, role_text: Option<&str>) -> EventRole {
    let role = role_text.or_else(|| value.get("kind").and_then(Value::as_str));
    match role {
        Some("user" | "human" | "user_prompt" | "user-prompt") => EventRole::User,
        Some("assistant" | "agent" | "ai" | "model") => EventRole::Assistant,
        Some("system" | "developer" | "system_prompt" | "system-prompt") => EventRole::System,
        Some("tool" | "tool_result" | "tool-result" | "tool_use_result") => EventRole::Tool,
        _ => EventRole::Unknown,
    }
}

pub(crate) fn provider_block_event_type(value: &Value, role_text: Option<&str>) -> EventType {
    let role = role_text.unwrap_or_default();
    if role.contains("tool_result")
        || role.contains("tool-result")
        || provider_message_has_part_kind(value, &["tool_result", "tool-result"])
    {
        EventType::ToolOutput
    } else if role.contains("tool_use")
        || role.contains("tool-use")
        || provider_message_has_part_kind(
            value,
            &["tool_use", "tool-use", "tool-call", "tool_call"],
        )
    {
        EventType::ToolCall
    } else if matches!(
        role,
        "system" | "developer" | "system_prompt" | "system-prompt"
    ) {
        EventType::Notice
    } else {
        EventType::Message
    }
}

pub(crate) fn provider_message_has_part_kind(value: &Value, kinds: &[&str]) -> bool {
    provider_message_parts(value)
        .map(|parts| {
            parts.iter().any(|part| {
                part.get("type")
                    .or_else(|| part.get("kind"))
                    .and_then(Value::as_str)
                    .is_some_and(|kind| kinds.contains(&kind))
            })
        })
        .unwrap_or(false)
}

pub(crate) fn provider_block_text(value: &Value) -> Option<String> {
    for key in [
        "text", "content", "message", "prompt", "response", "output", "summary",
    ] {
        if let Some(text) = value.get(key).and_then(provider_value_text) {
            if !text.trim().is_empty() {
                return Some(text);
            }
        }
    }
    let parts = provider_message_parts(value)?;
    let mut rendered = Vec::new();
    for part in parts {
        if let Some(text) = provider_part_text(part) {
            rendered.push(text);
        }
    }
    (!rendered.is_empty()).then(|| rendered.join("\n"))
}

pub(crate) fn provider_message_parts(value: &Value) -> Option<&Vec<Value>> {
    value
        .get("parts")
        .or_else(|| value.get("content"))
        .or_else(|| value.get("blocks"))
        .and_then(Value::as_array)
}

pub(crate) fn provider_part_text(part: &Value) -> Option<String> {
    let kind = part
        .get("type")
        .or_else(|| part.get("kind"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if matches!(
        kind,
        "tool_use" | "tool-use" | "tool_call" | "tool-call" | "function_call"
    ) {
        let name = part
            .get("name")
            .or_else(|| part.get("tool"))
            .or_else(|| part.get("tool_name"))
            .or_else(|| part.get("toolName"))
            .and_then(Value::as_str)
            .unwrap_or("tool");
        return Some(format!("tool call: {name}"));
    }
    if matches!(
        kind,
        "tool_result" | "tool-result" | "tool_use_result" | "function_result"
    ) {
        return part
            .get("content")
            .or_else(|| part.get("result"))
            .or_else(|| part.get("output"))
            .and_then(provider_value_text)
            .or_else(|| Some("tool result".to_owned()));
    }
    part.get("text")
        .or_else(|| part.get("content"))
        .or_else(|| part.get("thinking"))
        .or_else(|| part.get("summary"))
        .and_then(provider_value_text)
}
