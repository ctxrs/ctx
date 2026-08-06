use std::{path::PathBuf, time::SystemTime};

use chrono::{DateTime, Utc};
use ctx_history_core::{AgentType, EventRole, EventType};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::provider::ctx_retrieval::{ContributionClass, ResultAtom, ResultTerminalStatus};
use crate::{common::io::ProviderSourceRoot, CaptureError};

#[cfg(test)]
pub(crate) const GEMINI_NATIVEPATH_PARSER_REVISION: u32 = 8;
#[cfg(test)]
pub(crate) const GEMINI_NATIVEPATH_POLICY_REVISION: u32 = 5;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GeminiFileObservation {
    pub(crate) length: u64,
    pub(crate) modified: SystemTime,
    pub(crate) readonly: bool,
    pub(crate) device: Option<u64>,
    pub(crate) inode: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum GeminiTranscriptLayout {
    Primary,
    Subagent {
        parent_native_session_id_hint: String,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct GeminiTranscriptSource {
    pub(crate) path: PathBuf,
    pub(crate) relative_path: PathBuf,
    pub(crate) layout: GeminiTranscriptLayout,
    pub(crate) observation: GeminiFileObservation,
    pub(crate) ordinary_file_token: [u8; 32],
    pub(crate) authority_relative_path: PathBuf,
    pub(crate) authority: ProviderSourceRoot,
}

impl PartialEq for GeminiTranscriptSource {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
            && self.relative_path == other.relative_path
            && self.layout == other.layout
            && self.observation == other.observation
            && self.ordinary_file_token == other.ordinary_file_token
            && self.authority_relative_path == other.authority_relative_path
    }
}

impl Eq for GeminiTranscriptSource {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GeminiDiscovery {
    pub(crate) root: PathBuf,
    pub(crate) transcripts: Vec<GeminiTranscriptSource>,
    pub(crate) completed_inventory: bool,
    pub(crate) inventory_sha256: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GeminiSession {
    pub(crate) native_session_id: String,
    pub(crate) parent_native_session_id: Option<String>,
    pub(crate) agent_type: AgentType,
    pub(crate) started_at: Option<DateTime<Utc>>,
    pub(crate) cwd: Option<String>,
    pub(crate) cwd_ambiguous: bool,
    pub(crate) native_kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum GeminiEventIdentity {
    NativeRecordId(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub(crate) struct GeminiNativeOrder {
    pub(crate) raw_ordinal: u64,
    pub(crate) sub_ordinal: u32,
}

/// Exact raw JSONL record evidence retained only as bounded source-backed
/// metadata. The digest covers the complete byte range, including its newline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct GeminiSourceRecordEvidence {
    pub(crate) byte_offset: u64,
    pub(crate) byte_length: u64,
    pub(crate) record_digest: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum GeminiEventBody {
    Message {
        text: String,
        model: Option<String>,
    },
    ToolCall {
        calls: Vec<GeminiToolCall>,
    },
    OutputDiagnostic {
        result: Option<Value>,
        call_id: Option<String>,
        tool_name: Option<String>,
        command: Option<String>,
        command_too_large: bool,
        declared_workdir: Option<String>,
        file_paths: Vec<String>,
        ambiguous_native_fields: bool,
        outcome: String,
        exit_code: Option<i32>,
        duration_ms: Option<u64>,
    },
    StateNotice {
        summary: Option<String>,
    },
    RewindNotice {
        target_native_record_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct GeminiToolCall {
    pub(crate) id: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) args: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct GeminiRetainedEvent {
    pub(crate) identity: GeminiEventIdentity,
    pub(crate) native_order: GeminiNativeOrder,
    pub(crate) source_record: GeminiSourceRecordEvidence,
    pub(crate) event_type: EventType,
    pub(crate) role: EventRole,
    pub(crate) occurred_at: Option<DateTime<Utc>>,
    pub(crate) body: GeminiEventBody,
    pub(crate) body_sha256: [u8; 32],
    pub(crate) preview: String,
    pub(crate) searchable_text: String,
    pub(crate) safe_file_touches: Vec<String>,
    /// Complete-body contributions used only while projecting this source
    /// record. They are deliberately excluded from retained provider data.
    #[serde(skip)]
    pub(crate) extra_body_contributions: Vec<ContributionClass>,
    #[serde(skip)]
    pub(crate) result_terminal_status: Option<ResultTerminalStatus>,
    #[serde(skip)]
    pub(crate) result_atoms: Vec<ResultAtom>,
}

/// The certified scanner position immediately before or after a page. It only
/// covers records actually contained by that page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg(test)]
pub(crate) struct GeminiPageFrontier {
    pub(crate) parser_revision: u32,
    pub(crate) policy_revision: u32,
    pub(crate) complete_prefix_end: u64,
    /// Domain-separated SHA-256 over every raw byte through
    /// `complete_prefix_end`.
    pub(crate) complete_prefix_sha256: [u8; 32],
    pub(crate) source_device: Option<u64>,
    pub(crate) source_inode: Option<u64>,
    pub(crate) next_raw_ordinal: u64,
    pub(crate) retained_event_count: u64,
    pub(crate) rejected_records: u64,
    pub(crate) append_boundary_safe: bool,
    pub(crate) session: Option<GeminiSession>,
}

/// Stable scanner-local identity for one provider-owned Core page. It binds
/// only the safe frontiers and Core payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg(test)]
pub(crate) struct GeminiPageIdentity(pub(crate) [u8; 32]);

#[cfg(test)]
impl GeminiPageIdentity {
    pub(crate) const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GeminiTouchOverflow {
    Count { limit: usize },
    Bytes { limit: usize },
}

impl std::fmt::Display for GeminiTouchOverflow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Count { limit } => write!(
                formatter,
                "Gemini retained event exceeds the {limit} unique file-touch limit"
            ),
            Self::Bytes { limit } => write!(
                formatter,
                "Gemini retained event exceeds the {limit} file-touch byte limit"
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(test)]
pub(crate) enum GeminiRejectionKind {
    InvalidRecord,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(test)]
pub(crate) struct GeminiRejection {
    pub(crate) raw_ordinal: u64,
    pub(crate) byte_start: u64,
    pub(crate) byte_end_exclusive: u64,
    pub(crate) kind: GeminiRejectionKind,
    pub(crate) reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg(test)]
pub(crate) struct GeminiParserMetrics {
    pub(crate) native_records_observed: u64,
    pub(crate) native_record_bytes_observed: u64,
    pub(crate) native_result_records_observed: u64,
    pub(crate) native_result_record_bytes_observed: u64,
    pub(crate) retained_messages: u64,
    pub(crate) retained_tool_calls: u64,
    pub(crate) retained_notices: u64,
    pub(crate) retained_rows: u64,
    pub(crate) ignored_records: u64,
    pub(crate) header_records: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg(test)]
pub(crate) struct GeminiCheckpoint {
    pub(crate) parser_revision: u32,
    pub(crate) policy_revision: u32,
    pub(crate) source_path: PathBuf,
    pub(crate) source_observation: GeminiFileObservation,
    pub(crate) session: Option<GeminiSession>,
    pub(crate) complete_prefix_end: u64,
    pub(crate) complete_prefix_sha256: [u8; 32],
    pub(crate) source_sha256: [u8; 32],
    pub(crate) next_raw_ordinal: u64,
    pub(crate) retained_event_count: u64,
    pub(crate) rejected_records: u64,
    pub(crate) append_boundary_safe: bool,
    pub(crate) terminal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(test)]
pub(crate) struct GeminiPreviousSource {
    pub(crate) checkpoint: GeminiCheckpoint,
    /// True only when a completed current root inventory still contains the
    /// checkpoint's old route. It distinguishes a live copy from relocation.
    pub(crate) prior_route_still_live: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(test)]
pub(crate) enum GeminiSourceChange {
    Fresh,
    Unchanged,
    Append,
    Rewrite,
    Truncation,
    Replacement,
    Relocation,
    LiveCopy,
    Ambiguous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(test)]
pub(crate) enum GeminiPublicationShape {
    ObservationOnly,
    AppendDelta,
    AuthoritativeSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg(test)]
pub(crate) enum GeminiCompleteness {
    TerminalSnapshot,
    NonterminalCompletePrefix { end: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(test)]
pub(crate) struct GeminiLifecycleSignals {
    pub(crate) source_change: GeminiSourceChange,
    pub(crate) publication_shape: GeminiPublicationShape,
    pub(crate) completeness: GeminiCompleteness,
    pub(crate) emitted_zero_rows: bool,
    pub(crate) source_has_zero_retained_rows: bool,
    pub(crate) cursor_advance_allowed: bool,
    pub(crate) content_changed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(test)]
pub(crate) struct GeminiScanOutcome {
    pub(crate) checkpoint: GeminiCheckpoint,
    pub(crate) signals: GeminiLifecycleSignals,
    pub(crate) metrics: GeminiParserMetrics,
    pub(crate) rejected_records: u64,
    pub(crate) rejections: Vec<GeminiRejection>,
    /// Initial and final observations match when this outcome is returned.
    /// This is the terminal source-authority proof; it needs no second decode.
    pub(crate) terminal_source_observation: GeminiFileObservation,
}

#[derive(Debug, Error)]
pub(crate) enum GeminiScanError {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(
        "duplicate Gemini native event id {native_event_id:?} at raw ordinal \
         {duplicate_raw_ordinal}; first observed at raw ordinal {first_raw_ordinal}"
    )]
    DuplicateNativeEventId {
        native_event_id: String,
        first_raw_ordinal: u64,
        duplicate_raw_ordinal: u64,
    },
    #[error("Gemini native event identity count exceeds the bounded limit of {limit}")]
    NativeEventIdentityCountOverflow { limit: usize },
    #[error("Gemini native event identity bytes exceed the bounded limit of {limit}")]
    NativeEventIdentityBytesOverflow { limit: usize },
    #[error(
        "Gemini record at raw ordinal {raw_ordinal} and byte range \
         {byte_start}..{byte_end_exclusive} remains uncommitted: {reason}"
    )]
    UncommittedRecord {
        raw_ordinal: u64,
        byte_start: u64,
        byte_end_exclusive: u64,
        reason: String,
    },
}

impl From<std::io::Error> for GeminiScanError {
    fn from(error: std::io::Error) -> Self {
        Self::Capture(error.into())
    }
}

pub(crate) type GeminiScanResult<T> = std::result::Result<T, GeminiScanError>;
