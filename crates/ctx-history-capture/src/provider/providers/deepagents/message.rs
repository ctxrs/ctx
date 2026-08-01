//! Captured MessagePack decoding and privacy-preserving message interpretation.

use crate::{CaptureError, OutputOutcome, OutputOutcomeMetadata, Result};
use ctx_history_core::{EventRole, EventType};
use rmpv::{decode::read_value as read_msgpack_value, Value as MsgpackValue};

#[derive(Debug, Clone)]
pub(super) struct DeepAgentsMessage {
    pub(super) role: EventRole,
    pub(super) message_type: String,
    pub(super) message_class: Option<String>,
    pub(super) message_id: Option<String>,
    pub(super) tool_call_id: Option<String>,
    pub(super) status: Option<String>,
    pub(super) exit_code: Option<i32>,
    pub(super) duration_ms: Option<u64>,
    pub(super) timed_out: bool,
    pub(super) is_error: Option<bool>,
    pub(super) success: Option<bool>,
    pub(super) text: String,
}

pub(super) fn deepagents_event_type(message: &DeepAgentsMessage) -> EventType {
    if message.role == EventRole::Tool {
        EventType::ToolOutput
    } else {
        EventType::Message
    }
}

pub(super) fn core_eligible(message: &DeepAgentsMessage) -> bool {
    if message.role != EventRole::Tool {
        return true;
    }
    matches!(
        deepagents_output_outcome(message).outcome,
        OutputOutcome::Failure | OutputOutcome::Timeout
    )
}

fn deepagents_output_outcome(message: &DeepAgentsMessage) -> OutputOutcomeMetadata {
    let status = message
        .status
        .as_deref()
        .map(str::trim)
        .map(str::to_ascii_lowercase);
    let timeout = message.timed_out
        || status
            .as_deref()
            .is_some_and(|status| matches!(status, "timeout" | "timed_out" | "timedout"));
    let failure = message.is_error == Some(true)
        || message.success == Some(false)
        || message.exit_code.is_some_and(|code| code != 0)
        || status.as_deref().is_some_and(|status| {
            matches!(
                status,
                "failed" | "failure" | "error" | "errored" | "cancelled" | "canceled"
            )
        });
    let success = message.success == Some(true)
        || message.exit_code == Some(0)
        || status.as_deref().is_some_and(|status| {
            matches!(
                status,
                "success" | "succeeded" | "complete" | "completed" | "ok" | "passed"
            )
        });
    OutputOutcomeMetadata {
        outcome: if timeout {
            OutputOutcome::Timeout
        } else if failure {
            OutputOutcome::Failure
        } else if success {
            OutputOutcome::Success
        } else {
            OutputOutcome::Unknown
        },
        exit_code: message.exit_code,
        duration_ms: message.duration_ms,
    }
}

#[derive(Debug, Clone, Default)]
pub(super) struct DeepAgentsDecodedMessages {
    pub(super) messages: Vec<DeepAgentsMessage>,
    pub(super) rejected_entries: u64,
    pub(super) ignored_entries: u64,
}

pub(super) fn deepagents_messages_from_blob(
    value_type: Option<&str>,
    value: &[u8],
) -> Result<DeepAgentsDecodedMessages> {
    match value_type {
        Some("msgpack") => {
            let decoded = deepagents_decode_msgpack(value)?;
            Ok(deepagents_messages_from_msgpack_value(&decoded))
        }
        Some(other) => Err(CaptureError::InvalidPayload(format!(
            "unsupported Deep Agents writes.messages value type {other:?}"
        ))),
        None => Err(CaptureError::InvalidPayload(
            "Deep Agents writes.messages row has no value type".to_owned(),
        )),
    }
}

