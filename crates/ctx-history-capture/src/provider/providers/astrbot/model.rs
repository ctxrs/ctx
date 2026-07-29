use ctx_history_core::{EventRole, EventType};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::native_source::NativeSqliteValue;
use crate::provider::normalization::{provider_role, provider_value_text};
use crate::{CaptureError, Result};

const CONVERSATION_VALUE_COUNT: usize = 11;

#[derive(Debug, Clone, Serialize)]
pub(super) struct ConversationRow {
    pub(super) row_id: i64,
    pub(super) inner_conversation_id: Option<String>,
    pub(super) conversation_id: String,
    pub(super) platform_id: Option<String>,
    pub(super) user_id: Option<String>,
    pub(super) content: String,
    pub(super) title: Option<String>,
    pub(super) persona_id: Option<String>,
    pub(super) token_usage: Option<String>,
    pub(super) created_at: Option<i64>,
    pub(super) updated_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct PlatformMessageRow {
    pub(super) id: i64,
    pub(super) platform_id: Option<String>,
    pub(super) user_id: Option<String>,
    pub(super) sender_id: Option<String>,
    pub(super) sender_name: Option<String>,
    pub(super) content: Option<String>,
    pub(super) llm_checkpoint_id: Option<String>,
    pub(super) created_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PlatformMessageLink {
    pub(super) provider_session_id: String,
    pub(super) parent_created_at: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(super) struct LegacyOrderKey {
    pub(super) timestamp_is_present: bool,
    pub(super) timestamp: i64,
    pub(super) logical_id: i64,
    pub(super) physical_rowid: i64,
}

pub(super) fn provider_session_id(conversation: &ConversationRow) -> String {
    conversation
        .inner_conversation_id
        .as_ref()
        .unwrap_or(&conversation.conversation_id)
        .clone()
}

pub(super) fn item_id(item: &Value) -> Option<&str> {
    item.get("id")
        .or_else(|| item.get("message_id"))
        .or_else(|| item.get("checkpoint_id"))
        .and_then(Value::as_str)
}

pub(super) fn checkpoint_id(item: &Value) -> Option<String> {
    let item_type = item
        .get("type")
        .or_else(|| item.get("role"))
        .and_then(Value::as_str)?;
    matches!(item_type, "_checkpoint" | "checkpoint")
        .then(|| item_id(item).map(str::to_owned))
        .flatten()
}

pub(super) fn item_role(item: &Value) -> Option<EventRole> {
    item.get("role")
        .or_else(|| item.get("type"))
        .and_then(Value::as_str)
        .map(|role| provider_role(Some(role)))
}

pub(super) fn item_text(item: &Value) -> Option<String> {
    item.get("content")
        .or_else(|| item.get("text"))
        .or_else(|| item.get("message"))
        .and_then(provider_value_text)
}

pub(super) fn item_is_output(item: &Value) -> bool {
    item.get("role")
        .or_else(|| item.get("type"))
        .and_then(Value::as_str)
        .is_some_and(|kind| {
            let normalized = kind
                .chars()
                .filter(|ch| ch.is_ascii_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect::<String>();
            matches!(
                normalized.as_str(),
                "tool"
                    | "function"
                    | "toolresult"
                    | "tooloutput"
                    | "commandresult"
                    | "commandoutput"
            )
        })
}

pub(super) fn conversation_values(row: ConversationRow) -> Vec<NativeSqliteValue> {
    vec![
        NativeSqliteValue::Integer(row.row_id),
        optional_text(row.inner_conversation_id),
        NativeSqliteValue::Text(row.conversation_id),
        optional_text(row.platform_id),
        optional_text(row.user_id),
        NativeSqliteValue::Text(row.content),
        optional_text(row.title),
        optional_text(row.persona_id),
        optional_text(row.token_usage),
        optional_integer(row.created_at),
        optional_integer(row.updated_at),
    ]
}

/// Shape retained only so the untouched released complete-content wrapper can
/// compile. Provider-local fallback construction now rejects below.
pub(crate) struct AstrBotCompleteMessage {
    pub(crate) provider_event_index: u64,
    pub(crate) provider_event_hash: Option<String>,
    pub(crate) cursor: String,
    pub(crate) event_type: EventType,
    pub(crate) payload: Value,
    pub(crate) text: String,
    pub(crate) provider_session_id: String,
}

pub(super) fn complete_conversation_message(
    _values: &[NativeSqliteValue],
    _item_index: u32,
) -> Result<Option<AstrBotCompleteMessage>> {
    Err(CaptureError::InvalidPayload(
        "AstrBot canonical Store hydration was removed; use source-backed hydration".to_owned(),
    ))
}

pub(super) fn decode_conversation(values: &[NativeSqliteValue]) -> Result<ConversationRow> {
    if values.len() != CONVERSATION_VALUE_COUNT {
        return Err(CaptureError::InvalidPayload(
            "AstrBot conversation logical row has an unexpected value count".to_owned(),
        ));
    }
    Ok(ConversationRow {
        row_id: required_integer(values, 0, "conversation row id")?,
        inner_conversation_id: optional_text_value(values, 1, "inner_conversation_id")?,
        conversation_id: required_text(values, 2, "conversation_id")?,
        platform_id: optional_text_value(values, 3, "platform_id")?,
        user_id: optional_text_value(values, 4, "user_id")?,
        content: required_text(values, 5, "conversation content")?,
        title: optional_text_value(values, 6, "conversation title")?,
        persona_id: optional_text_value(values, 7, "persona_id")?,
        token_usage: optional_text_value(values, 8, "token_usage")?,
        created_at: optional_integer_value(values, 9, "conversation created_at")?,
        updated_at: optional_integer_value(values, 10, "conversation updated_at")?,
    })
}

fn value<'a>(
    values: &'a [NativeSqliteValue],
    index: usize,
    field: &str,
) -> Result<&'a NativeSqliteValue> {
    values.get(index).ok_or_else(|| {
        CaptureError::InvalidPayload(format!("AstrBot logical row is missing {field}"))
    })
}

fn required_text(values: &[NativeSqliteValue], index: usize, field: &str) -> Result<String> {
    match value(values, index, field)? {
        NativeSqliteValue::Text(value) => Ok(value.clone()),
        _ => Err(CaptureError::InvalidPayload(format!(
            "AstrBot logical row {field} must be text"
        ))),
    }
}

fn optional_text_value(
    values: &[NativeSqliteValue],
    index: usize,
    field: &str,
) -> Result<Option<String>> {
    match value(values, index, field)? {
        NativeSqliteValue::Null => Ok(None),
        NativeSqliteValue::Text(value) => Ok(Some(value.clone())),
        _ => Err(CaptureError::InvalidPayload(format!(
            "AstrBot logical row {field} must be text or null"
        ))),
    }
}

fn required_integer(values: &[NativeSqliteValue], index: usize, field: &str) -> Result<i64> {
    match value(values, index, field)? {
        NativeSqliteValue::Integer(value) => Ok(*value),
        _ => Err(CaptureError::InvalidPayload(format!(
            "AstrBot logical row {field} must be an integer"
        ))),
    }
}

fn optional_integer_value(
    values: &[NativeSqliteValue],
    index: usize,
    field: &str,
) -> Result<Option<i64>> {
    match value(values, index, field)? {
        NativeSqliteValue::Null => Ok(None),
        NativeSqliteValue::Integer(value) => Ok(Some(*value)),
        _ => Err(CaptureError::InvalidPayload(format!(
            "AstrBot logical row {field} must be an integer or null"
        ))),
    }
}

fn optional_text(value: Option<String>) -> NativeSqliteValue {
    value.map_or(NativeSqliteValue::Null, NativeSqliteValue::Text)
}

fn optional_integer(value: Option<i64>) -> NativeSqliteValue {
    value.map_or(NativeSqliteValue::Null, NativeSqliteValue::Integer)
}
