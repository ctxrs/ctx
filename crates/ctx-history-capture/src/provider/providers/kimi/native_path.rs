use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    Confidence, ContentRef, Event, EventType, Fidelity, FileChangeKind, FileTouched, Run,
    RunStatus, RunType, Session, SessionEdge, SessionEdgeType, SessionStatus, SyncCursor,
};
use ctx_history_store::{
    decode_native_path_committed_cursor, CanonicalActor, EventSearchBulkGuard,
    NativePathCursorSetClassification, NativePathCursorTransition, NativePathGroupAccounting,
    ProviderEventHashAuthority, ProviderSourceLocatorObservation, ProviderSourceRouteRetirement,
    ProviderSourceRouteRetirementDisposition, ProviderSourceRouteRetirementReason, Store,
    NATIVE_PATH_MAX_RETAINED_PAGE_BYTES,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    complete_content::{
        attach_verified_content_locator, verified_content_profile, CompleteContentBodyDigest,
        CompleteContentSourceFamily, VerifiedContentLocatorV1, VerifiedContentRole,
        COMPLETE_CONTENT_MAX_BODY_BYTES, VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
    },
    compute_payload_hash,
    native_source::NativePosition,
    provider::{
        file_touches::{
            event_type_supports_structured_file_touches,
            visit_provider_file_touch_drafts_with_limit, MAX_PACKED_PROVIDER_EVENT_INDEX,
            MAX_PROVIDER_FILE_TOUCHES_PER_EVENT, PROVIDER_FILE_TOUCH_LIMIT_REJECTION,
        },
        importer::{
            certified_provider_sync_cursor, compact_provider_result_payload,
            provider_event_import_identity_with_exact_legacy_source, provider_file_touch_import_id,
            provider_import_session_uuid, provider_path_identity,
            provider_scoped_source_identity_key, provider_session_uuid,
            provider_source_cursor_stream_for_path, provider_source_identity,
            provider_sync_metadata, timestamps, BoundedParserCheckpoint, CertifiedProviderCursor,
        },
        native_ingestion::{
            process_pro_replay_only, NativePageAccounting, NativeProOutputPage,
            NativeProReplayPage, NativeSafeFrontier, NativeSourceIdentity,
            NATIVE_INGESTION_PAGE_MAX_BYTES,
        },
        normalization::{
            provider_local_preview, provider_output_event_is_failure,
            provider_result_outcome_evidence,
        },
        tool_input,
    },
    stable_capture_uuid, CaptureError, CaptureWorkLimit, OutputAssociations, OutputCommandContext,
    OutputNativeCoordinate, OutputObservationKind, OutputOutcome, OutputOutcomeMetadata,
    OutputSourceIdentity, OutputSourceLocator, ProOutputObservation, ProOutputProgress,
    ProOutputSink, ProOutputSinkError, ProOutputSourceDisposition, ProviderAdapterContext,
    ProviderImportFailure, ProviderImportOptions, ProviderImportSummary, ProviderImportWorkResult,
    Result, KIMI_CODE_CLI_SOURCE_FORMAT, MAX_PROVIDER_JSONL_LINE_BYTES, PROVIDER_MAX_PREVIEW_CHARS,
    PROVIDER_MAX_TEXT_CHARS,
};

use super::{
    event::{
        kimi_event, kimi_event_text, kimi_event_type, kimi_legacy_provider_event_hash,
        kimi_output_content, kimi_record_timestamp, KimiCoreEvent,
    },
    kimi_admission_scope_revision, kimi_admission_scope_revision_for_display,
    layout::{canonical_source_root_for_wire, KimiFrozenFileMetadata, KimiWireRoute},
    source::{KimiWireObservation, KimiWireSessionState},
};