pub(super) fn deepagents_decode_msgpack(value: &[u8]) -> Result<MsgpackValue> {
    let mut cursor = std::io::Cursor::new(value);
    let decoded = read_msgpack_value(&mut cursor).map_err(|err| {
        CaptureError::InvalidPayload(format!("invalid Deep Agents msgpack payload: {err}"))
    })?;
    if cursor.position() != u64::try_from(value.len()).unwrap_or(u64::MAX) {
        return Err(CaptureError::InvalidPayload(
            "invalid Deep Agents msgpack payload: trailing bytes after the decoded value"
                .to_owned(),
        ));
    }
    Ok(decoded)
}

pub(super) fn deepagents_messages_from_msgpack_value(
    value: &MsgpackValue,
) -> DeepAgentsDecodedMessages {
    let mut decoded = DeepAgentsDecodedMessages::default();
    match value {
        MsgpackValue::Array(items) => {
            for item in items {
                decoded.record(deepagents_message_from_msgpack_value(item));
            }
        }
        _ => decoded.record(deepagents_message_from_msgpack_value(value)),
    }
    decoded
}

impl DeepAgentsDecodedMessages {
    fn record(&mut self, outcome: DeepAgentsMessageOutcome) {
        match outcome {
            DeepAgentsMessageOutcome::Message(message) => self.messages.push(message),
            DeepAgentsMessageOutcome::System => {
                self.ignored_entries = self.ignored_entries.saturating_add(1);
            }
            DeepAgentsMessageOutcome::Rejected => {
                self.rejected_entries = self.rejected_entries.saturating_add(1);
            }
        }
    }
}

enum DeepAgentsMessageOutcome {
    Message(DeepAgentsMessage),
    System,
    Rejected,
}

fn deepagents_message_from_msgpack_value(value: &MsgpackValue) -> DeepAgentsMessageOutcome {
    match value {
        MsgpackValue::Map(fields) => deepagents_message_from_fields(fields, None),
        MsgpackValue::Ext(5, payload) => {
            let decoded = match deepagents_decode_msgpack(payload) {
                Ok(decoded) => decoded,
                Err(_) => return DeepAgentsMessageOutcome::Rejected,
            };
            let MsgpackValue::Array(items) = decoded else {
                return DeepAgentsMessageOutcome::Rejected;
            };
            let class_name = items.get(1).and_then(msgpack_string);
            let fields = match items.get(2) {
                Some(MsgpackValue::Map(fields)) => fields,
                Some(_) => {
                    return DeepAgentsMessageOutcome::Rejected;
                }
                None => {
                    return DeepAgentsMessageOutcome::Rejected;
                }
            };
            deepagents_message_from_fields(fields, class_name)
        }
        _ => DeepAgentsMessageOutcome::Rejected,
    }
}

fn deepagents_message_from_fields(
    fields: &[(MsgpackValue, MsgpackValue)],
    class_name: Option<String>,
) -> DeepAgentsMessageOutcome {
    let message_type = msgpack_map_string(fields, "type")
        .or_else(|| msgpack_map_string(fields, "role"))
        .or_else(|| class_name.clone())
        .unwrap_or_else(|| "unknown".to_owned());
    let Some(role) = deepagents_message_role(&message_type, class_name.as_deref()) else {
        return DeepAgentsMessageOutcome::Rejected;
    };
    if role == EventRole::System {
        return DeepAgentsMessageOutcome::System;
    }
    let Some(content) = msgpack_map_get(fields, "content") else {
        return DeepAgentsMessageOutcome::Rejected;
    };
    let Some(text) = deepagents_content_text(content) else {
        return DeepAgentsMessageOutcome::Rejected;
    };
    if text.starts_with("[SYSTEM]") {
        return DeepAgentsMessageOutcome::System;
    }
    if text.trim().is_empty() {
        return DeepAgentsMessageOutcome::Rejected;
    }
    DeepAgentsMessageOutcome::Message(DeepAgentsMessage {
        role,
        message_type,
        message_class: class_name,
        message_id: msgpack_map_string(fields, "id"),
        tool_call_id: msgpack_map_string(fields, "tool_call_id")
            .or_else(|| msgpack_map_string(fields, "toolCallId")),
        status: msgpack_map_string(fields, "status")
            .or_else(|| msgpack_map_string(fields, "state"))
            .or_else(|| msgpack_map_string(fields, "outcome")),
        exit_code: msgpack_map_i64(fields, "exit_code")
            .or_else(|| msgpack_map_i64(fields, "exitCode"))
            .and_then(|value| i32::try_from(value).ok()),
        duration_ms: msgpack_map_u64(fields, "duration_ms")
            .or_else(|| msgpack_map_u64(fields, "durationMs")),
        timed_out: msgpack_map_bool(fields, "timed_out")
            .or_else(|| msgpack_map_bool(fields, "timedOut"))
            .or_else(|| msgpack_map_bool(fields, "timeout"))
            .unwrap_or(false),
        is_error: msgpack_map_bool(fields, "is_error")
            .or_else(|| msgpack_map_bool(fields, "isError")),
        success: msgpack_map_bool(fields, "success").or_else(|| msgpack_map_bool(fields, "ok")),
        text,
    })
}

