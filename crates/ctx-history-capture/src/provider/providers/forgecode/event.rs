use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use ctx_history_core::{Confidence, EventRole, EventType, FileChangeKind};
use serde::Serialize;
use serde_json::{json, Value};

use crate::provider::file_touches::normalized_key;
use crate::provider::normalization::{
    provider_capped_json, provider_capped_json_value, provider_line_from_index,
    provider_normalized_result_value, provider_policy_body, provider_result_identifier_evidence,
    provider_result_outcome_evidence, provider_role, provider_timestamp_value, provider_value_text,
};
use crate::provider::providers::goose::goose_timestamp;
use crate::{compute_payload_hash, FORGECODE_SQLITE_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS};

#[derive(Debug, Clone, Copy)]
pub(super) struct ForgeCodeMessageParts<'a> {
    pub(super) variant: &'static str,
    pub(super) body: &'a Value,
    pub(super) usage: Option<&'a Value>,
}

pub(super) fn forgecode_message_parts(entry: &Value) -> ForgeCodeMessageParts<'_> {
    let message = entry.get("message").unwrap_or(entry);
    let usage = entry.get("usage");
    if let Some((variant, body)) = forgecode_message_variant(message) {
        return ForgeCodeMessageParts {
            variant,
            body,
            usage,
        };
    }
    ForgeCodeMessageParts {
        variant: "unknown",
        body: message,
        usage,
    }
}

fn forgecode_message_variant(value: &Value) -> Option<(&'static str, &Value)> {
    let Value::Object(object) = value else {
        return None;
    };
    object
        .iter()
        .find_map(|(key, value)| match normalized_key(key).as_str() {
            "text" => Some(("text", value)),
            "tool" => Some(("tool", value)),
            "image" => Some(("image", value)),
            _ => None,
        })
}

pub(super) fn forgecode_event_type(parts: ForgeCodeMessageParts<'_>) -> EventType {
    match parts.variant {
        "text" if forgecode_text_has_tool_calls(parts.body) => EventType::ToolCall,
        "text" => EventType::Message,
        "tool" => EventType::ToolOutput,
        "image" => EventType::Artifact,
        _ => EventType::Notice,
    }
}

pub(super) fn forgecode_tool_result_is_error(parts: ForgeCodeMessageParts<'_>) -> Option<bool> {
    (parts.variant == "tool")
        .then(|| {
            parts
                .body
                .pointer("/output/is_error")
                .and_then(Value::as_bool)
        })
        .flatten()
}

pub(super) fn forgecode_tool_result_call_id(parts: ForgeCodeMessageParts<'_>) -> Option<String> {
    (parts.variant == "tool")
        .then(|| parts.body.get("call_id").and_then(forgecode_scalar_text))
        .flatten()
}

pub(super) fn forgecode_event_role(parts: ForgeCodeMessageParts<'_>) -> Option<EventRole> {
    match parts.variant {
        "text" => forgecode_role_text(parts).map(|role| provider_role(Some(&role))),
        "tool" => Some(EventRole::Tool),
        "image" => Some(EventRole::Unknown),
        _ => None,
    }
}

pub(super) fn forgecode_role_text(parts: ForgeCodeMessageParts<'_>) -> Option<String> {
    forgecode_text_body(parts)
        .and_then(|body| body.get("role"))
        .and_then(Value::as_str)
        .map(|role| role.to_ascii_lowercase())
}

pub(super) fn forgecode_text_body(parts: ForgeCodeMessageParts<'_>) -> Option<&Value> {
    (parts.variant == "text").then_some(parts.body)
}

fn forgecode_text_has_tool_calls(body: &Value) -> bool {
    body.get("tool_calls")
        .or_else(|| body.get("toolCalls"))
        .and_then(Value::as_array)
        .is_some_and(|calls| !calls.is_empty())
}

pub(super) fn forgecode_message_text(
    parts: ForgeCodeMessageParts<'_>,
    event_type: EventType,
) -> String {
    match parts.variant {
        "text" => forgecode_text_message_text(parts.body, event_type),
        "tool" => forgecode_tool_result_text(parts.body),
        "image" => forgecode_image_text(parts.body),
        _ => provider_value_text(parts.body).unwrap_or_default(),
    }
}

