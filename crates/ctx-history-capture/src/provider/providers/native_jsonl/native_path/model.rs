use chrono::{DateTime, Utc};
use ctx_history_core::{AgentType, EventRole, EventType, FileChangeKind, SessionStatus};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::provider::source_backed::family::jsonl::JsonlCheckpoint;

pub(crate) const DIRECT_JSONL_NATIVEPATH_PARSER_REVISION: &str = "direct-native-jsonl-parser-v1";
pub(crate) const DIRECT_JSONL_NATIVEPATH_POLICY_REVISION: &str = "direct-native-jsonl-policy-v1";

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
    pub(crate) physical: JsonlCheckpoint,
    pub(crate) accepted_events: u64,
    pub(crate) accepted_file_touches: u64,
    pub(crate) rejected_records: u64,
    pub(crate) rejection_details: Vec<DirectJsonlRejection>,
    pub(crate) represented_physical_records: u64,
    pub(crate) ignored_records: u64,
    pub(crate) indexed_documents: u64,
    pub(crate) session: Option<DirectJsonlSession>,
}

impl DirectJsonlCheckpoint {
    pub(crate) const VERSION: u32 = 3;

    pub(crate) fn is_internally_consistent(&self) -> bool {
        let Some(classified_physical) = self
            .represented_physical_records
            .checked_add(self.rejected_records)
            .and_then(|value| value.checked_add(self.ignored_records))
        else {
            return false;
        };
        self.version == Self::VERSION
            && self.physical.is_internally_consistent()
            && classified_physical == self.physical.next_physical_ordinal()
            && self.accepted_events == self.indexed_documents
            && self.rejection_details.len()
                <= super::source_backed::DIRECT_JSONL_MAX_REJECTION_DETAILS
            && u64::try_from(self.rejection_details.len())
                .is_ok_and(|details| details <= self.rejected_records)
            && self.rejection_details.iter().all(|rejection| {
                rejection.raw_ordinal < self.physical.next_physical_ordinal()
                    && rejection.byte_start < rejection.byte_end_exclusive
                    && rejection.byte_end_exclusive <= self.physical.complete_prefix_end()
            })
            && self.session.is_some()
    }
}
