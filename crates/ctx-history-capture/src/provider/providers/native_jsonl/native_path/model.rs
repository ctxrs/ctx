use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventRole, EventType, FileChangeKind, SessionStatus,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(crate) const DIRECT_JSONL_NATIVEPATH_PARSER_REVISION: u32 = 1;
pub(crate) const DIRECT_JSONL_NATIVEPATH_POLICY_REVISION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DirectJsonlObservedTime {
    pub(crate) before_epoch: bool,
    pub(crate) seconds: u64,
    pub(crate) nanos: u32,
}

impl DirectJsonlObservedTime {
    pub(crate) fn from_system_time(value: SystemTime) -> Self {
        match value.duration_since(UNIX_EPOCH) {
            Ok(duration) => Self {
                before_epoch: false,
                seconds: duration.as_secs(),
                nanos: duration.subsec_nanos(),
            },
            Err(error) => {
                let duration = error.duration();
                Self {
                    before_epoch: true,
                    seconds: duration.as_secs(),
                    nanos: duration.subsec_nanos(),
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DirectJsonlFileObservation {
    pub(crate) length: u64,
    pub(crate) modified: DirectJsonlObservedTime,
    pub(crate) readonly: bool,
    pub(crate) device: Option<u64>,
    pub(crate) inode: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct DirectJsonlSession {
    pub(crate) native_session_id: String,
    pub(crate) provider_session_id: String,
    pub(crate) parent_provider_session_id: Option<String>,
    pub(crate) root_provider_session_id: Option<String>,
    pub(crate) external_agent_id: Option<String>,
    pub(crate) agent_type: AgentType,
    pub(crate) role_hint: Option<String>,
    pub(crate) is_primary: bool,
    pub(crate) status: SessionStatus,
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) ended_at: Option<DateTime<Utc>>,
    pub(crate) cwd: Option<String>,
    pub(crate) metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct DirectJsonlTouch {
    pub(crate) path: String,
    pub(crate) old_path: Option<String>,
    pub(crate) change_kind: Option<FileChangeKind>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct DirectJsonlEvent {
    pub(crate) raw_ordinal: u64,
    pub(crate) sub_ordinal: u32,
    pub(crate) native_record_id: Option<String>,
    pub(crate) provider_event_sequence_index: u64,
    pub(crate) provider_event_hash: String,
    pub(crate) event_type: EventType,
    pub(crate) role: EventRole,
    pub(crate) occurred_at: DateTime<Utc>,
    #[serde(skip)]
    pub(crate) lexical_text: String,
    pub(crate) metadata: Value,
    pub(crate) touches: Vec<DirectJsonlTouch>,
    #[serde(skip, default)]
    pub(crate) source_record: DirectJsonlSourceRecord,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DirectJsonlSourceRecord {
    pub(crate) byte_start: u64,
    pub(crate) byte_end_exclusive: u64,
    pub(crate) record_digest: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DirectJsonlRejection {
    pub(crate) raw_ordinal: u64,
    pub(crate) byte_start: u64,
    pub(crate) byte_end_exclusive: u64,
    pub(crate) reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DirectJsonlCheckpoint {
    pub(crate) version: u32,
    pub(crate) parser_revision: u32,
    pub(crate) policy_revision: u32,
    pub(crate) provider: CaptureProvider,
    pub(crate) source_format: String,
    pub(crate) source_path: PathBuf,
    pub(crate) source_observation: DirectJsonlFileObservation,
    pub(crate) complete_prefix_end: u64,
    pub(crate) complete_prefix_sha256: [u8; 32],
    pub(crate) next_raw_ordinal: u64,
    pub(crate) accepted_events: u64,
    pub(crate) accepted_file_touches: u64,
    pub(crate) rejected_records: u64,
    pub(crate) session: Option<DirectJsonlSession>,
    pub(crate) terminal: bool,
}

impl DirectJsonlCheckpoint {
    pub(crate) const VERSION: u32 = 1;

    pub(crate) fn is_supported_for(&self, provider: CaptureProvider, source_format: &str) -> bool {
        self.version == Self::VERSION
            && self.parser_revision == DIRECT_JSONL_NATIVEPATH_PARSER_REVISION
            && self.policy_revision == DIRECT_JSONL_NATIVEPATH_POLICY_REVISION
            && self.provider == provider
            && self.source_format == source_format
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DirectJsonlSourceChange {
    Fresh,
    Unchanged,
    Append,
    Rewrite,
    Truncation,
    Replacement,
}

#[derive(Debug)]
pub(crate) struct DirectJsonlPage {
    pub(crate) expected_checkpoint: DirectJsonlCheckpoint,
    pub(crate) next_checkpoint: DirectJsonlCheckpoint,
    pub(crate) events: Vec<DirectJsonlEvent>,
    pub(crate) rejections: Vec<DirectJsonlRejection>,
    // Bounded page accounting and terminal state remain part of the cross-target
    // scanner contract even when the Core coordinator uses checkpoints.
    #[allow(dead_code)]
    pub(crate) logical_units: usize,
    #[allow(dead_code)]
    pub(crate) conservative_serialized_bytes: usize,
    #[allow(dead_code)]
    pub(crate) terminal: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DirectJsonlScanOutcome {
    pub(crate) checkpoint: DirectJsonlCheckpoint,
    pub(crate) source_change: DirectJsonlSourceChange,
    pub(crate) source_sha256: [u8; 32],
    pub(crate) accepted_events: u64,
    pub(crate) accepted_file_touches: u64,
    pub(crate) rejected_records: u64,
}
