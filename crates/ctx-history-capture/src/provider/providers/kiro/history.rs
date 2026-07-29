use std::{iter::Enumerate, slice::Iter};

use chrono::{DateTime, Utc};
use ctx_history_core::{EventRole, EventType};
use serde_json::Value;

use crate::native_source::NativeSqliteValue;
use crate::provider::normalization::{
    provider_timestamp_millis, provider_timestamp_value, provider_value_text,
};
use crate::{CaptureError, Result};

use super::event::{kiro_native_event, KiroNativeEvent};

pub(super) const KIRO_V2_RECORD_KIND: &str = "kiro-conversation-v2-v1";
pub(super) const KIRO_LEGACY_RECORD_KIND: &str = "kiro-conversation-v1";

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

pub(super) fn decode_kiro_conversation(
    record_kind: &str,
    values: &[NativeSqliteValue],
) -> Result<KiroConversationRow> {
    match (record_kind, values) {
        (
            KIRO_V2_RECORD_KIND,
            [NativeSqliteValue::Integer(rowid), NativeSqliteValue::Text(key), NativeSqliteValue::Text(conversation_id), NativeSqliteValue::Text(value), created_at, updated_at],
        ) => Ok(KiroConversationRow {
            table: "conversations_v2",
            rowid: *rowid,
            key: key.clone(),
            conversation_id: Some(conversation_id.clone()),
            value: value.clone(),
            created_at: kiro_optional_integer(created_at)?,
            updated_at: kiro_optional_integer(updated_at)?,
        }),
        (
            KIRO_LEGACY_RECORD_KIND,
            [NativeSqliteValue::Integer(rowid), NativeSqliteValue::Text(key), NativeSqliteValue::Text(value)],
        ) => Ok(KiroConversationRow {
            table: "conversations",
            rowid: *rowid,
            key: key.clone(),
            conversation_id: None,
            value: value.clone(),
            created_at: None,
            updated_at: None,
        }),
        (KIRO_V2_RECORD_KIND | KIRO_LEGACY_RECORD_KIND, _) => Err(CaptureError::SystemInvariant(
            "Kiro logical row has an invalid value shape",
        )),
        _ => Err(CaptureError::SystemInvariant(
            "Kiro history decoder received an unexpected record kind",
        )),
    }
}

pub(crate) fn decode_kiro_conversation_for_complete(
    table: &str,
    values: &[NativeSqliteValue],
) -> Result<KiroConversationRow> {
    let record_kind = match table {
        "conversations_v2" => KIRO_V2_RECORD_KIND,
        "conversations" => KIRO_LEGACY_RECORD_KIND,
        _ => {
            return Err(CaptureError::InvalidPayload(
                "Kiro complete-content locator names an unsupported table".to_owned(),
            ));
        }
    };
    decode_kiro_conversation(record_kind, values)
}

fn kiro_optional_integer(value: &NativeSqliteValue) -> Result<Option<i64>> {
    match value {
        NativeSqliteValue::Null => Ok(None),
        NativeSqliteValue::Integer(value) => Ok(Some(*value)),
        _ => Err(CaptureError::SystemInvariant(
            "Kiro logical row has an invalid optional integer value",
        )),
    }
}

pub(crate) struct KiroAssistantMessage {
    pub(crate) event_type: EventType,
    pub(crate) text: String,
    pub(crate) tool_uses: Option<Value>,
}

pub(crate) fn kiro_provider_session_id(row: &KiroConversationRow, value: &Value) -> String {
    row.conversation_id
        .as_deref()
        .or_else(|| value.get("conversation_id").and_then(Value::as_str))
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{}:{}:{}", row.table, row.key, row.rowid))
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
            tool_uses: None,
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
        tool_uses,
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
        events.push(KiroNativeHistoryEvent {
            event: kiro_native_event(
                row,
                provider_session_id,
                history_index,
                0,
                EventType::Message,
                EventRole::User,
                user_at,
                text,
                entry,
                None,
            ),
        });
    }
    if let Some(assistant) = kiro_assistant_message(entry) {
        events.push(KiroNativeHistoryEvent {
            event: kiro_native_event(
                row,
                provider_session_id,
                history_index,
                1,
                assistant.event_type,
                EventRole::Assistant,
                kiro_entry_timestamp(entry, "assistant", user_at),
                assistant.text,
                entry,
                assistant.tool_uses,
            ),
        });
    }
    events
}

pub(super) struct KiroNativeHistoryEvent {
    pub(super) event: KiroNativeEvent,
}

#[derive(Clone, Copy)]
enum KiroHistoryTextSource {
    User,
    Assistant,
}

impl KiroHistoryTextSource {
    fn complete_text(self, entry: &Value) -> Option<String> {
        match self {
            Self::User => kiro_user_prompt_text(entry),
            Self::Assistant => kiro_assistant_message(entry).map(|message| message.text),
        }
    }
}

pub(crate) struct KiroHistoryEvent<'a> {
    pub(crate) event: KiroNativeEvent,
    pub(crate) entry: &'a Value,
    text_source: KiroHistoryTextSource,
}

impl<'a> KiroHistoryEvent<'a> {
    pub(crate) fn complete_text(&self) -> String {
        self.text_source
            .complete_text(self.entry)
            .unwrap_or_default()
    }
}

pub(crate) struct KiroHistoryEvents<'a> {
    row: &'a KiroConversationRow,
    provider_session_id: &'a str,
    started_at: DateTime<Utc>,
    history: Option<Enumerate<Iter<'a, Value>>>,
    pending_assistant: Option<(usize, &'a Value, DateTime<Utc>)>,
}

pub(crate) fn kiro_history_events<'a>(
    row: &'a KiroConversationRow,
    provider_session_id: &'a str,
    value: &'a Value,
    started_at: DateTime<Utc>,
) -> KiroHistoryEvents<'a> {
    KiroHistoryEvents {
        row,
        provider_session_id,
        started_at,
        history: value
            .get("history")
            .and_then(Value::as_array)
            .map(|history| history.iter().enumerate()),
        pending_assistant: None,
    }
}

impl<'a> Iterator for KiroHistoryEvents<'a> {
    type Item = KiroHistoryEvent<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some((history_index, entry, user_at)) = self.pending_assistant.take() {
                if let Some(assistant) = kiro_assistant_message(entry) {
                    let assistant_at = kiro_entry_timestamp(entry, "assistant", user_at);
                    return Some(KiroHistoryEvent {
                        event: kiro_native_event(
                            self.row,
                            self.provider_session_id,
                            history_index,
                            1,
                            assistant.event_type,
                            EventRole::Assistant,
                            assistant_at,
                            assistant.text,
                            entry,
                            assistant.tool_uses,
                        ),
                        entry,
                        text_source: KiroHistoryTextSource::Assistant,
                    });
                }
                continue;
            }

            let (history_index, entry) = self.history.as_mut()?.next()?;
            let user_at = kiro_entry_timestamp(entry, "user", self.started_at);
            self.pending_assistant = Some((history_index, entry, user_at));
            if let Some(text) = kiro_user_prompt_text(entry) {
                return Some(KiroHistoryEvent {
                    event: kiro_native_event(
                        self.row,
                        self.provider_session_id,
                        history_index,
                        0,
                        EventType::Message,
                        EventRole::User,
                        user_at,
                        text,
                        entry,
                        None,
                    ),
                    entry,
                    text_source: KiroHistoryTextSource::User,
                });
            }
        }
    }
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;