const KIMI_NATIVE_CAPTURE_REVISION: u32 = 6;
const KIMI_NATIVE_POLICY_REVISION: u32 = 8;
const KIMI_NATIVE_CURSOR_VERSION: u32 = 1;
const KIMI_NATIVE_POSITION_KIND: &str = "kimi-nativepath-jsonl-frontier-v1";
const KIMI_NATIVE_PAGE_MAX_UNITS: usize = 56;
const KIMI_NATIVE_DISCOVERY_MAX_DEPTH: usize = 16;
const KIMI_NATIVE_DISCOVERY_MAX_ENTRIES: usize = 65_536;
const KIMI_OUTPUT_FRONTIER_VERSION: u32 = 1;
const KIMI_OUTPUT_PARSER_REVISION: &str = "kimi-nativepath-output-v2";
const KIMI_OUTPUT_PAGE_MAX_OBSERVATIONS: usize = 64;
const KIMI_JSONL_LOCATOR_KIND: &str = "jsonl-exact-range-v1";
const KIMI_OUTPUT_LOCATOR_KIND: &str = "jsonl-source-item-byte-range-v1";
const KIMI_PREFIX_DOMAIN: &[u8] = b"ctx-kimi-nativepath-prefix-v1\0";
const KIMI_ROUTE_DOMAIN: &[u8] = b"ctx-kimi-nativepath-route-v1\0";
const KIMI_PUBLICATION_DOMAIN: &[u8] = b"ctx-kimi-nativepath-publication-v1\0";
const KIMI_RETIREMENT_DOMAIN: &[u8] = b"ctx-kimi-nativepath-retirement-v1\0";
const SOURCE_REVISION_DIGEST_DOMAIN: &[u8] = b"ctx-complete-content-source-revision-v1\0";
const PATH_IDENTITY_DIGEST_DOMAIN: &[u8] = b"ctx-complete-content-path-identity-v1\0";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KimiNativeCheckpoint {
    version: u32,
    route_sha256: [u8; 32],
    physical_device: Option<u64>,
    physical_inode: Option<u64>,
    observed_file_len: u64,
    wire_revision: String,
    auxiliary_revision: u64,
    admission_scope_revision: String,
    complete_offset: u64,
    next_ordinal: u64,
    committed_prefix_sha256: [u8; 32],
    started_at: Option<DateTime<Utc>>,
    emitted_session: bool,
    accepted_events: u64,
    accepted_file_touches: u64,
    rejected_records: u64,
    #[serde(default)]
    rejected_outputs: u64,
    terminal: bool,
    retired: bool,
}

impl KimiNativeCheckpoint {
    fn initial(
        route_sha256: [u8; 32],
        observation: &KimiWireObservation,
        admission_scope_revision: String,
    ) -> Self {
        let (physical_device, physical_inode) = observation.wire().physical_identity();
        Self {
            version: KIMI_NATIVE_CURSOR_VERSION,
            route_sha256,
            physical_device,
            physical_inode,
            observed_file_len: observation.wire().length,
            wire_revision: observation.wire().revision_component(),
            auxiliary_revision: observation.session.auxiliary_revision,
            admission_scope_revision,
            complete_offset: 0,
            next_ordinal: 0,
            committed_prefix_sha256: initial_prefix_sha256(),
            started_at: observation.session.started_at,
            emitted_session: false,
            accepted_events: 0,
            accepted_file_touches: 0,
            rejected_records: 0,
            rejected_outputs: 0,
            terminal: false,
            retired: false,
        }
    }

    fn frontier(&self) -> KimiNativeFrontier {
        KimiNativeFrontier {
            complete_offset: self.complete_offset,
            next_ordinal: self.next_ordinal,
            committed_prefix_sha256: self.committed_prefix_sha256,
        }
    }

