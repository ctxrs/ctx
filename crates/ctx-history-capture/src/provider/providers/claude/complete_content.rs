use ctx_history_core::EventType;
use serde_json::{json, Value};

use crate::provider::normalization::{
    provider_capped_json, provider_explicit_result_value_text, provider_policy_body,
    provider_policy_event_text, provider_result_identifier_evidence,
    provider_result_outcome_evidence, provider_value_text,
};
use crate::PROVIDER_MAX_PREVIEW_CHARS;

#[allow(dead_code)] // Registered by the universal locator integration branch.
pub(crate) const CLAUDE_RESULT_CONTENT_PROFILE: &str = "claude.result-body.v1";

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
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("line-{line_number}"));
    Some((text, native_id))
}

/// Rebuilds the exact normalized payload used by Store event hashing for a
/// Claude ordinary message without copying provider parsing into the resolver.
pub(crate) fn claude_complete_content_normalized_payload(
    value: &Value,
    _line_number: usize,
) -> Option<Value> {
    let entry_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let message = value.get("message").unwrap_or(value);
    if claude_event_type(entry_type, message) != EventType::Message {
        return None;
    }
    let message_role = message
        .get("role")
        .and_then(Value::as_str)
        .or_else(|| value.get("role").and_then(Value::as_str));
    let null = Value::Null;
    let content = message.get("content").unwrap_or(&null);
    let event_type = EventType::Message;
    let text = provider_value_text(content).unwrap_or_default();
    let retained_text = provider_policy_event_text(event_type, &text, content);
    let result_source = json!({
        "content": content,
        "tool_use_result": value.get("toolUseResult"),
    });
    let result_evidence = provider_result_identifier_evidence(event_type, &text, &result_source);
    let result_outcome = provider_result_outcome_evidence(event_type, &result_source);
    Some(json!({
        "entry_type": entry_type,
        "uuid": value.get("uuid").and_then(Value::as_str),
        "parent_uuid": value.get("parentUuid").and_then(Value::as_str),
        "message_id": message.get("id").and_then(Value::as_str),
        "request_id": value.get("requestId").and_then(Value::as_str),
        "role": message_role,
        "text": retained_text.text,
        "text_retention": retained_text.retention.as_json(),
        "result_evidence": result_evidence,
        "result_outcome": result_outcome,
        "content_preview": provider_capped_json(
            &provider_policy_body(event_type, content),
            PROVIDER_MAX_PREVIEW_CHARS,
        ),
    }))
}

/// Returns only explicit Claude tool-result content, with no tool-name or
/// status label fallback.
#[allow(dead_code)] // Registered by the universal locator integration branch.
pub(crate) fn claude_result_content(value: &Value) -> Option<String> {
    let message = value.get("message").unwrap_or(value);
    if let Some(blocks) = message.get("content").and_then(Value::as_array) {
        let mut parts = Vec::new();
        for block in blocks {
            if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                continue;
            }
            if let Some(content) = block
                .get("content")
                .and_then(provider_explicit_result_value_text)
            {
                parts.push(content);
            }
        }
        if !parts.is_empty() {
            return Some(parts.join("\n"));
        }
    }
    value
        .get("toolUseResult")
        .or_else(|| message.get("toolUseResult"))
        .and_then(claude_tool_use_result_text)
}

#[allow(dead_code)]
fn claude_tool_use_result_text(value: &Value) -> Option<String> {
    let Some(object) = value.as_object() else {
        return provider_explicit_result_value_text(value);
    };
    let mut streams = Vec::new();
    for key in ["stdout", "stderr"] {
        if let Some(text) = object
            .get(key)
            .and_then(provider_explicit_result_value_text)
        {
            streams.push(text);
        }
    }
    if !streams.is_empty() {
        return Some(streams.join("\n"));
    }
    ["output", "content", "result"].into_iter().find_map(|key| {
        object
            .get(key)
            .and_then(provider_explicit_result_value_text)
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn result_profile_extracts_only_explicit_result_blocks_and_streams() {
        let value = json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [
                    {"type": "text", "text": "not part of result"},
                    {"type": "tool_result", "content": [
                        {"type": "text", "text": "first"},
                        {"type": "image", "source": "not synthesized"},
                        {"type": "text", "text": "second"}
                    ]}
                ]
            }
        });
        assert_eq!(
            claude_result_content(&value).as_deref(),
            Some("first\n{\"source\":\"not synthesized\",\"type\":\"image\"}\nsecond")
        );

        let streams = json!({
            "toolUseResult": {"stdout": "out", "stderr": "err", "command": "ignored"}
        });
        assert_eq!(claude_result_content(&streams).as_deref(), Some("out\nerr"));
        assert_eq!(
            claude_result_content(&json!({"message": {"content": []}})),
            None
        );
    }

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
    }
}