pub(super) fn forgecode_event(
    provider_session_id: &str,
    entry: &Value,
    provider_event_index: u64,
    occurred_at: DateTime<Utc>,
) -> ForgeCodeNativeEvent {
    let parts = forgecode_message_parts(entry);
    let event_type = forgecode_event_type(parts);
    let text = forgecode_message_text(parts, event_type);
    let body = json!({
        "message_index": provider_event_index,
        "message_variant": parts.variant,
        "message": entry,
        "usage": parts.usage,
    });
    let retained_body = provider_policy_body(event_type, &body);
    ForgeCodeNativeEvent {
        provider_event_index,
        provider_event_hash: compute_payload_hash(entry).ok(),
        cursor: format!("conversation:{provider_session_id}:message:{provider_event_index}"),
        event_type,
        role: forgecode_event_role(parts),
        occurred_at,
        payload: json!({
            "text": text,
            "text_retention": {
                "mode": "none",
                "limit_chars": Value::Null,
                "truncated": false,
                "omission_policy": "none",
                "omission_applied": false,
            },
            "result_evidence": provider_result_identifier_evidence(event_type, &text, &body),
            "result_outcome": provider_result_outcome_evidence(event_type, &body),
            "source_format": FORGECODE_SQLITE_SOURCE_FORMAT,
            "body": provider_capped_json(&retained_body, PROVIDER_MAX_PREVIEW_CHARS),
        }),
        metadata: json!({
            "source": "forgecode_conversations",
            "source_format": FORGECODE_SQLITE_SOURCE_FORMAT,
            "conversation_id": provider_session_id,
            "message_index": provider_event_index,
            "message_variant": parts.variant,
            "role": forgecode_role_text(parts),
            "model": forgecode_text_body(parts)
                .and_then(|body| body.get("model"))
                .and_then(provider_value_text),
            "usage": parts.usage
                .map(|value| provider_capped_json_value(value, PROVIDER_MAX_PREVIEW_CHARS)),
        }),
    }
}

#[derive(Debug)]
pub(super) struct ForgeCodeNativeEvent {
    // Keep the native sequence with the event for non-Core materializers.
    #[allow(dead_code)]
    pub(super) provider_event_index: u64,
    pub(super) provider_event_hash: Option<String>,
    pub(super) cursor: String,
    pub(super) event_type: EventType,
    pub(super) role: Option<EventRole>,
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) payload: Value,
    pub(super) metadata: Value,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct ForgeCodeFileTouch {
    pub(super) provider_touch_index: u64,
    pub(super) provider_event_index: Option<u64>,
    pub(super) raw_source_path: Option<String>,
    pub(super) source_root: Option<String>,
    pub(super) path: String,
    pub(super) change_kind: Option<FileChangeKind>,
    pub(super) old_path: Option<String>,
    pub(super) line_count_delta: Option<i64>,
    pub(super) confidence: Confidence,
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) metadata: Value,
}

pub(super) fn forgecode_text_message_text(body: &Value, _event_type: EventType) -> String {
    let mut parts = Vec::new();
    if let Some(content) = body
        .get("content")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
    {
        parts.push(content.to_owned());
    }
    if let Some(tool_text) = body
        .get("tool_calls")
        .or_else(|| body.get("toolCalls"))
        .and_then(forgecode_tool_calls_text)
    {
        parts.push(tool_text);
    }
    if parts.is_empty() {
        if let Some(raw_content) = body.get("raw_content").and_then(provider_value_text) {
            parts.push(raw_content);
        }
    }
    parts.join("\n")
}