    fn safe_frontier(&self) -> Result<NativeSafeFrontier> {
        NativeSafeFrontier::new(KIMI_OUTPUT_FRONTIER_VERSION, serde_json::to_vec(self)?)
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KimiNativeFrontier {
    complete_offset: u64,
    next_ordinal: u64,
    committed_prefix_sha256: [u8; 32],
}

#[derive(Clone)]
struct KimiCommittedSource {
    checkpoint: Option<KimiNativeCheckpoint>,
    source_revision: String,
}

#[derive(Clone, Debug, Serialize)]
enum KimiCoreUnit {
    Event {
        raw_ordinal: u64,
        event: KimiCoreEvent,
    },
    FileTouch(KimiFileTouch),
    Rejection {
        line: usize,
        reason: String,
    },
}

#[derive(Clone, Debug, Serialize)]
struct KimiFileTouch {
    provider_touch_index: u64,
    provider_event_index: Option<u64>,
    path: String,
    change_kind: Option<FileChangeKind>,
    old_path: Option<String>,
    confidence: Confidence,
    occurred_at: DateTime<Utc>,
    metadata: Value,
}

#[derive(Default)]
struct KimiFileTouchCollection {
    touches: Vec<KimiFileTouch>,
    limit_exceeded: bool,
}

impl KimiFileTouchCollection {
    fn emitted(&self) -> usize {
        self.touches.len()
    }

    fn limit_exceeded(&self) -> bool {
        self.limit_exceeded
    }
}

fn kimi_file_touches(
    raw_value: &Value,
    event_type: EventType,
    occurred_at: DateTime<Utc>,
    provider_event_index: Option<u64>,
    provider_touch_base_index: u64,
    include_structured_touches: bool,
) -> Result<KimiFileTouchCollection> {
    if !matches!(
        event_type,
        EventType::ToolCall
            | EventType::ToolOutput
            | EventType::CommandOutput
            | EventType::FileTouched
    ) {
        return Ok(KimiFileTouchCollection::default());
    }

    let mut touches = Vec::new();
    let outcome = visit_provider_file_touch_drafts_with_limit(
        raw_value,
        include_structured_touches,
        MAX_PROVIDER_FILE_TOUCHES_PER_EVENT,
        |(ordinal, draft)| {
            let provider_touch_index = match provider_event_index {
                Some(index) if index > MAX_PACKED_PROVIDER_EVENT_INDEX => ordinal,
                _ => provider_touch_base_index | ordinal,
            };
            touches.push(KimiFileTouch {
                provider_touch_index,
                provider_event_index,
                path: draft.path,
                change_kind: draft.change_kind,
                old_path: draft.old_path,
                confidence: draft.confidence,
                occurred_at,
                metadata: draft.metadata,
            });
            Ok::<(), CaptureError>(())
        },
    )?;
    Ok(KimiFileTouchCollection {
        touches,
        limit_exceeded: outcome.limit_exceeded(),
    })
}

#[derive(Clone, Default, Debug, Serialize)]
struct KimiCorePage {
    session_first_observed: bool,
    units: Vec<KimiCoreUnit>,
}

impl KimiCorePage {
    fn can_push(
        &self,
        checkpoint: &KimiNativeCheckpoint,
        units: &[KimiCoreUnit],
        session_first_observed: bool,
    ) -> Result<bool> {
        let mut candidate = self.clone();
        candidate.session_first_observed |= session_first_observed;
        candidate.units.extend_from_slice(units);
        Ok(candidate.units.len() <= KIMI_NATIVE_PAGE_MAX_UNITS
            && core_page_retained_bytes(checkpoint, &candidate)?
                <= NATIVE_PATH_MAX_RETAINED_PAGE_BYTES)
    }

    fn push(&mut self, mut units: Vec<KimiCoreUnit>) {
        self.units.append(&mut units);
    }

    fn is_empty(&self) -> bool {
        self.units.is_empty() && !self.session_first_observed
    }

    fn take(&mut self) -> Self {
        Self {
            session_first_observed: std::mem::take(&mut self.session_first_observed),
            units: std::mem::take(&mut self.units),
        }
    }
}

fn core_page_retained_bytes(
    checkpoint: &KimiNativeCheckpoint,
    page: &KimiCorePage,
) -> Result<usize> {
    Ok(serde_json::to_vec(&(checkpoint, page))?.len())
}

struct RawLine {
    bytes: Vec<u8>,
    observed_bytes: u64,
    terminated: bool,
    oversized: bool,
}

struct KimiInventory {
    paths: BTreeSet<PathBuf>,
    source_root: PathBuf,
    root_missing: bool,
}

#[derive(Clone)]
struct KnownKimiRoute {
    path: PathBuf,
    locator_identity: String,
    canonical_source_identity: String,
    source_revision: String,
    current_cursor: SyncCursor,
    checkpoint: Option<KimiNativeCheckpoint>,
    needs_source_root_migration: bool,
}

mod ingestion;
mod outputs;
mod publication;
mod routes;

pub(crate) use ingestion::import_kimi_nativepath_tree;
use ingestion::*;
use outputs::*;
use publication::*;
use routes::*;
