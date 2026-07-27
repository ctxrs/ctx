use std::{
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{EventRole, EventType};
use serde::{Deserialize, Serialize};
use tempfile::TempPath;

use super::ZedNativeResult;

pub(crate) const ZED_NATIVE_PAGE_MAX_ROWS: usize = 4_096;
pub(crate) const ZED_NATIVE_PAGE_MAX_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ZedNativeSourceSelection {
    selected_path: PathBuf,
    inventory_observation_token: Option<String>,
}

impl ZedNativeSourceSelection {
    pub(super) fn exact(path: impl Into<PathBuf>) -> Self {
        Self {
            selected_path: path.into(),
            inventory_observation_token: None,
        }
    }

    pub(super) fn with_inventory_observation_token(mut self, token: Option<String>) -> Self {
        self.inventory_observation_token = token;
        self
    }

    pub(super) fn selected_path(&self) -> &Path {
        &self.selected_path
    }

    pub(super) fn inventory_observation_token(&self) -> Option<&str> {
        self.inventory_observation_token.as_deref()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ZedNativeSourceAuthority {
    ExactDispatchedDatabase {
        path: PathBuf,
        inventory_observation_token: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum ZedNativeEncoding {
    Json,
    Zstd,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ZedNativeSession {
    pub(super) thread_id: String,
    pub(super) parent_thread_id: Option<String>,
    pub(super) root_thread_id: String,
    pub(super) title: String,
    pub(super) summary: String,
    pub(super) created_at: DateTime<Utc>,
    pub(super) updated_at: DateTime<Utc>,
    pub(super) cwd: Option<String>,
    pub(super) folder_paths: Vec<String>,
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
    pub(super) event_type: EventType,
    pub(super) role: EventRole,
    pub(super) occurred_at: DateTime<Utc>,
    pub(super) kind: String,
    pub(super) call_ids: Vec<String>,
    pub(super) body: String,
    pub(super) content_hash: String,
    pub(super) preview: String,
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
pub(crate) struct ZedNativeOutputCounters {
    pub(super) native_results_observed: u64,
    pub(super) native_results_success: u64,
    pub(super) native_results_failure: u64,
    pub(super) native_results_unknown: u64,
    pub(super) result_body_bytes_observed: u64,
    pub(super) retained_result_body_bytes: u64,
    pub(super) retained_result_body_strings_allocated: u64,
    pub(super) result_events_created: u64,
    pub(super) result_hashes_created: u64,
    pub(super) result_previews_created: u64,
    pub(super) result_file_touches_created: u64,
    pub(super) result_fts_documents_created: u64,
    pub(super) result_handoffs_created: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ZedNativeCounters {
    pub(super) candidate_page_queries: u64,
    pub(super) hydration_queries: u64,
    pub(super) native_thread_rows: u64,
    pub(super) encoded_payload_bytes: u64,
    pub(super) decompressed_payload_bytes: u64,
    pub(super) sessions_retained: u64,
    pub(super) retained_events: u64,
    pub(super) retained_messages: u64,
    pub(super) retained_tool_calls: u64,
    pub(super) retained_summaries: u64,
    pub(super) retained_notices: u64,
    pub(super) retained_body_bytes: u64,
    pub(super) retained_hashes: u64,
    pub(super) retained_previews: u64,
    pub(super) retained_file_touches: u64,
    pub(super) rejected_threads: u64,
    pub(super) output: ZedNativeOutputCounters,
    pub(super) durable_transaction_rotations: u64,
}

#[derive(Clone)]
pub(crate) struct ZedNativeOutputIndex {
    inner: Arc<ZedNativeOutputIndexInner>,
}

struct ZedNativeOutputIndexInner {
    path: TempPath,
}

impl ZedNativeOutputIndex {
    pub(super) fn new(path: TempPath) -> Self {
        Self {
            inner: Arc::new(ZedNativeOutputIndexInner { path }),
        }
    }

    pub(super) fn path(&self) -> &Path {
        self.inner.path.as_ref()
    }
}

impl fmt::Debug for ZedNativeOutputIndex {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ZedNativeOutputIndex")
            .field("storage", &"private temporary SQLite")
            .finish()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ZedNativeGenerationAuthority {
    pub(super) source_complete: bool,
    pub(super) zero_native_rows: bool,
    pub(super) zero_retained_events: bool,
    pub(super) has_useful_content: bool,
    pub(super) source_authority: ZedNativeSourceAuthority,
    pub(super) physical_locator: String,
    pub(super) snapshot_revision: String,
    pub(super) capability_digest: String,
    pub(super) source_integrity_digest: String,
    pub(super) core_generation_digest: String,
    pub(super) output_index: ZedNativeOutputIndex,
    pub(super) pages_emitted: u64,
    pub(super) counters: ZedNativeCounters,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ZedNativeIncompleteReason {
    SnapshotAcquisitionRace,
    SourceChangedAfterScan,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ZedNativeIncomplete {
    pub(super) source_complete: bool,
    pub(super) reason: ZedNativeIncompleteReason,
    pub(super) physical_locator: String,
    pub(super) pages_emitted: u64,
    pub(super) counters: ZedNativeCounters,
}

#[derive(Clone, Debug)]
pub(crate) enum ZedNativeScanOutcome {
    Complete(Box<ZedNativeGenerationAuthority>),
    Incomplete(Box<ZedNativeIncomplete>),
}
