//! OpenClaw source-backed legacy JSONL capture.

use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{EventRole, EventType};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{
    common::io::OpenedProviderSourceFile,
    provider::{
        file_touches::visit_all_file_touch_drafts,
        normalization::{provider_capped_json, provider_local_preview, provider_timestamp_value},
    },
    CaptureError, OutputObservationKind, OutputOutcome, Result, MAX_PROVIDER_JSONL_LINE_BYTES,
    OPENCLAW_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS,
};

use super::{
    complete_content, normalization, openclaw_agent_id, openclaw_output_metadata,
    OpenClawFrozenFileMetadata, OpenClawSessionObservation,
};

const CHECKPOINT_VERSION: u32 = 1;
const PARSER_REVISION: u32 = 1;
const POLICY_REVISION: u32 = 1;
const PREFIX_HASH_DOMAIN: &[u8] = b"ctx-openclaw-nativepath-prefix-v1\0";

const PAGE_MAX_RECORDS: usize = crate::provider::native_ingestion::NATIVE_INGESTION_PAGE_MAX_UNITS;
const PAGE_MAX_BYTES: usize = crate::provider::native_ingestion::NATIVE_INGESTION_PAGE_MAX_BYTES;
const PAGE_ENVELOPE_BYTES: usize = 2 * 1024;
const EVENT_ENVELOPE_BYTES: usize = 1024;

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
struct SessionState {
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
    session: SessionState,
    terminal: bool,
}

impl Checkpoint {
    fn supported(&self) -> bool {
        self.version == CHECKPOINT_VERSION
            && self.parser_revision == PARSER_REVISION
            && self.policy_revision == POLICY_REVISION
    }
}

#[derive(Debug, Clone)]
struct SessionFact {
    state: SessionState,
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
    native_record_id: Option<String>,
    byte_start: u64,
    byte_end_exclusive: u64,
    record_digest: [u8; 32],
    provider_event_sequence_index: u64,
    event_type: EventType,
    role: Option<EventRole>,
    occurred_at: DateTime<Utc>,
    lexical_text: String,
}

#[derive(Debug)]
struct CoreTouch {
    event_ordinal: Option<u64>,
    path: String,
}

#[derive(Debug)]
struct Rejection {
    reason: String,
}

#[derive(Debug)]
struct Page {
    expected_checkpoint: Checkpoint,
    next_checkpoint: Checkpoint,
    session: SessionFact,
    events: Vec<CoreEvent>,
    touches: Vec<CoreTouch>,
    rejections: Vec<Rejection>,
    terminal: bool,
}

struct ScanOutcome {
    checkpoint: Checkpoint,
}

struct PageReader {
    path: PathBuf,
    imported_at: DateTime<Utc>,
    observation: OpenClawSessionObservation,
    source_revision: String,
    generation: u64,
    reader: BufReader<File>,
    admitted_transcript: OpenedProviderSourceFile,
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

mod reader;
mod routes;
mod source_backed;

use reader::*;
use routes::*;

pub(crate) use source_backed::{
    openclaw_source_backed_adapter_v0, OpenClawHydratedRecordV0, OpenClawSourceBackedAdapterV0,
    OpenClawSourceBackedDispositionV0, OpenClawSourceBackedErrorV0, OpenClawSourceBackedPageV0,
    OpenClawSourceBackedReaderV0, OpenClawSourceBackedResultV0, OpenClawSourceBackedScanV0,
    OpenClawSourceBackedSourceV0, OpenClawSourceBackedVerifiedPrefixV0,
};
