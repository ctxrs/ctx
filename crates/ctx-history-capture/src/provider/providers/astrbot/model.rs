use ctx_history_core::EventRole;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::native_source::NativeSqliteValue;
use crate::provider::normalization::{provider_role, provider_value_text};

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

fn optional_text(value: Option<String>) -> NativeSqliteValue {
    value.map_or(NativeSqliteValue::Null, NativeSqliteValue::Text)
}

fn optional_integer(value: Option<i64>) -> NativeSqliteValue {
    value.map_or(NativeSqliteValue::Null, NativeSqliteValue::Integer)
}
