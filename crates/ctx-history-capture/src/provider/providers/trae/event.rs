use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::provider::providers::task_json::{task_json_string_field, task_json_time_field};

use super::TRAE_CN_INPUT_HISTORY_KEY;

#[derive(Debug, Clone)]
pub(super) struct TraeEventInput {
    pub(super) native_message_id: String,
    pub(super) native_message_id_from_provider: bool,
    pub(super) role: Option<String>,
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) text: String,
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
    let provider_native_message_id = task_json_string_field(
        &message,
        &[
            "id",
            "messageId",
            "message_id",
            "uuid",
            "requestId",
            "responseId",
        ],
    );
    let native_message_id = provider_native_message_id.clone().unwrap_or_else(|| {
        format!("{workspace_id}:{provider_session_id}:{chat_key}:{message_index}")
    });
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
        native_message_id,
        native_message_id_from_provider: provider_native_message_id.is_some(),
        role,
        occurred_at,
        text,
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

pub(super) fn trae_message_is_output(message: &Value) -> bool {
    fn normalized(value: &str) -> String {
        value
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    }

    for field in ["role", "type", "kind", "messageType", "message_type"] {
        if message
            .get(field)
            .and_then(Value::as_str)
            .map(normalized)
            .is_some_and(|value| {
                matches!(
                    value.as_str(),
                    "tool"
                        | "toolresult"
                        | "tooloutput"
                        | "functionresult"
                        | "functionoutput"
                        | "commandresult"
                        | "commandoutput"
                )
            })
        {
            return true;
        }
    }

    let Value::Object(object) = message else {
        return false;
    };
    let normalized_keys = object
        .keys()
        .map(|key| normalized(key))
        .collect::<BTreeSet<_>>();
    normalized_keys.iter().any(|key| {
        matches!(
            key.as_str(),
            "toolresult"
                | "tooloutput"
                | "functionresult"
                | "functionoutput"
                | "commandresult"
                | "commandoutput"
        )
    }) || (normalized_keys
        .iter()
        .any(|key| matches!(key.as_str(), "toolcallid" | "tooluseid" | "callid"))
        && normalized_keys
            .iter()
            .any(|key| matches!(key.as_str(), "result" | "output" | "error")))
        || (normalized_keys.iter().any(|key| {
            matches!(
                key.as_str(),
                "output" | "result" | "error" | "stdout" | "stderr"
            )
        }) && normalized_keys.iter().any(|key| {
            matches!(
                key.as_str(),
                "command"
                    | "cmd"
                    | "exitcode"
                    | "duration"
                    | "durationms"
                    | "toolname"
                    | "functionname"
                    | "status"
                    | "outcome"
                    | "timedout"
                    | "timeout"
            )
        }))
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
