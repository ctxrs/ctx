use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, EventType, ProviderEventEnvelope};
use serde_json::{json, Value};

use crate::provider::normalization::{
    native_event, provider_block_event_type, provider_block_text,
    provider_explicit_result_value_text, provider_message_id, provider_message_parts,
    provider_role_from_message, NativeEventDraft,
};
use crate::ROVODEV_SOURCE_FORMAT;

use super::source::RovoDevSessionSource;

pub(super) fn rovodev_event(
    provider_session_id: &str,
    event_index: u64,
    message: &Value,
    occurred_at: DateTime<Utc>,
    source: &RovoDevSessionSource,
) -> ProviderEventEnvelope {
    let role_text = message
        .get("role")
        .or_else(|| message.get("kind"))
        .or_else(|| message.get("type"))
        .and_then(Value::as_str);
    native_event(NativeEventDraft {
        provider: CaptureProvider::RovoDev,
        source_format: ROVODEV_SOURCE_FORMAT,
        provider_session_id: provider_session_id.to_owned(),
        provider_event_index: event_index,
        provider_event_hash: Some(provider_message_id(message, event_index)),
        cursor: format!(
            "{}:{}",
            source.context_path.display(),
            provider_message_id(message, event_index)
        ),
        event_type: provider_block_event_type(message, role_text),
        role: Some(provider_role_from_message(message, role_text)),
        occurred_at,
        text: provider_block_text(message).unwrap_or_default(),
        body: message.clone(),
        metadata: json!({
            "source": ROVODEV_SOURCE_FORMAT,
            "source_format": ROVODEV_SOURCE_FORMAT,
            "message_id": provider_message_id(message, event_index),
            "role": role_text,
            "kind": message.get("kind").and_then(Value::as_str),
            "part_count": provider_message_parts(message).map(|parts| parts.len()),
        }),
    })
}

/// Extracts exact Rovo Dev result-part content without the shared display
/// normalizer's `tool result` fallback.
pub(crate) fn rovodev_result_content(message: &Value) -> Option<String> {
    let role_text = message
        .get("role")
        .or_else(|| message.get("kind"))
        .or_else(|| message.get("type"))
        .and_then(Value::as_str);
    if provider_block_event_type(message, role_text) != EventType::ToolOutput {
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