fn forgecode_tool_calls_text(value: &Value) -> Option<String> {
    let calls = value.as_array()?;
    let mut parts = Vec::new();
    for call in calls {
        let name = call
            .get("name")
            .and_then(forgecode_scalar_text)
            .unwrap_or_else(|| "tool".to_owned());
        parts.push(format!("tool call: {name}"));
        if let Some(call_id) = call.get("call_id").and_then(forgecode_scalar_text) {
            parts.push(format!("tool call id: {call_id}"));
        }
        if let Some(arguments) = call
            .get("arguments")
            .and_then(provider_value_text)
            .filter(|text| !text.trim().is_empty())
        {
            parts.push(format!("tool input: {arguments}"));
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn forgecode_tool_result_text(body: &Value) -> String {
    let name = body
        .get("name")
        .and_then(forgecode_scalar_text)
        .unwrap_or_else(|| "tool".to_owned());
    let mut parts = vec![format!("tool result: {name}")];
    if let Some(call_id) = body.get("call_id").and_then(forgecode_scalar_text) {
        parts.push(format!("tool call id: {call_id}"));
    }
    if body
        .pointer("/output/is_error")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        parts.push("tool error".to_owned());
    }
    if let Some(content) = forgecode_normalized_result_content(body) {
        parts.push(content);
    }
    parts.join("\n")
}

/// Returns ForgeCode's complete normalized tool-result body.
///
/// The DTO owns an ordered `output.values` list. Variant selection below has
/// explicit precedence and never searches arbitrary descendants for an
/// output-looking field. The caller owns any byte bound.
pub(crate) fn forgecode_normalized_result_content(body: &Value) -> Option<String> {
    let values = body.pointer("/output/values").and_then(Value::as_array)?;
    let parts = values
        .iter()
        .filter_map(forgecode_tool_value_text)
        .collect::<Vec<_>>();
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn forgecode_tool_value_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Object(object) => {
            if let Some(child) = object_value_by_normalized_key(object, "text")
                .or_else(|| object_value_by_normalized_key(object, "markdown"))
            {
                return child.as_str().map(str::to_owned);
            }
            if let Some(child) = object_value_by_normalized_key(object, "ai") {
                return child
                    .get("value")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| Some(provider_normalized_result_value(child)));
            }
            if let Some(child) = object_value_by_normalized_key(object, "image") {
                return Some(forgecode_image_text(child));
            }
            if let Some(child) = object_value_by_normalized_key(object, "filediff") {
                let path = child
                    .get("path")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                return Some(format!("[File diff: {path}]"));
            }
            if let Some(items) =
                object_value_by_normalized_key(object, "pair").and_then(Value::as_array)
            {
                return items.first().and_then(forgecode_tool_value_text);
            }
            if object_value_by_normalized_key(object, "empty").is_some() {
                return None;
            }
            Some(provider_normalized_result_value(value))
        }
        Value::Array(items) => {
            let parts = items
                .iter()
                .filter_map(forgecode_tool_value_text)
                .collect::<Vec<_>>();
            (!parts.is_empty()).then(|| parts.join("\n"))
        }
        Value::Number(_) | Value::Bool(_) => Some(value.to_string()),
        Value::Null => None,
    }
}

fn object_value_by_normalized_key<'a>(
    object: &'a serde_json::Map<String, Value>,
    expected: &str,
) -> Option<&'a Value> {
    object
        .iter()
        .find(|(key, _)| normalized_key(key) == expected)
        .map(|(_, value)| value)
}

fn forgecode_image_text(body: &Value) -> String {
    let mime_type = body
        .get("mime_type")
        .or_else(|| body.get("mimeType"))
        .and_then(Value::as_str)
        .unwrap_or("image");
    let url = body
        .get("url")
        .and_then(Value::as_str)
        .filter(|url| !url.trim().is_empty());
    match url {
        Some(url) => format!("ForgeCode image: {mime_type} {url}"),
        None => format!("ForgeCode image: {mime_type}"),
    }
}

fn forgecode_scalar_text(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| provider_value_text(value))
}

