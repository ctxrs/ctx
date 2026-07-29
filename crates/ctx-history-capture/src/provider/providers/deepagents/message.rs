//! Captured MessagePack decoding and privacy-preserving message interpretation.

use chrono::{DateTime, Utc};
use ctx_history_core::{EventRole, EventType};
use rmpv::{decode::read_value as read_msgpack_value, Value as MsgpackValue};
use serde_json::{json, Value};

use crate::{
    provider::normalization::{
        provider_policy_body, provider_policy_event_text, provider_result_identifier_evidence,
        provider_result_outcome_evidence,
    },
    CaptureError, OutputOutcome, OutputOutcomeMetadata, Result, DEEPAGENTS_SQLITE_SOURCE_FORMAT,
};

use super::source::DeepAgentsWriteKey;

const MAX_RETAINED_MESSAGE_REJECTIONS: usize = 64;

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

#[derive(Debug, Clone)]
pub(super) struct DeepAgentsParsedMessage {
    pub(super) offset: usize,
    pub(super) provider_event_index: u64,
    pub(super) message: DeepAgentsMessage,
}

#[derive(Debug, Clone)]
pub(super) struct DeepAgentsMessageIdentity {
    pub(super) provider_index: u64,
}

pub(super) fn deepagents_message_identity(
    thread_id: &str,
    message_id: &str,
) -> DeepAgentsMessageIdentity {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for component in [
        b"ctx-deepagents-message-v1".as_slice(),
        thread_id.as_bytes(),
        message_id.as_bytes(),
    ] {
        for byte in component {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(PRIME);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(PRIME);
    }
    DeepAgentsMessageIdentity {
        provider_index: hash,
    }
}

#[derive(Debug)]
pub(crate) struct DeepAgentsNativeEvent {
    pub(crate) provider_event_index: u64,
    pub(crate) provider_event_hash: Option<String>,
    pub(crate) cursor: String,
    pub(crate) event_type: EventType,
    pub(crate) role: Option<EventRole>,
    pub(crate) occurred_at: DateTime<Utc>,
    pub(crate) payload: Value,
    pub(crate) metadata: Value,
}

pub(super) fn deepagents_native_event(
    key: &DeepAgentsWriteKey,
    parsed: &DeepAgentsParsedMessage,
    occurred_at: DateTime<Utc>,
    provider_event_hash: &str,
    provider_event_identity_index: Option<u64>,
    _record_digest: Option<crate::complete_content::CompleteContentBodyDigest>,
) -> DeepAgentsNativeEvent {
    let event_type = deepagents_event_type(&parsed.message);
    let cursor = format!(
        "thread:{}:checkpoint:{}:task:{}:write:{}:message:{}",
        key.thread_id, key.checkpoint_id, key.task_id, key.idx, parsed.offset
    );
    let body = json!({
        "message_type": parsed.message.message_type,
        "message_class": parsed.message.message_class,
        "message_id": parsed.message.message_id,
        "tool_call_id": parsed.message.tool_call_id,
        "status": parsed.message.status,
        "exit_code": parsed.message.exit_code,
        "duration_ms": parsed.message.duration_ms,
        "timed_out": parsed.message.timed_out,
        "is_error": parsed.message.is_error,
        "success": parsed.message.success,
        "checkpoint_id": key.checkpoint_id,
        "task_id": key.task_id,
        "write_idx": key.idx,
        "message_offset": parsed.offset,
    });
    let retained_text = provider_policy_event_text(event_type, &parsed.message.text, &body);
    let retained_body = provider_policy_body(event_type, &body);
    let result_evidence =
        provider_result_identifier_evidence(event_type, &parsed.message.text, &body);
    let result_outcome = provider_result_outcome_evidence(event_type, &body);
    DeepAgentsNativeEvent {
        provider_event_index: parsed.provider_event_index,
        provider_event_hash: Some(provider_event_hash.to_owned()),
        cursor,
        event_type,
        role: Some(parsed.message.role),
        occurred_at,
        payload: json!({
            "text": retained_text.text,
            "text_retention": retained_text.retention.as_json(),
            "result_evidence": result_evidence,
            "result_outcome": result_outcome,
            "source_format": DEEPAGENTS_SQLITE_SOURCE_FORMAT,
            "body": retained_body,
        }),
        metadata: json!({
            "source": DEEPAGENTS_SQLITE_SOURCE_FORMAT,
            "source_format": DEEPAGENTS_SQLITE_SOURCE_FORMAT,
            "checkpoint_id": key.checkpoint_id,
            "task_id": key.task_id,
            "write_idx": key.idx,
            "message_offset": parsed.offset,
            "message_type": parsed.message.message_type,
            "message_class": parsed.message.message_class,
            "message_id": parsed.message.message_id,
            "provider_event_identity_index": provider_event_identity_index,
            "privacy": "decoded from writes.messages only",
        }),
    }
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

#[derive(Debug, Clone)]
pub(super) struct DeepAgentsMessageRejection {
    pub(super) entry_offset: usize,
    pub(super) error: String,
}

#[derive(Debug, Clone, Default)]
pub(super) struct DeepAgentsDecodedMessages {
    pub(super) messages: Vec<DeepAgentsMessage>,
    pub(super) rejected_entries: u64,
    pub(super) ignored_entries: u64,
    pub(super) rejections: Vec<DeepAgentsMessageRejection>,
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
            for (entry_offset, item) in items.iter().enumerate() {
                decoded.record(entry_offset, deepagents_message_from_msgpack_value(item));
            }
        }
        _ => decoded.record(0, deepagents_message_from_msgpack_value(value)),
    }
    decoded
}

