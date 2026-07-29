use chrono::{DateTime, Utc};
use ctx_history_core::{EventRole, EventType};
use serde_json::{json, Value};

use crate::provider::normalization::{
    provider_capped_json, provider_policy_body, provider_policy_event_text,
    provider_result_identifier_evidence, provider_result_outcome_evidence,
};
use crate::{KIRO_SQLITE_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS};

use super::history::KiroConversationRow;

#[allow(clippy::too_many_arguments)]
pub(super) fn kiro_native_event(
    row: &KiroConversationRow,
    provider_session_id: &str,
    history_index: usize,
    part_index: u64,
    event_type: EventType,
    role: EventRole,
    occurred_at: DateTime<Utc>,
    text: String,
    entry: &Value,
    tool_uses: Option<Value>,
) -> KiroNativeEvent {
    let provider_event_index = history_index
        .saturating_mul(2)
        .saturating_add(part_index as usize) as u64;
    let role_name = match role {
        EventRole::User => "user",
        EventRole::Assistant => "assistant",
        EventRole::System => "system",
        EventRole::Tool => "tool",
        EventRole::Unknown => "unknown",
    };
    let legacy_provider_event_hash = format!(
        "{}:{}:{}:{role_name}",
        row.table, provider_session_id, history_index
    );
    let body = json!({
        "table": row.table,
        "key": row.key,
        "conversation_id": provider_session_id,
        "history_index": history_index,
        "role": role_name,
        "entry": kiro_core_value(entry),
        "tool_uses": tool_uses.as_ref().map(kiro_core_value),
    });
    let retained_text = provider_policy_event_text(event_type, &text, &body);
    let retained_body = provider_policy_body(event_type, &body);
    KiroNativeEvent {
        provider_event_index,
        provider_event_hash: None,
        cursor: format!(
            "{}:{}:history:{}:{role_name}",
            row.table, provider_session_id, history_index
        ),
        event_type,
        role: Some(role),
        occurred_at,
        payload: json!({
            "text": retained_text.text,
            "text_retention": retained_text.retention.as_json(),
            "result_evidence": provider_result_identifier_evidence(event_type, &text, &body),
            "result_outcome": provider_result_outcome_evidence(event_type, &body),
            "source_format": KIRO_SQLITE_SOURCE_FORMAT,
            "body": provider_capped_json(&retained_body, PROVIDER_MAX_PREVIEW_CHARS),
        }),
        metadata: json!({
            "source": row.table,
            "source_format": KIRO_SQLITE_SOURCE_FORMAT,
            "key": row.key,
            "conversation_id": provider_session_id,
            "history_index": history_index,
            "rowid": row.rowid,
            "legacy_provider_event_hash": legacy_provider_event_hash,
        }),
    }
}

pub(crate) struct KiroNativeEvent {
    pub(crate) provider_event_index: u64,
    pub(crate) provider_event_hash: Option<String>,
    pub(crate) cursor: String,
    pub(crate) event_type: EventType,
    pub(crate) role: Option<EventRole>,
    pub(crate) occurred_at: DateTime<Utc>,
    pub(crate) payload: Value,
    // Native row identity remains provenance alongside the normalized payload.
    #[allow(dead_code)]
    pub(crate) metadata: Value,
}

/// Core may retain prompts, assistant prose, and tool-call inputs, but never
/// provider output/result bodies.
pub(super) fn kiro_core_value(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(kiro_core_value).collect()),
        Value::Object(values) => Value::Object(
            values
                .iter()
                .filter(|(key, _)| !kiro_output_body_key(key))
                .map(|(key, value)| (key.clone(), kiro_core_value(value)))
                .collect(),
        ),
        value => value.clone(),
    }
}

fn kiro_output_body_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "output"
            | "outputs"
            | "result"
            | "results"
            | "tool_result"
            | "tool_results"
            | "toolresult"
            | "toolresults"
            | "stdout"
            | "stderr"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_value_removes_nested_output_bodies_without_dropping_tool_inputs() {
        let value = json!({
            "assistant": {
                "ToolUse": {
                    "tool_uses": [{
                        "name": "shell",
                        "input": {"command": "pwd"},
                        "result": {"stdout": "PRIVATE-SUCCESS"}
                    }]
                },
                "tool_results": {"call": {"output": "PRIVATE-SUCCESS"}}
            }
        });
        let core = kiro_core_value(&value);
        assert_eq!(
            core.pointer("/assistant/ToolUse/tool_uses/0/input/command"),
            Some(&Value::String("pwd".to_owned()))
        );
        assert!(!core.to_string().contains("PRIVATE-SUCCESS"));
    }
}
