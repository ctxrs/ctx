use ctx_history_core::{ContentRef, EventType};
use serde_json::{json, Value};

use crate::provider::normalization::{provider_policy_event_text, provider_value_text};

pub(crate) fn claude_event_type(entry_type: &str, message: &Value) -> EventType {
    if claude_content_has_type(message.get("content"), "tool_result")
        || message.get("toolUseResult").is_some()
    {
        return EventType::ToolOutput;
    }
    if claude_content_has_type(message.get("content"), "tool_use") {
        return EventType::ToolCall;
    }
    match entry_type {
        "user" | "assistant" => EventType::Message,
        "system"
        | "progress"
        | "permission-mode"
        | "last-prompt"
        | "queue-operation"
        | "attachment"
        | "file-history-snapshot"
        | "ai-title" => EventType::Notice,
        _ => EventType::Notice,
    }
}

fn claude_content_has_type(content: Option<&Value>, expected: &str) -> bool {
    content
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .any(|block| block.get("type").and_then(Value::as_str) == Some(expected))
        })
        .unwrap_or(false)
}

/// Pure ordinary-message normalization shared by capture and verified source
/// reopening. Compound tool records are deliberately excluded.
pub(crate) fn claude_complete_content_message_record(
    value: &Value,
    line_number: usize,
) -> Option<(String, String)> {
    let entry_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let message = value.get("message").unwrap_or(value);
    if claude_event_type(entry_type, message) != EventType::Message {
        return None;
    }
    let content = message.get("content").unwrap_or(&Value::Null);
    let text = provider_value_text(content).unwrap_or_default();
    let native_id = value
        .get("uuid")
        .and_then(Value::as_str)
        .or_else(|| message.get("id").and_then(Value::as_str))
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("line-{line_number}"));
    Some((text, native_id))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn claude_nativepath_message_hash_payload(
    native_record_id: &str,
    parent_native_record_id: Option<&str>,
    role: Option<&str>,
    occurred_at: Option<&str>,
    retained_text: &str,
    text_retention: &Value,
    content_ref: &ContentRef,
) -> Value {
    json!({
        "native_record_id": native_record_id,
        "parent_native_record_id": parent_native_record_id,
        "role": role,
        "occurred_at": occurred_at,
        "body": retained_text,
        "text_retention": text_retention,
        "complete_body_ref": content_ref,
    })
}

/// Rebuilds the exact normalized payload used by Store event hashing for a
/// Claude ordinary message without copying provider parsing into the resolver.
pub(crate) fn claude_complete_content_normalized_payload(
    value: &Value,
    line_number: usize,
) -> Option<Value> {
    let message = value.get("message").unwrap_or(value);
    let (text, native_record_id) = claude_complete_content_message_record(value, line_number)?;
    let message_role = message
        .get("role")
        .and_then(Value::as_str)
        .or_else(|| value.get("role").and_then(Value::as_str));
    let retained = provider_policy_event_text(EventType::Message, &text, &Value::Null);
    let content_ref = ContentRef::from_bytes(text.as_bytes())?;
    Some(claude_nativepath_message_hash_payload(
        &native_record_id,
        value.get("parentUuid").and_then(Value::as_str),
        message_role,
        value.get("timestamp").and_then(Value::as_str),
        &retained.text,
        &retained.retention.as_json(),
        &content_ref,
    ))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn message_profile_excludes_compound_tool_records() {
        let message = json!({
            "type": "assistant",
            "uuid": "message-1",
            "message": {"role": "assistant", "content": "complete message"}
        });
        assert_eq!(
            claude_complete_content_message_record(&message, 7),
            Some(("complete message".to_owned(), "message-1".to_owned()))
        );

        let tool_result = json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [{"type": "tool_result", "content": "output"}]
            }
        });
        assert_eq!(
            claude_complete_content_message_record(&tool_result, 8),
            None
        );

        let pure_tool_call = json!({
            "type": "assistant",
            "message": {
                "role": "assistant",
                "content": [{"type": "tool_use", "id": "call-1", "name": "Read"}]
            }
        });
        assert_eq!(
            claude_complete_content_message_record(&pure_tool_call, 9),
            None
        );
    }

    #[test]
    fn message_profile_identity_prefers_uuid_then_message_id_then_line() {
        let with_both = json!({
            "type": "assistant",
            "uuid": "uuid-1",
            "message": {"id": "message-1", "content": "body"}
        });
        assert_eq!(
            claude_complete_content_message_record(&with_both, 7)
                .map(|record| record.1)
                .as_deref(),
            Some("uuid-1")
        );

        let with_message_id = json!({
            "type": "assistant",
            "message": {"id": "message-2", "content": "body"}
        });
        assert_eq!(
            claude_complete_content_message_record(&with_message_id, 8)
                .map(|record| record.1)
                .as_deref(),
            Some("message-2")
        );

        let with_line_fallback = json!({
            "type": "assistant",
            "message": {"content": "body"}
        });
        assert_eq!(
            claude_complete_content_message_record(&with_line_fallback, 9)
                .map(|record| record.1)
                .as_deref(),
            Some("line-9")
        );
    }
}
