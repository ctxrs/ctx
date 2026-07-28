use ctx_history_core::EventType;
use serde_json::{json, Value};

use crate::provider::normalization::{
    provider_capped_json, provider_policy_body, provider_policy_event_text,
    provider_result_identifier_evidence, provider_result_outcome_evidence,
};
use crate::PROVIDER_MAX_PREVIEW_CHARS;

pub(crate) const PI_SOURCE_FORMAT: &str = "pi_session_jsonl";

pub(crate) mod nativepath;
mod text;

pub(crate) use nativepath::import_pi_nativepath_history;
use text::{pi_entry_text, pi_message_has_tool_call};

pub(crate) fn pi_complete_content_message_record(
    entry: &Value,
    line_number: usize,
) -> Option<(String, String)> {
    let entry_type = entry
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let message = entry.get("message");
    (pi_event_type(entry_type, message) == EventType::Message).then(|| {
        (
            pi_entry_text(entry, message).unwrap_or_default(),
            entry
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.trim().is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("line-{line_number}")),
        )
    })
}

pub(crate) fn pi_complete_content_normalized_payload(entry: &Value) -> Option<Value> {
    let entry_type = entry
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let message = entry.get("message");
    let event_type = pi_event_type(entry_type, message);
    if event_type != EventType::Message {
        return None;
    }
    Some(pi_normalized_event_payload(entry, event_type))
}

fn pi_normalized_event_payload(entry: &Value, event_type: EventType) -> Value {
    let entry_type = entry
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let message = entry.get("message");
    let message_role = message
        .and_then(|message| message.get("role"))
        .and_then(Value::as_str);
    let text = pi_entry_text(entry, message).unwrap_or_default();
    let retained_text = provider_policy_event_text(event_type, &text, entry);
    let result_evidence = provider_result_identifier_evidence(event_type, &text, entry);
    let result_outcome = provider_result_outcome_evidence(event_type, entry);
    let command = message
        .and_then(|value| value.get("command"))
        .and_then(Value::as_str);
    let exit_code = message
        .and_then(|value| value.get("exitCode"))
        .and_then(Value::as_i64);
    json!({
        "entry_type": entry_type,
        "entry_id": entry.get("id").and_then(Value::as_str),
        "parent_id": entry.get("parentId").and_then(Value::as_str),
        "message_role": message_role,
        "command": command,
        "exit_code": exit_code,
        "text": retained_text.text,
        "text_retention": retained_text.retention.as_json(),
        "result_evidence": result_evidence,
        "result_outcome": result_outcome,
        "body": provider_capped_json(
            &provider_policy_body(event_type, entry),
            PROVIDER_MAX_PREVIEW_CHARS,
        ),
    })
}

pub(crate) fn pi_event_type(entry_type: &str, message: Option<&Value>) -> EventType {
    match entry_type {
        "compaction" | "branch_summary" => EventType::Summary,
        "message" => match message
            .and_then(|message| message.get("role"))
            .and_then(Value::as_str)
            .unwrap_or("unknown")
        {
            "toolResult" => EventType::ToolOutput,
            "bashExecution" => EventType::CommandOutput,
            "assistant" if message.is_some_and(pi_message_has_tool_call) => EventType::ToolCall,
            _ => EventType::Message,
        },
        "model_change"
        | "thinking_level_change"
        | "custom"
        | "custom_message"
        | "label"
        | "session_info" => EventType::Notice,
        _ => EventType::Notice,
    }
}
