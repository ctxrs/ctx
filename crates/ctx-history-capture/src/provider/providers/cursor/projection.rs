use std::path::PathBuf;

use chrono::{DateTime, Utc};
use ctx_history_core::{ContentRef, EventRole, EventType};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::common::time::parse_rfc3339_utc;

use super::{
    checkpoint::CursorSessionCheckpoint,
    parser::{CursorSafePart, CursorSanitizedRecord},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct CursorNativeOrder {
    pub(crate) semantic_ordinal: u64,
    pub(crate) physical_ordinal: u64,
    pub(crate) part_ordinal: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum CursorEventBody {
    None,
    Text {
        text: String,
    },
    ToolCall {
        call_id: Option<String>,
        tool_name: Option<String>,
        input_paths: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CursorNativeEvent {
    pub(crate) native_order: CursorNativeOrder,
    pub(crate) event_type: EventType,
    pub(crate) role: EventRole,
    pub(crate) occurred_at: Option<DateTime<Utc>>,
    pub(crate) body: CursorEventBody,
    pub(crate) complete_content_ref: Option<ContentRef>,
    pub(crate) record_byte_start: u64,
    pub(crate) record_byte_end_exclusive: u64,
    pub(crate) record_sha256: [u8; 32],
    pub(crate) provider_event_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CursorNativeSession {
    pub(crate) native_session_id: String,
    pub(crate) project: PathBuf,
    pub(crate) started_at: Option<DateTime<Utc>>,
    pub(crate) ended_at: Option<DateTime<Utc>>,
    pub(crate) title: Option<String>,
}

pub(super) fn project_cursor_record(
    record: &CursorSanitizedRecord,
) -> serde_json::Result<Vec<CursorNativeEvent>> {
    record
        .parts
        .iter()
        .enumerate()
        .map(|(part_ordinal, part)| {
            let part_ordinal = u32::try_from(part_ordinal).unwrap_or(u32::MAX);
            let (event_type, role, body, complete_content_ref) = match part {
                CursorSafePart::BodyFree { event_type, role } => {
                    (*event_type, *role, CursorEventBody::None, None)
                }
                CursorSafePart::Text {
                    event_type,
                    role,
                    text,
                    complete_content_ref,
                } => (
                    *event_type,
                    *role,
                    CursorEventBody::Text { text: text.clone() },
                    complete_content_ref.clone(),
                ),
                CursorSafePart::ToolUse {
                    role,
                    call_id,
                    tool_name,
                    input_paths,
                } => (
                    EventType::ToolCall,
                    *role,
                    CursorEventBody::ToolCall {
                        call_id: call_id.clone(),
                        tool_name: tool_name.clone(),
                        input_paths: input_paths.clone(),
                    },
                    None,
                ),
            };
            let encoded =
                serde_json::to_vec(&("cursor-event-payload-v1", event_type, role, &body))?;
            Ok(CursorNativeEvent {
                native_order: CursorNativeOrder {
                    semantic_ordinal: record.semantic_ordinal,
                    physical_ordinal: record.physical_ordinal,
                    part_ordinal,
                },
                event_type,
                role,
                occurred_at: record.timestamp.as_deref().and_then(parse_rfc3339_utc),
                body,
                complete_content_ref,
                record_byte_start: record.byte_start,
                record_byte_end_exclusive: record.byte_end_exclusive,
                record_sha256: record.record_sha256,
                provider_event_hash: format!("{:x}", Sha256::digest(encoded)),
            })
        })
        .collect()
}

pub(super) fn retained_body_bytes(events: &[CursorNativeEvent]) -> usize {
    events.iter().fold(0_usize, |total, event| {
        let body_bytes = match &event.body {
            CursorEventBody::None => 0,
            CursorEventBody::Text { text } => text.len(),
            CursorEventBody::ToolCall {
                call_id,
                tool_name,
                input_paths,
            } => {
                call_id.as_deref().map_or(0, str::len)
                    + tool_name.as_deref().map_or(0, str::len)
                    + input_paths.iter().map(String::len).sum::<usize>()
            }
        };
        total.saturating_add(body_bytes)
    })
}

pub(super) fn update_cursor_session_checkpoint(
    session: &mut CursorSessionCheckpoint,
    events: &[CursorNativeEvent],
) {
    for event in events {
        if let Some(occurred_at) = event.occurred_at {
            session.started_at.get_or_insert(occurred_at);
            session.ended_at = Some(occurred_at);
        }
        if session.title.is_none() && event.role == EventRole::User {
            if let CursorEventBody::Text { text } = &event.body {
                let title = text.replace('\n', " ").chars().take(80).collect::<String>();
                if !title.trim().is_empty() {
                    session.title = Some(title);
                }
            }
        }
    }
}
