use std::path::PathBuf;

use chrono::{DateTime, Utc};
use ctx_history_core::{EventRole, EventType};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::common::time::parse_rfc3339_utc;
use crate::Result;

use super::{
    checkpoint::{CursorCheckpoint, CursorSessionCheckpoint},
    parser::{CursorRecordRejection, CursorSafePart, CursorSanitizedRecord},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct CursorNativeOrder {
    pub(crate) semantic_ordinal: u64,
    pub(crate) part_ordinal: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CursorEventIdentity {
    pub(crate) semantic_ordinal: u64,
    pub(crate) part_ordinal: u32,
}

impl CursorEventIdentity {
    pub(crate) fn provider_identity(&self) -> String {
        format!(
            "cursor-semantic-v1:{}:{}",
            self.semantic_ordinal, self.part_ordinal
        )
    }
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
    pub(crate) identity: CursorEventIdentity,
    pub(crate) native_order: CursorNativeOrder,
    pub(crate) event_type: EventType,
    pub(crate) role: EventRole,
    pub(crate) occurred_at: Option<DateTime<Utc>>,
    pub(crate) body: CursorEventBody,
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

/// Keep the provider-to-publication handoff small enough that a sink can apply
/// backpressure without retaining a corpus-sized event vector.
pub(crate) const CURSOR_PUBLICATION_PAGE_MAX_ROWS: usize = 64;
pub(crate) const CURSOR_PUBLICATION_PAGE_MAX_BYTES: usize = 8 * 1024 * 1024;

// This is a conservative upper bound for the page object, including its
// envelope metadata and a future 64-byte SHA-256 page digest. Event fields
// below add their own fixed envelope allowance and worst-case JSON escaping.
const CURSOR_PUBLICATION_PAGE_ENVELOPE_BYTES: usize = 1_024;
const CURSOR_PUBLICATION_EVENT_ENVELOPE_BYTES: usize = 768;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CursorPublicationPage {
    /// Exact certified parser position before this page.
    pub(crate) expected_checkpoint: CursorCheckpoint,
    /// Exact certified parser position after this page, including physical
    /// progress through deliberately excluded records and rejections.
    pub(crate) next_checkpoint: CursorCheckpoint,
    pub(crate) rejected_records: u64,
    pub(crate) rejections: Vec<CursorRecordRejection>,
    pub(crate) events: Vec<CursorNativeEvent>,
    /// Conservative upper bound for the serialized page payload.
    pub(crate) serialized_bytes: usize,
    /// Bytes owned by retained event strings in this page.
    pub(crate) retained_bytes: usize,
}

pub(crate) trait CursorPublicationSink {
    /// Start the caller-owned transaction receiving this source's pages.
    fn begin_cursor_publication(&mut self) -> Result<()>;

    fn stage_cursor_page(&mut self, page: CursorPublicationPage) -> Result<()>;

    /// Discard every page staged since `begin_cursor_publication`.
    fn abort_cursor_publication(&mut self);

    /// Atomically publish every page staged since `begin_cursor_publication`.
    fn commit_cursor_publication(&mut self) -> Result<()>;
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct CursorPageStats {
    pub(super) pages: u64,
    pub(super) rows: u64,
    pub(super) serialized_bytes: u64,
    pub(super) max_page_rows: usize,
    pub(super) max_page_bytes: usize,
}

pub(super) struct CursorPageBuffer<'a> {
    sink: &'a mut dyn CursorPublicationSink,
    events: Vec<CursorNativeEvent>,
    serialized_bytes: usize,
    retained_bytes: usize,
    expected_checkpoint: CursorCheckpoint,
    next_checkpoint: CursorCheckpoint,
    rejected_records: u64,
    rejections: Vec<CursorRecordRejection>,
    stats: CursorPageStats,
}

impl<'a> CursorPageBuffer<'a> {
    pub(super) fn new(
        sink: &'a mut dyn CursorPublicationSink,
        expected_checkpoint: CursorCheckpoint,
    ) -> Self {
        Self {
            sink,
            events: Vec::new(),
            serialized_bytes: CURSOR_PUBLICATION_PAGE_ENVELOPE_BYTES,
            retained_bytes: 0,
            next_checkpoint: expected_checkpoint.clone(),
            expected_checkpoint,
            rejected_records: 0,
            rejections: Vec::new(),
            stats: CursorPageStats::default(),
        }
    }

    pub(super) fn push(
        &mut self,
        event: CursorNativeEvent,
        next_checkpoint: &CursorCheckpoint,
        rejected_records: u64,
        rejections: &[CursorRecordRejection],
    ) -> Result<()> {
        let event_bytes = serialized_event_upper_bound(&event);
        let separator_bytes = usize::from(!self.events.is_empty());
        if !self.events.is_empty()
            && (self.events.len() >= CURSOR_PUBLICATION_PAGE_MAX_ROWS
                || self
                    .serialized_bytes
                    .saturating_add(separator_bytes)
                    .saturating_add(event_bytes)
                    > CURSOR_PUBLICATION_PAGE_MAX_BYTES)
        {
            self.flush()?;
        }
        if self.events.is_empty()
            && self.serialized_bytes.saturating_add(event_bytes) > CURSOR_PUBLICATION_PAGE_MAX_BYTES
        {
            return Err(crate::CaptureError::SystemInvariant(
                "Cursor event exceeds the serialized publication page limit",
            ));
        }
        self.serialized_bytes = self
            .serialized_bytes
            .saturating_add(usize::from(!self.events.is_empty()))
            .saturating_add(event_bytes);
        self.retained_bytes = self
            .retained_bytes
            .saturating_add(retained_event_bytes(&event));
        self.events.push(event);
        self.next_checkpoint.clone_from(next_checkpoint);
        self.rejected_records = rejected_records;
        self.rejections = rejections.to_vec();
        Ok(())
    }

    pub(super) fn finish(
        mut self,
        final_checkpoint: CursorCheckpoint,
        rejected_records: u64,
        rejections: &[CursorRecordRejection],
    ) -> Result<CursorPageStats> {
        self.next_checkpoint = final_checkpoint;
        self.rejected_records = rejected_records;
        self.rejections = rejections.to_vec();
        if !self.events.is_empty() || self.expected_checkpoint != self.next_checkpoint {
            self.flush()?;
        }
        Ok(self.stats)
    }

    fn flush(&mut self) -> Result<()> {
        let events = std::mem::take(&mut self.events);
        let serialized_bytes = std::mem::replace(
            &mut self.serialized_bytes,
            CURSOR_PUBLICATION_PAGE_ENVELOPE_BYTES,
        );
        let retained_bytes = std::mem::take(&mut self.retained_bytes);
        self.stats.pages = self.stats.pages.saturating_add(1);
        self.stats.rows = self.stats.rows.saturating_add(events.len() as u64);
        self.stats.serialized_bytes = self
            .stats
            .serialized_bytes
            .saturating_add(serialized_bytes as u64);
        self.stats.max_page_rows = self.stats.max_page_rows.max(events.len());
        self.stats.max_page_bytes = self.stats.max_page_bytes.max(serialized_bytes);
        let next_checkpoint = self.next_checkpoint.clone();
        let result = self.sink.stage_cursor_page(CursorPublicationPage {
            expected_checkpoint: self.expected_checkpoint.clone(),
            next_checkpoint: next_checkpoint.clone(),
            rejected_records: self.rejected_records,
            rejections: self.rejections.clone(),
            events,
            serialized_bytes,
            retained_bytes,
        });
        if result.is_ok() {
            self.expected_checkpoint = next_checkpoint;
        }
        result
    }
}

fn retained_event_bytes(event: &CursorNativeEvent) -> usize {
    event
        .provider_event_hash
        .len()
        .saturating_add(match &event.body {
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
        })
}

fn serialized_event_upper_bound(event: &CursorNativeEvent) -> usize {
    let string_bytes = event
        .provider_event_hash
        .len()
        .saturating_add(match &event.body {
            CursorEventBody::None => 0,
            CursorEventBody::Text { text } => text.len(),
            CursorEventBody::ToolCall {
                call_id,
                tool_name,
                input_paths,
            } => call_id
                .as_deref()
                .map_or(0, str::len)
                .saturating_add(tool_name.as_deref().map_or(0, str::len))
                .saturating_add(input_paths.iter().map(String::len).sum::<usize>()),
        });
    // Every UTF-8 input byte can conservatively occupy six JSON bytes as a
    // control escape. The fixed allowance covers keys, enum/tag metadata,
    // ordinals, timestamps, nulls, quotes, and separators.
    CURSOR_PUBLICATION_EVENT_ENVELOPE_BYTES.saturating_add(string_bytes.saturating_mul(6))
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
            let (event_type, role, body) = match part {
                CursorSafePart::BodyFree { event_type, role } => {
                    (*event_type, *role, CursorEventBody::None)
                }
                CursorSafePart::Text {
                    event_type,
                    role,
                    text,
                } => (
                    *event_type,
                    *role,
                    CursorEventBody::Text { text: text.clone() },
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
                ),
            };
            let encoded =
                serde_json::to_vec(&("cursor-event-payload-v1", event_type, role, &body))?;
            Ok(CursorNativeEvent {
                identity: CursorEventIdentity {
                    semantic_ordinal: record.semantic_ordinal,
                    part_ordinal,
                },
                native_order: CursorNativeOrder {
                    semantic_ordinal: record.semantic_ordinal,
                    part_ordinal,
                },
                event_type,
                role,
                occurred_at: record.timestamp.as_deref().and_then(parse_rfc3339_utc),
                body,
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
