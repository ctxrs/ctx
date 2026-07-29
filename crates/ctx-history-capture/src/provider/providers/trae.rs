use chrono::{DateTime, Utc};
use ctx_history_core::EventType;
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

use crate::{CaptureError, Result};

mod event;
mod json_stream;
pub(crate) mod nativepath;
mod workspace;

#[cfg(test)]
pub(crate) use nativepath::{
    hydrate_trae_source_backed_locator_v0, scan_trae_source_backed_explicit_v0,
    TraeSourceBackedErrorV0,
};

pub(crate) const TRAE_STATE_VSCDB_SOURCE_FORMAT: &str = "trae_state_vscdb";
pub(crate) const TRAE_CN_INPUT_HISTORY_KEY: &str = "icube-ai-agent-storage-input-history";
pub(crate) const TRAE_CHAT_KEYS: &[&str] = &[
    "memento/icube-ai-agent-storage",
    TRAE_CN_INPUT_HISTORY_KEY,
    "chat.ChatSessionStore.index",
    "ChatStore",
    "memento/icube-ai-chat-storage-7467774676505887760",
    "memento/icube-ai-ng-chat-storage-7467774676505887760",
];

pub(crate) fn trae_complete_value(conn: &Connection, key_index: u16) -> Result<Option<Vec<u8>>> {
    let Some(chat_key) = TRAE_CHAT_KEYS.get(usize::from(key_index)) else {
        return Ok(None);
    };
    conn.query_row(
        "select cast(value as text) from ItemTable where [key] = ?1 and typeof(value) = 'text'",
        [chat_key],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map(|value| value.map(String::into_bytes))
    .map_err(CaptureError::from)
}

pub(crate) fn trae_complete_message(
    bytes: &[u8],
    key_index: u16,
    session_index: u32,
    message_index: u32,
    provider_session_id: &str,
) -> Result<Option<(TraeCompleteEvent, String)>> {
    let Some(chat_key) = TRAE_CHAT_KEYS.get(usize::from(key_index)) else {
        return Ok(None);
    };
    let selection = json_stream::trae_session_selection(bytes, chat_key)?;
    let Some(selection) = selection else {
        return Ok(None);
    };
    let session_index_usize = usize::try_from(session_index)
        .map_err(|_| CaptureError::InvalidPayload("Trae session index exceeds usize".to_owned()))?;
    let session = match selection {
        json_stream::TraeSessionSelection::CnMessages(messages) => {
            if session_index != 0 {
                return Ok(None);
            }
            json_stream::TraeStreamSession {
                native_session_id: "trae-cn-input-history".to_owned(),
                native_session_id_from_provider: true,
                messages,
            }
        }
        json_stream::TraeSessionSelection::Sessions(container) => {
            let mut sessions = json_stream::TraeJsonContainerValues::new(bytes, container)?;
            let mut current = 0_usize;
            let mut selected = None;
            while let Some(range) = sessions.next_range()? {
                if current == session_index_usize {
                    selected = json_stream::trae_stream_session(bytes, range, current)?;
                    break;
                }
                current = current.saturating_add(1);
            }
            let Some(session) = selected else {
                return Ok(None);
            };
            session
        }
    };
    let suffix = format!("/{}", session.native_session_id);
    let Some(workspace_id) = provider_session_id.strip_suffix(&suffix) else {
        return Ok(None);
    };
    if workspace_id.is_empty() {
        return Ok(None);
    }
    let mut messages = json_stream::TraeJsonArrayValues::new(bytes, session.messages)?;
    let mut current = 0_u32;
    while let Some(range) = messages.next_range()? {
        if current == message_index {
            let message: Value = serde_json::from_slice(&bytes[range])?;
            let Some(input) = event::trae_event_from_owned_message(
                provider_session_id,
                workspace_id,
                chat_key,
                message,
                usize::try_from(message_index).unwrap_or(usize::MAX),
                DateTime::<Utc>::UNIX_EPOCH,
            ) else {
                return Ok(None);
            };
            let text = input.text.clone();
            let event = event::trae_core_event(provider_session_id, workspace_id, chat_key, &input);
            return Ok(Some((
                TraeCompleteEvent {
                    provider_event_index: event.provider_event_index,
                    provider_event_hash: event.provider_event_hash,
                    cursor: event.cursor,
                    event_type: event.event_type,
                    payload: event.payload,
                },
                text,
            )));
        }
        current = current.saturating_add(1);
    }
    Ok(None)
}

/// Migration-only fields needed to verify a released complete-content locator.
pub(crate) struct TraeCompleteEvent {
    pub(crate) provider_event_index: u64,
    pub(crate) provider_event_hash: String,
    pub(crate) cursor: String,
    pub(crate) event_type: EventType,
    pub(crate) payload: Value,
}
