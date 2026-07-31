use chrono::{DateTime, Utc};
use ctx_history_core::{ContentRef, EventRole, EventType};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::common::time::parse_rfc3339_utc;

use super::parser::{CursorSafePart, CursorSanitizedRecord};

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
        command: Option<String>,
        declared_workdir: Option<String>,
        input_paths: Vec<String>,
        ambiguous_native_fields: bool,
    },
    ToolOutput {
        call_id: Option<String>,
        ambiguous_linkage: bool,
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

pub(super) fn project_cursor_record(
    record: CursorSanitizedRecord,
) -> serde_json::Result<Vec<CursorNativeEvent>> {
    let occurred_at = record.timestamp.as_deref().and_then(parse_rfc3339_utc);
    record
        .parts
        .into_iter()
        .enumerate()
        .map(|(part_ordinal, part)| {
            let part_ordinal = u32::try_from(part_ordinal).unwrap_or(u32::MAX);
            let (event_type, role, body, complete_content_ref) = match part {
                CursorSafePart::BodyFree { event_type, role } => {
                    (event_type, role, CursorEventBody::None, None)
                }
                CursorSafePart::Text {
                    event_type,
                    role,
                    text,
                    complete_content_ref,
                } => (
                    event_type,
                    role,
                    CursorEventBody::Text { text },
                    complete_content_ref,
                ),
                CursorSafePart::ToolUse {
                    role,
                    call_id,
                    tool_name,
                    command,
                    declared_workdir,
                    input_paths,
                    ambiguous_native_fields,
                } => (
                    EventType::ToolCall,
                    role,
                    CursorEventBody::ToolCall {
                        call_id,
                        tool_name,
                        command,
                        declared_workdir,
                        input_paths,
                        ambiguous_native_fields,
                    },
                    None,
                ),
                CursorSafePart::ToolResult {
                    role,
                    call_id,
                    ambiguous_linkage,
                } => (
                    EventType::ToolOutput,
                    role,
                    CursorEventBody::ToolOutput {
                        call_id,
                        ambiguous_linkage,
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
                occurred_at,
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