impl DeepAgentsDecodedMessages {
    fn record(&mut self, entry_offset: usize, outcome: DeepAgentsMessageOutcome) {
        match outcome {
            DeepAgentsMessageOutcome::Message(message) => self.messages.push(message),
            DeepAgentsMessageOutcome::System => {
                self.ignored_entries = self.ignored_entries.saturating_add(1);
            }
            DeepAgentsMessageOutcome::Rejected(error) => {
                self.rejected_entries = self.rejected_entries.saturating_add(1);
                if self.rejections.len() < MAX_RETAINED_MESSAGE_REJECTIONS {
                    self.rejections.push(DeepAgentsMessageRejection {
                        entry_offset,
                        error,
                    });
                }
            }
        }
    }
}

enum DeepAgentsMessageOutcome {
    Message(DeepAgentsMessage),
    System,
    Rejected(String),
}

fn deepagents_message_from_msgpack_value(value: &MsgpackValue) -> DeepAgentsMessageOutcome {
    match value {
        MsgpackValue::Map(fields) => deepagents_message_from_fields(fields, None),
        MsgpackValue::Ext(5, payload) => {
            let decoded = match deepagents_decode_msgpack(payload) {
                Ok(decoded) => decoded,
                Err(error) => {
                    return DeepAgentsMessageOutcome::Rejected(format!(
                        "Deep Agents message extension payload is invalid: {error}"
                    ));
                }
            };
            let MsgpackValue::Array(items) = decoded else {
                return DeepAgentsMessageOutcome::Rejected(
                    "Deep Agents message extension payload is not an array".to_owned(),
                );
            };
            let class_name = items.get(1).and_then(msgpack_string);
            let fields = match items.get(2) {
                Some(MsgpackValue::Map(fields)) => fields,
                Some(_) => {
                    return DeepAgentsMessageOutcome::Rejected(
                        "Deep Agents message extension fields are not a map".to_owned(),
                    );
                }
                None => {
                    return DeepAgentsMessageOutcome::Rejected(
                        "Deep Agents message extension is missing fields".to_owned(),
                    );
                }
            };
            deepagents_message_from_fields(fields, class_name)
        }
        MsgpackValue::Ext(_, _) => DeepAgentsMessageOutcome::Rejected(
            "Deep Agents message uses an unsupported extension type".to_owned(),
        ),
        _ => DeepAgentsMessageOutcome::Rejected(
            "Deep Agents message entry has an unsupported non-system shape".to_owned(),
        ),
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
        return DeepAgentsMessageOutcome::Rejected(
            "Deep Agents message has an unsupported non-system type".to_owned(),
        );
    };
    if role == EventRole::System {
        return DeepAgentsMessageOutcome::System;
    }
    let Some(content) = msgpack_map_get(fields, "content") else {
        return DeepAgentsMessageOutcome::Rejected(
            "Deep Agents non-system message is missing content".to_owned(),
        );
    };
    let Some(text) = deepagents_content_text(content) else {
        return DeepAgentsMessageOutcome::Rejected(
            "Deep Agents non-system message content has an unsupported shape".to_owned(),
        );
    };
    if text.starts_with("[SYSTEM]") {
        return DeepAgentsMessageOutcome::System;
    }
    if text.trim().is_empty() {
        return DeepAgentsMessageOutcome::Rejected(
            "Deep Agents non-system message content is empty".to_owned(),
        );
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