pub(super) fn deepagents_message_role(
    message_type: &str,
    class_name: Option<&str>,
) -> Option<EventRole> {
    let lowered = message_type.to_ascii_lowercase();
    match lowered.as_str() {
        "human" | "user" => Some(EventRole::User),
        "ai" | "assistant" => Some(EventRole::Assistant),
        "tool" => Some(EventRole::Tool),
        "system" => Some(EventRole::System),
        _ => match class_name.unwrap_or_default() {
            "HumanMessage" => Some(EventRole::User),
            "AIMessage" => Some(EventRole::Assistant),
            "ToolMessage" => Some(EventRole::Tool),
            "SystemMessage" => Some(EventRole::System),
            _ => None,
        },
    }
}

pub(super) fn deepagents_content_text(value: &MsgpackValue) -> Option<String> {
    if let Some(text) = msgpack_string(value) {
        return Some(text);
    }
    if let MsgpackValue::Array(items) = value {
        let parts = items
            .iter()
            .filter_map(|item| match item {
                MsgpackValue::Map(fields) => msgpack_map_string(fields, "text"),
                _ => msgpack_string(item),
            })
            .collect::<Vec<_>>();
        let joined = parts.join(" ").trim().to_owned();
        if !joined.is_empty() {
            return Some(joined);
        }
    }
    None
}

pub(super) fn msgpack_map_get<'a>(
    fields: &'a [(MsgpackValue, MsgpackValue)],
    key: &str,
) -> Option<&'a MsgpackValue> {
    fields.iter().find_map(|(field_key, field_value)| {
        (msgpack_string(field_key).as_deref() == Some(key)).then_some(field_value)
    })
}

pub(super) fn msgpack_map_string(
    fields: &[(MsgpackValue, MsgpackValue)],
    key: &str,
) -> Option<String> {
    msgpack_map_get(fields, key).and_then(msgpack_string)
}

fn msgpack_map_i64(fields: &[(MsgpackValue, MsgpackValue)], key: &str) -> Option<i64> {
    msgpack_map_get(fields, key).and_then(MsgpackValue::as_i64)
}

fn msgpack_map_u64(fields: &[(MsgpackValue, MsgpackValue)], key: &str) -> Option<u64> {
    msgpack_map_get(fields, key).and_then(MsgpackValue::as_u64)
}

fn msgpack_map_bool(fields: &[(MsgpackValue, MsgpackValue)], key: &str) -> Option<bool> {
    msgpack_map_get(fields, key).and_then(MsgpackValue::as_bool)
}

pub(super) fn msgpack_string(value: &MsgpackValue) -> Option<String> {
    match value {
        MsgpackValue::String(text) => text.as_str().map(str::to_owned),
        _ => None,
    }
}