pub(super) fn forgecode_for_each_metric_file_touch_with_limit<E>(
    metrics: &Value,
    raw_source_path: &str,
    fallback: DateTime<Utc>,
    touch_limit: usize,
    mut emit: impl FnMut((usize, ForgeCodeFileTouch)) -> std::result::Result<(), E>,
) -> std::result::Result<bool, E> {
    let touch_limit = u64::try_from(touch_limit).unwrap_or(u64::MAX);
    let occurred_at = metrics
        .get("started_at")
        .map(|value| provider_timestamp_value(Some(value), fallback))
        .unwrap_or(fallback);
    let mut emitted_count = 0_u64;
    let mut seen = BTreeSet::<(String, &'static str)>::new();

    if let Some(files_changed) = metrics.get("files_changed").and_then(Value::as_object) {
        for (path, operation_value) in files_changed {
            let Some(operation) = forgecode_metric_operation(operation_value) else {
                continue;
            };
            let tool = operation
                .get("tool")
                .and_then(Value::as_str)
                .unwrap_or("write");
            let change_kind = forgecode_metric_change_kind(tool);
            let key = (path.clone(), change_kind.as_str());
            if seen.contains(&key) {
                continue;
            }
            if emitted_count == touch_limit {
                return Ok(true);
            }
            seen.insert(key);
            let lines_added = operation.get("lines_added").and_then(forgecode_json_i64);
            let lines_removed = operation.get("lines_removed").and_then(forgecode_json_i64);
            let line_count_delta = match (lines_added, lines_removed) {
                (Some(added), Some(removed)) => Some(added.saturating_sub(removed)),
                (Some(added), None) => Some(added),
                (None, Some(removed)) => Some(removed.saturating_neg()),
                _ => None,
            };
            let touch_index = 0x0400_0000_0000_u64.saturating_add(emitted_count);
            emit((
                provider_line_from_index(touch_index),
                ForgeCodeFileTouch {
                    provider_touch_index: touch_index,
                    provider_event_index: None,
                    raw_source_path: Some(raw_source_path.to_owned()),
                    source_root: Some(raw_source_path.to_owned()),
                    path: path.clone(),
                    change_kind: Some(change_kind),
                    old_path: None,
                    line_count_delta,
                    confidence: Confidence::Explicit,
                    occurred_at,
                    metadata: json!({
                        "source": "forgecode_metrics_files_changed",
                        "tool": tool,
                        "lines_added": lines_added,
                        "lines_removed": lines_removed,
                        "content_hash": operation.get("content_hash").and_then(Value::as_str),
                    }),
                },
            ))?;
            emitted_count = emitted_count.saturating_add(1);
        }
    }

    if let Some(files_accessed) = metrics.get("files_accessed").and_then(Value::as_array) {
        for path in files_accessed
            .iter()
            .filter_map(Value::as_str)
            .filter(|path| !path.trim().is_empty())
        {
            let key = (path.to_owned(), FileChangeKind::Read.as_str());
            if seen.contains(&key) {
                continue;
            }
            if emitted_count == touch_limit {
                return Ok(true);
            }
            seen.insert(key);
            let touch_index = 0x0500_0000_0000_u64.saturating_add(emitted_count);
            emit((
                provider_line_from_index(touch_index),
                ForgeCodeFileTouch {
                    provider_touch_index: touch_index,
                    provider_event_index: None,
                    raw_source_path: Some(raw_source_path.to_owned()),
                    source_root: Some(raw_source_path.to_owned()),
                    path: path.to_owned(),
                    change_kind: Some(FileChangeKind::Read),
                    old_path: None,
                    line_count_delta: None,
                    confidence: Confidence::Explicit,
                    occurred_at,
                    metadata: json!({
                        "source": "forgecode_metrics_files_accessed",
                    }),
                },
            ))?;
            emitted_count = emitted_count.saturating_add(1);
        }
    }

    Ok(false)
}

fn forgecode_metric_operation(value: &Value) -> Option<&Value> {
    match value {
        Value::Object(_) => Some(value),
        Value::Array(items) => items.iter().rev().find(|item| item.is_object()),
        _ => None,
    }
}

fn forgecode_metric_change_kind(tool: &str) -> FileChangeKind {
    match tool.to_ascii_lowercase().as_str() {
        "read" => FileChangeKind::Read,
        "patch" | "edit" | "update" | "write" => FileChangeKind::Modified,
        "delete" | "remove" => FileChangeKind::Deleted,
        "create" | "add" => FileChangeKind::Created,
        _ => FileChangeKind::Unknown,
    }
}

fn forgecode_json_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
}

pub(super) fn forgecode_timestamp(raw: Option<&str>, fallback: DateTime<Utc>) -> DateTime<Utc> {
    goose_timestamp(raw, fallback)
}
