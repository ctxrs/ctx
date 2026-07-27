use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    Confidence, Event, EventRole, EventType, Fidelity, FileChangeKind, FileTouched, Session,
    SessionEdge, SessionEdgeType, SessionStatus, SyncCursor,
};
use ctx_history_store::{
    decode_native_path_committed_cursor, CanonicalActor, EventSearchBulkGuard,
    NativePathCursorSetClassification, NativePathCursorTransition, NativePathGroupAccounting,
    ProviderEventHashAuthority, ProviderSourceLocatorObservation, ProviderSourceRouteRetirement,
    ProviderSourceRouteRetirementDisposition, ProviderSourceRouteRetirementReason, Store,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    complete_content::VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
    provider::{
        file_touches::visit_all_file_touch_drafts,
        importer::{
            provider_event_import_identity_with_exact_legacy_source, provider_file_touch_import_id,
            provider_import_session_uuid, provider_path_identity,
            provider_scoped_source_identity_key, provider_scoped_source_uuid,
            provider_source_cursor_stream_for_path, provider_source_identity,
            provider_sync_metadata, timestamps, CertifiedProviderCursor,
        },
        native_ingestion::{
            process_pro_replay_only, NativePageAccounting, NativeProOutputPage,
            NativeProReplayPage, NativeSafeFrontier, NativeSourceIdentity,
            NATIVE_INGESTION_PAGE_MAX_BYTES, NATIVE_INGESTION_PAGE_MAX_UNITS,
        },
        normalization::{provider_capped_json, provider_local_preview, provider_timestamp_value},
    },
    released_jsonl_cursor::released_jsonl_position_offset,
    stable_capture_uuid, CaptureError, CaptureWorkLimit, OutputAssociations,
    OutputNativeCoordinate, OutputObservationKind, OutputOutcome, OutputSourceIdentity,
    OutputSourceLocator, ProOutputObservation, ProOutputProgress, ProOutputSink,
    ProOutputSinkError, ProOutputSourceDisposition, ProviderAdapterContext, ProviderImportFailure,
    ProviderImportOptions, ProviderImportSummary, ProviderImportWorkResult, Result,
    MAX_PROVIDER_JSONL_LINE_BYTES, OPENCLAW_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS,
};

use super::{
    complete_content, normalization, openclaw_agent_id, openclaw_output_metadata,
    OpenClawFrozenFileMetadata, OpenClawSessionObservation, OPENCLAW_RELEASED_CAPTURE_REVISION,
    OPENCLAW_RELEASED_POLICY_REVISION,
};

const CURSOR_VERSION: u32 = 1;
const PARSER_REVISION: u32 = 1;
const POLICY_REVISION: u32 = 1;
const OUTPUT_FRONTIER_VERSION: u32 = 1;
const OUTPUT_PARSER_REVISION: &str = "openclaw-nativepath-jsonl-v1";
const PREFIX_HASH_DOMAIN: &[u8] = b"ctx-openclaw-nativepath-prefix-v1\0";
const PUBLICATION_DOMAIN: &[u8] = b"ctx-openclaw-nativepath-publication-v1\0";
const RETIREMENT_DOMAIN: &[u8] = b"ctx-openclaw-nativepath-retirement-v1\0";

const PAGE_MAX_RECORDS: usize = NATIVE_INGESTION_PAGE_MAX_UNITS;
const PAGE_MAX_BYTES: usize = NATIVE_INGESTION_PAGE_MAX_BYTES;
const PAGE_ENVELOPE_BYTES: usize = 2 * 1024;
const EVENT_ENVELOPE_BYTES: usize = 1024;
const GROUP_MAX_PAGES: usize = 32;
const GROUP_MAX_SOURCES: usize = 64;
const GROUP_MAX_BYTES: usize = 6 * 1024 * 1024;
const GROUP_MAX_ESTIMATED_MUTATIONS: usize = 3_000;
const RELEASED_POSITION_PROOF_LENGTH_START: usize = 20;
const RELEASED_POSITION_DIGEST_START: usize = 24;
const RELEASED_POSITION_ENCODED_BYTES: usize = 56;
const RELEASED_BOUNDARY_MAX_BYTES: u64 = 64 * 1024;
const RELEASED_BOUNDARY_HASH_DOMAIN: &[u8] = b"ctx-jsonl-append-boundary-sha256-v1\0";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct ObservedTime {
    before_epoch: bool,
    seconds: u64,
    nanos: u32,
}

