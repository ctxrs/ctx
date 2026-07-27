//! Captured MessagePack decoding and privacy-preserving message interpretation.

use ctx_history_core::EventRole;
use rmpv::{decode::read_value as read_msgpack_value, Value as MsgpackValue};

use crate::{CaptureError, Result};

#[derive(Debug, Clone)]
pub(super) struct DeepAgentsMessage {
    pub(super) role: EventRole,
    pub(super) message_type: String,
    pub(super) message_class: Option<String>,
    pub(super) message_id: Option<String>,
    pub(super) text: String,
}

pub(super) fn deepagents_messages_from_blob(
    value_type: Option<&str>,
    value: &[u8],
) -> Result<Vec<DeepAgentsMessage>> {
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
    read_msgpack_value(&mut cursor).map_err(|err| {
        CaptureError::InvalidPayload(format!("invalid Deep Agents msgpack payload: {err}"))
    })
}

pub(super) fn deepagents_messages_from_msgpack_value(
    value: &MsgpackValue,
) -> Vec<DeepAgentsMessage> {
    match value {
        MsgpackValue::Array(items) => items
            .iter()
            .filter_map(deepagents_message_from_msgpack_value)
            .collect(),
        _ => deepagents_message_from_msgpack_value(value)
            .into_iter()
            .collect(),
    }
}

pub(super) fn deepagents_message_from_msgpack_value(
    value: &MsgpackValue,
) -> Option<DeepAgentsMessage> {
    match value {
        MsgpackValue::Map(fields) => deepagents_message_from_fields(fields, None),
        MsgpackValue::Ext(5, payload) => {
            let decoded = deepagents_decode_msgpack(payload).ok()?;
            let MsgpackValue::Array(items) = decoded else {
                return None;
            };
            let class_name = items.get(1).and_then(msgpack_string);
            let fields = match items.get(2)? {
                MsgpackValue::Map(fields) => fields,
                _ => return None,
            };
            deepagents_message_from_fields(fields, class_name)
        }
        _ => None,
    }
}

pub(super) fn deepagents_message_from_fields(
    fields: &[(MsgpackValue, MsgpackValue)],
    class_name: Option<String>,
) -> Option<DeepAgentsMessage> {
    let message_type = msgpack_map_string(fields, "type")
        .or_else(|| msgpack_map_string(fields, "role"))
        .or_else(|| class_name.clone())
        .unwrap_or_else(|| "unknown".to_owned());
    let role = deepagents_message_role(&message_type, class_name.as_deref())?;
    if role == EventRole::System {
        return None;
    }
    let content = msgpack_map_get(fields, "content")?;
    let text = deepagents_content_text(content)?;
    if text.trim().is_empty() || text.starts_with("[SYSTEM]") {
        return None;
    }
    Some(DeepAgentsMessage {
        role,
        message_type,
        message_class: class_name,
        message_id: msgpack_map_string(fields, "id"),
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

pub(super) fn msgpack_string(value: &MsgpackValue) -> Option<String> {
    match value {
        MsgpackValue::String(text) => text.as_str().map(str::to_owned),
        _ => None,
    }
}
