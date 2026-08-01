use chrono::{DateTime, Utc};
use ctx_history_core::{EventRole, EventType};
use serde::{Deserialize, Serialize};

use crate::{
    provider::native_ingestion::{
        NATIVE_INGESTION_PAGE_MAX_BYTES, NATIVE_INGESTION_PAGE_MAX_UNITS,
    },
    record_evidence::RecordDigest,
};

use super::ZedNativeResult;

pub(crate) const ZED_NATIVE_PAGE_MAX_UNITS: usize = NATIVE_INGESTION_PAGE_MAX_UNITS;
pub(crate) const ZED_NATIVE_PAGE_MAX_BYTES: usize = NATIVE_INGESTION_PAGE_MAX_BYTES;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum ZedNativeEncoding {
    Json,
    Zstd,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ZedNativeSession {
    pub(super) sqlite_rowid: i64,
    pub(super) thread_id: String,
    pub(super) parent_thread_id: Option<String>,
    pub(super) title: String,
    pub(super) payload_title: Option<String>,
    pub(super) summary: String,
    pub(super) created_at: DateTime<Utc>,
    pub(super) updated_at: DateTime<Utc>,
    pub(super) native_created_at: Option<String>,
    pub(super) native_updated_at: String,
    pub(super) cwd: Option<String>,
    pub(super) folder_paths: Vec<String>,
    pub(super) native_folder_paths: Option<String>,
    pub(super) native_folder_paths_order: Option<String>,
    pub(super) native_data_type: String,
    pub(super) encoding: ZedNativeEncoding,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum ZedNativeMessageIdentity {
    ProviderId { value: String, message_ordinal: u64 },
    MessageOrdinal(u64),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ZedNativeEventIdentity {
    pub(super) thread_id: String,
    pub(super) message: ZedNativeMessageIdentity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct ZedNativeOrder {
    pub(super) thread_ordinal: u64,
    pub(super) message_ordinal: u64,
    pub(super) sub_ordinal: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ZedNativeEvent {
    pub(super) sqlite_rowid: i64,
    pub(super) identity: ZedNativeEventIdentity,
    pub(super) native_order: ZedNativeOrder,
    pub(super) record_digest: RecordDigest,
    pub(super) event_type: EventType,
    pub(super) role: EventRole,
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) kind: String,
    pub(super) call_ids: Vec<String>,
    pub(super) native_content: serde_json::Value,
    #[serde(skip)]
    pub(super) normalized_body: String,
    pub(super) safe_file_touches: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum ZedNativeRejectionKind {
    InvalidStorageClass,
    OversizedEncodedCell,
    InvalidCompression,
    OversizedDecompression,
    UnsupportedEncoding,
    MalformedJson,
    MalformedThread,
    UnsupportedThreadVersion,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ZedNativeRejection {
    pub(super) sqlite_rowid: i64,
    pub(super) thread_id: Option<String>,
    pub(super) kind: ZedNativeRejectionKind,
    pub(super) reason: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ZedNativePage {
    pub(super) sessions: Vec<ZedNativeSession>,
    pub(super) events: Vec<ZedNativeEvent>,
    pub(super) rejections: Vec<ZedNativeRejection>,
    pub(super) estimated_bytes: usize,
}

impl ZedNativePage {
    pub(super) fn logical_units(&self) -> usize {
        self.sessions
            .len()
            .saturating_add(self.rejections.len())
            .saturating_add(self.events.iter().fold(0_usize, |units, event| {
                units
                    .saturating_add(1)
                    .saturating_add(event.safe_file_touches.len())
            }))
    }

    pub(super) fn row_count(&self) -> usize {
        self.sessions
            .len()
            .saturating_add(self.events.len())
            .saturating_add(self.rejections.len())
    }

    pub(super) fn is_empty(&self) -> bool {
        self.row_count() == 0
    }
}

pub(crate) trait ZedNativeSink {
    fn push_page(&mut self, page: ZedNativePage) -> ZedNativeResult<()>;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ZedNativeCounters {
    pub(super) candidate_page_queries: u64,
    pub(crate) native_thread_rows: u64,
    pub(crate) certified_logical_bytes: u64,
    pub(super) encoded_payload_bytes: u64,
    pub(super) decompressed_payload_bytes: u64,
    pub(crate) sessions_retained: u64,
    pub(crate) retained_events: u64,
    pub(super) retained_messages: u64,
    pub(super) retained_tool_calls: u64,
    pub(super) retained_summaries: u64,
    pub(super) retained_notices: u64,
    pub(super) retained_body_bytes: u64,
    pub(super) retained_file_touches: u64,
    pub(crate) rejected_threads: u64,
}
