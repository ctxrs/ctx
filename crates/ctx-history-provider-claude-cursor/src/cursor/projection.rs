use chrono::{DateTime, FixedOffset, NaiveDateTime, TimeZone, Utc};
use ctx_history_core::ProviderDeclaredFact;
use ctx_history_core::{EventRole, EventType, TypedKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use ctx_history_capture_model::time::parse_rfc3339_utc;

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
        native_content: serde_json::Value,
        call_id: Option<String>,
        tool_name: Option<String>,
        arguments: Option<serde_json::Value>,
        protocol: Option<String>,
        server: Option<String>,
        explicit_tool: Option<String>,
        call_id_unavailable: bool,
        tool_name_unavailable: bool,
        arguments_unavailable: bool,
        mcp_identity_unavailable: bool,
        native_content_unavailable: bool,
        literal_facts: Vec<ProviderDeclaredFact>,
    },
    ToolOutput {
        native_content: serde_json::Value,
        call_id: Option<String>,
        call_id_unavailable: bool,
        content_unavailable: bool,
        native_content_unavailable: bool,
        literal_facts: Vec<ProviderDeclaredFact>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CursorNativeEvent {
    pub(crate) native_order: CursorNativeOrder,
    pub(crate) event_type: EventType,
    pub(crate) role: EventRole,
    pub(crate) occurred_at: Option<DateTime<Utc>>,
    pub(crate) body: CursorEventBody,
    pub(crate) record_byte_start: u64,
    pub(crate) record_byte_end_exclusive: u64,
    pub(crate) record_sha256: [u8; 32],
    pub(crate) provider_event_hash: [u8; 32],
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
            let (event_type, role, body) = match part {
                CursorSafePart::BodyFree { event_type, role } => {
                    (event_type, role, CursorEventBody::None)
                }
                CursorSafePart::Text {
                    event_type,
                    role,
                    text,
                } => (event_type, role, CursorEventBody::Text { text }),
                CursorSafePart::ToolUse {
                    role,
                    native_content,
                    call_id,
                    tool_name,
                    arguments,
                    protocol,
                    server,
                    explicit_tool,
                    call_id_unavailable,
                    tool_name_unavailable,
                    arguments_unavailable,
                    mcp_identity_unavailable,
                    native_content_unavailable,
                    literal_facts,
                } => (
                    EventType::ToolCall,
                    role,
                    CursorEventBody::ToolCall {
                        native_content,
                        call_id,
                        tool_name,
                        arguments,
                        protocol,
                        server,
                        explicit_tool,
                        call_id_unavailable,
                        tool_name_unavailable,
                        arguments_unavailable,
                        mcp_identity_unavailable,
                        native_content_unavailable,
                        literal_facts,
                    },
                ),
                CursorSafePart::ToolResult {
                    role,
                    native_content,
                    call_id,
                    call_id_unavailable,
                    content_unavailable,
                    native_content_unavailable,
                    literal_facts,
                } => (
                    EventType::ToolOutput,
                    role,
                    CursorEventBody::ToolOutput {
                        native_content,
                        call_id,
                        call_id_unavailable,
                        content_unavailable,
                        native_content_unavailable,
                        literal_facts,
                    },
                ),
            };
            let provider_event_hash = cursor_logical_event_hash(
                event_type,
                role,
                occurred_at.map(|value| value.timestamp_millis()),
                &body,
            )?;
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
                record_byte_start: record.byte_start,
                record_byte_end_exclusive: record.byte_end_exclusive,
                record_sha256: record.record_sha256,
                provider_event_hash,
            })
        })
        .collect()
}

#[derive(Default)]
pub(super) struct CursorTimestampState(Option<DateTime<Utc>>);

impl CursorTimestampState {
    /// Resumes the carried turn timestamp across an append boundary. The
    /// shared family supplies provider state only when resuming a certified
    /// prefix, so a cold or replacement scan always starts empty.
    pub(super) fn resume(checkpoint: Option<&TypedKey>) -> Self {
        Self(match checkpoint {
            Some(TypedKey::I64(unix_ms)) => DateTime::from_timestamp_millis(*unix_ms),
            _ => None,
        })
    }

    pub(super) fn checkpoint(&self) -> Option<TypedKey> {
        self.0
            .map(|carried| TypedKey::I64(carried.timestamp_millis()))
    }

    pub(super) fn apply(&mut self, events: &mut [CursorNativeEvent]) -> serde_json::Result<()> {
        if let Some(occurred_at) = events.iter().find_map(|event| {
            event
                .occurred_at
                .or_else(|| embedded_cursor_timestamp(event))
        }) {
            self.0 = Some(occurred_at);
        }
        for event in events {
            if event.occurred_at.is_none() {
                event.occurred_at = self.0;
                event.provider_event_hash = cursor_logical_event_hash(
                    event.event_type,
                    event.role,
                    event.occurred_at.map(|value| value.timestamp_millis()),
                    &event.body,
                )?;
            }
        }
        Ok(())
    }
}

fn embedded_cursor_timestamp(event: &CursorNativeEvent) -> Option<DateTime<Utc>> {
    let CursorEventBody::Text { text } = &event.body else {
        return None;
    };
    let raw = text
        .strip_prefix("<timestamp>")?
        .split_once("</timestamp>")?
        .0;
    let (local, offset) = raw.strip_suffix(')')?.rsplit_once(" (UTC")?;
    let local = NaiveDateTime::parse_from_str(local, "%A, %b %-d, %Y, %-I:%M %p").ok()?;
    let (hours, minutes) = offset
        .get(1..)?
        .split_once(':')
        .map_or((offset.get(1..)?, "0"), |parts| parts);
    let seconds = hours.parse::<i32>().ok()?.checked_mul(3_600)?
        + minutes.parse::<i32>().ok()?.checked_mul(60)?;
    let seconds = match offset.as_bytes().first()? {
        b'+' => seconds,
        b'-' => -seconds,
        _ => return None,
    };
    FixedOffset::east_opt(seconds)?
        .from_local_datetime(&local)
        .single()
        .map(|value| value.with_timezone(&Utc))
}

fn cursor_logical_event_hash(
    event_type: EventType,
    role: EventRole,
    occurred_at_unix_ms: Option<i64>,
    body: &CursorEventBody,
) -> serde_json::Result<[u8; 32]> {
    let encoded = match body {
        CursorEventBody::None => serde_json::to_vec(&(
            "cursor-logical-event-v2",
            event_type,
            role,
            occurred_at_unix_ms,
            "none",
            serde_json::Value::Null,
        )),
        CursorEventBody::Text { text } => serde_json::to_vec(&(
            "cursor-logical-event-v2",
            event_type,
            role,
            occurred_at_unix_ms,
            "text",
            text,
        )),
        CursorEventBody::ToolCall { native_content, .. } => serde_json::to_vec(&(
            "cursor-logical-event-v2",
            event_type,
            role,
            occurred_at_unix_ms,
            "tool_call",
            native_content,
        )),
        CursorEventBody::ToolOutput { native_content, .. } => serde_json::to_vec(&(
            "cursor-logical-event-v2",
            event_type,
            role,
            occurred_at_unix_ms,
            "tool_output",
            native_content,
        )),
    }?;
    Ok(Sha256::digest(encoded).into())
}