impl ObservedTime {
    fn from_system_time(value: SystemTime) -> Self {
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

    fn to_system_time(self) -> Result<SystemTime> {
        let duration = Duration::new(self.seconds, self.nanos);
        if self.before_epoch {
            UNIX_EPOCH.checked_sub(duration)
        } else {
            UNIX_EPOCH.checked_add(duration)
        }
        .ok_or(CaptureError::SystemInvariant(
            "OpenClaw observed time is outside SystemTime",
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FrozenMetadata {
    length: u64,
    modified: ObservedTime,
    readonly: bool,
    device: Option<u64>,
    inode: Option<u64>,
}

impl FrozenMetadata {
    fn from_live(value: &OpenClawFrozenFileMetadata) -> Self {
        Self {
            length: value.length,
            modified: ObservedTime::from_system_time(value.modified),
            readonly: value.readonly,
            device: value.device,
            inode: value.inode,
        }
    }

    fn to_live(&self) -> Result<OpenClawFrozenFileMetadata> {
        Ok(OpenClawFrozenFileMetadata {
            length: self.length,
            modified: self.modified.to_system_time()?,
            readonly: self.readonly,
            device: self.device,
            inode: self.inode,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SourceObservation {
    transcript: FrozenMetadata,
    index_file: Option<FrozenMetadata>,
    index_revision: u64,
}

impl SourceObservation {
    fn from_live(value: &OpenClawSessionObservation) -> Self {
        Self {
            transcript: FrozenMetadata::from_live(&value.transcript),
            index_file: value.index_file.as_ref().map(FrozenMetadata::from_live),
            index_revision: value.index_revision,
        }
    }

    fn matches_live(&self, value: &OpenClawSessionObservation) -> Result<bool> {
        Ok(self.transcript.to_live()? == value.transcript
            && self
                .index_file
                .as_ref()
                .map(FrozenMetadata::to_live)
                .transpose()?
                == value.index_file
            && self.index_revision == value.index_revision)
    }

    fn auxiliary_matches_live(&self, value: &OpenClawSessionObservation) -> Result<bool> {
        Ok(self
            .index_file
            .as_ref()
            .map(FrozenMetadata::to_live)
            .transpose()?
            == value.index_file
            && self.index_revision == value.index_revision)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct HeaderAnchor {
    start: u64,
    end: u64,
    digest: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SessionCursor {
    provider_session_id: String,
    agent_id: Option<String>,
    parent_provider_session_id: Option<String>,
    root_provider_session_id: Option<String>,
    started_at: DateTime<Utc>,
    cwd: Option<String>,
    header_anchor: Option<HeaderAnchor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Checkpoint {
    version: u32,
    parser_revision: u32,
    policy_revision: u32,
    generation: u64,
    source_path: PathBuf,
    source_observation: SourceObservation,
    route_source_revision: String,
    complete_prefix_end: u64,
    complete_prefix_sha256: [u8; 32],
    next_raw_ordinal: u64,
    accepted_events: u64,
    accepted_file_touches: u64,
    rejected_records: u64,
    session: SessionCursor,
    terminal: bool,
}

impl Checkpoint {
    fn supported(&self) -> bool {
        self.version == CURSOR_VERSION
            && self.parser_revision == PARSER_REVISION
            && self.policy_revision == POLICY_REVISION
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CursorWire {
    version: u32,
    kind: String,
    checkpoint: Checkpoint,
}

enum CursorDecode {
    Native(Checkpoint),
    Migrated(Checkpoint),
    Reset,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleasedSessionState {
    provider_session_id: String,
    agent_id: Option<String>,
    started_at: DateTime<Utc>,
    cwd: Option<String>,
    index_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleasedParserCheckpoint {
    session: ReleasedSessionState,
    next_ordinal: u64,
    header_anchor: Option<HeaderAnchor>,
    emitted_session: bool,
    accepted_events: u64,
}

#[derive(Debug, Clone)]
struct SessionFact {
    cursor: SessionCursor,
    index: Value,
    header: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceChange {
    Fresh,
    Unchanged,
    Append,
    Rewrite,
    Truncation,
    Replacement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileContinuity {
    SamePhysicalFile,
    ExactPathPrefixProof,
    Replacement,
}

#[derive(Debug)]
struct CoreEvent {
    raw_ordinal: u64,
    provider_event_index: u64,
    provider_event_sequence_index: u64,
    provider_event_hash: String,
    cursor: String,
    event_type: EventType,
    role: Option<EventRole>,
    occurred_at: DateTime<Utc>,
    payload: Value,
    metadata: Value,
}

#[derive(Debug)]
struct CoreTouch {
    raw_ordinal: u64,
    event_ordinal: Option<u64>,
    path: String,
    old_path: Option<String>,
    change_kind: Option<FileChangeKind>,
    occurred_at: DateTime<Utc>,
}

#[derive(Debug)]
struct OutputFact {
    raw_ordinal: u64,
    byte_start: u64,
    byte_end_exclusive: u64,
    occurred_at: DateTime<Utc>,
    kind: OutputObservationKind,
    native_record_id: String,
    call_id: Option<String>,
    command: Option<crate::OutputCommandContext>,
    outcome: crate::OutputOutcomeMetadata,
    content: Vec<u8>,
}

#[derive(Debug)]
struct Rejection {
    raw_ordinal: u64,
    reason: String,
}

#[derive(Debug)]
struct Page {
    expected_checkpoint: Checkpoint,
    next_checkpoint: Checkpoint,
    source_change: SourceChange,
    session: SessionFact,
    events: Vec<CoreEvent>,
    touches: Vec<CoreTouch>,
    outputs: Vec<OutputFact>,
    rejections: Vec<Rejection>,
    logical_units: usize,
    conservative_serialized_bytes: usize,
    terminal: bool,
}

struct ScanOutcome {
    checkpoint: Checkpoint,
    source_change: SourceChange,
    accepted_events: u64,
    rejected_records: u64,
}

struct PageReader {
    path: PathBuf,
    imported_at: DateTime<Utc>,
    collect_outputs: bool,
    observation: OpenClawSessionObservation,
    source_revision: String,
    path_identity: String,
    generation: u64,
    reader: BufReader<File>,
    prefix_hasher: Sha256,
    complete_prefix_end: u64,
    next_raw_ordinal: u64,
    accepted_events: u64,
    accepted_file_touches: u64,
    rejected_records: u64,
    session: SessionFact,
    source_change: SourceChange,
    skip_scan: bool,
    finished: bool,
    outcome: Option<ScanOutcome>,
}

mod cursor;
mod lifecycle;
mod output;
mod publication;
mod reader;
mod routes;

use cursor::*;
use lifecycle::*;
use output::*;
use publication::*;
use reader::*;
use routes::*;

pub(crate) use lifecycle::import_openclaw_nativepath_tree;
#[cfg(test)]
pub(super) use routes::{committed_generation_for_test, install_released_cursor_for_test};

#[cfg(test)]
pub(super) fn acquisition_page_accounting_for_test(
    path: &Path,
    imported_at: DateTime<Utc>,
) -> Result<Vec<(usize, usize)>> {
    let mut reader = open_pages(path, imported_at, false, None, false, None)?;
    let mut accounting = Vec::new();
    while let Some(page) = reader.next_page()? {
        accounting.push((page.logical_units, page.conservative_serialized_bytes));
    }
    Ok(accounting)
}
