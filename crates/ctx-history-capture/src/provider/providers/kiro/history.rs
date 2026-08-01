use chrono::{DateTime, Utc};
use ctx_history_core::{EventRole, EventType};
use serde_json::Value;

use crate::provider::normalization::{
    provider_timestamp_millis, provider_timestamp_value, provider_value_text,
};

use super::event::{kiro_native_event, KiroNativeEvent};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KiroConversationRow {
    pub(crate) table: &'static str,
    pub(crate) rowid: i64,
    pub(crate) key: String,
    pub(crate) conversation_id: Option<String>,
    pub(crate) value: String,
    pub(crate) created_at: Option<i64>,
    pub(crate) updated_at: Option<i64>,
}

pub(crate) struct KiroAssistantMessage {
    pub(crate) event_type: EventType,
    pub(crate) text: String,
}

pub(crate) fn kiro_provider_session_id(row: &KiroConversationRow, value: &Value) -> String {
    row.conversation_id
        .as_deref()
        .or_else(|| value.get("conversation_id").and_then(Value::as_str))
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{}:{}", row.table, row.key))
}

pub(crate) fn kiro_session_started_at(
    row: &KiroConversationRow,
    value: &Value,
    fallback: DateTime<Utc>,
) -> DateTime<Utc> {
    value
        .get("history")
        .and_then(Value::as_array)
        .and_then(|history| {
            history
                .iter()
                .map(|entry| kiro_entry_timestamp(entry, "user", fallback))
                .min()
        })
        .unwrap_or_else(|| provider_timestamp_millis(row.created_at, fallback))
}

pub(crate) fn kiro_entry_timestamp(
    entry: &Value,
    role: &str,
    fallback: DateTime<Utc>,
) -> DateTime<Utc> {
    provider_timestamp_value(
        entry
            .get(role)
            .and_then(|value| value.get("timestamp"))
            .or_else(|| entry.get("timestamp")),
        fallback,
    )
}

pub(crate) fn kiro_user_prompt_text(entry: &Value) -> Option<String> {
    entry
        .pointer("/user/content/Prompt/prompt")
        .and_then(provider_value_text)
        .filter(|text| !text.trim().is_empty())
}

pub(crate) fn kiro_assistant_message(entry: &Value) -> Option<KiroAssistantMessage> {
    if let Some(content) = entry
        .pointer("/assistant/Response/content")
        .and_then(provider_value_text)
        .filter(|text| !text.trim().is_empty())
    {
        return Some(KiroAssistantMessage {
            event_type: EventType::Message,
            text: content,
        });
    }

    let tool_use = entry.pointer("/assistant/ToolUse")?;
    let tool_uses = tool_use
        .get("tool_uses")
        .or_else(|| tool_use.get("toolUses"))
        .cloned();
    let text = tool_use
        .get("content")
        .and_then(provider_value_text)
        .filter(|text| !text.trim().is_empty())
        .or_else(|| tool_uses.as_ref().and_then(kiro_tool_uses_text))
        .unwrap_or_else(|| "Kiro assistant tool use".to_owned());
    let has_tool_uses = tool_uses
        .as_ref()
        .and_then(Value::as_array)
        .map(|items| !items.is_empty())
        .unwrap_or(false);
    Some(KiroAssistantMessage {
        event_type: if has_tool_uses {
            EventType::ToolCall
        } else {
            EventType::Message
        },
        text,
    })
}

pub(crate) fn kiro_tool_uses_text(value: &Value) -> Option<String> {
    let names = value
        .as_array()?
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .filter(|name| !name.trim().is_empty())
        .collect::<Vec<_>>();
    (!names.is_empty()).then(|| format!("tool calls: {}", names.join(", ")))
}

pub(super) fn kiro_history_entry_events(
    row: &KiroConversationRow,
    provider_session_id: &str,
    history_index: usize,
    entry: &Value,
    started_at: DateTime<Utc>,
) -> Vec<KiroNativeHistoryEvent> {
    let user_at = kiro_entry_timestamp(entry, "user", started_at);
    let mut events = Vec::with_capacity(2);
    if let Some(text) = kiro_user_prompt_text(entry) {
        let complete_text = text.clone();
        events.push(KiroNativeHistoryEvent {
            event: kiro_native_event(
                row,
                provider_session_id,
                history_index,
                0,
                EventType::Message,
                EventRole::User,
                user_at,
            ),
            complete_text,
        });
    }
    if let Some(assistant) = kiro_assistant_message(entry) {
        let complete_text = assistant.text.clone();
        events.push(KiroNativeHistoryEvent {
            event: kiro_native_event(
                row,
                provider_session_id,
                history_index,
                1,
                assistant.event_type,
                EventRole::Assistant,
                kiro_entry_timestamp(entry, "assistant", user_at),
            ),
            complete_text,
        });
    }
    events
}

pub(super) struct KiroNativeHistoryEvent {
    pub(super) event: KiroNativeEvent,
    pub(super) complete_text: String,
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;
