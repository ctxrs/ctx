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
        kimi_event, kimi_event_text, kimi_event_type, kimi_record_timestamp, kimi_result_content,
        KimiCoreEvent,
    },
    kimi_admission_scope_revision, kimi_admission_scope_revision_for_display,
    layout::KimiFrozenFileMetadata,
    source::{KimiWireObservation, KimiWireSessionState},
};

const KIMI_NATIVE_CAPTURE_REVISION: u32 = 6;
const KIMI_NATIVE_POLICY_REVISION: u32 = 7;
const KIMI_NATIVE_CURSOR_VERSION: u32 = 1;
const KIMI_NATIVE_POSITION_KIND: &str = "kimi-nativepath-jsonl-frontier-v1";
const KIMI_NATIVE_PAGE_MAX_UNITS: usize = 56;
const KIMI_NATIVE_PAGE_MAX_BYTES: usize = 6 * 1024 * 1024;
const KIMI_NATIVE_DISCOVERY_MAX_DEPTH: usize = 16;
const KIMI_NATIVE_DISCOVERY_MAX_ENTRIES: usize = 65_536;
const KIMI_OUTPUT_FRONTIER_VERSION: u32 = 1;
const KIMI_OUTPUT_PARSER_REVISION: &str = "kimi-nativepath-output-v1";
const KIMI_OUTPUT_PAGE_MAX_OBSERVATIONS: usize = 64;
const KIMI_OUTPUT_PAGE_MAX_BYTES: usize = 6 * 1024 * 1024;
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

#[derive(Default, Debug, Serialize)]
struct KimiCorePage {
    session_first_observed: bool,
    units: Vec<KimiCoreUnit>,
    encoded_bytes: usize,
}

impl KimiCorePage {
    fn can_push(&self, units: &[KimiCoreUnit], encoded_bytes: usize) -> bool {
        self.units.len().saturating_add(units.len()) <= KIMI_NATIVE_PAGE_MAX_UNITS
            && self
                .encoded_bytes
                .saturating_add(encoded_bytes)
                .saturating_add(64 * 1024)
                <= KIMI_NATIVE_PAGE_MAX_BYTES
    }

    fn push(&mut self, mut units: Vec<KimiCoreUnit>, encoded_bytes: usize) {
        self.encoded_bytes = self.encoded_bytes.saturating_add(encoded_bytes);
        self.units.append(&mut units);
    }

    fn is_empty(&self) -> bool {
        self.units.is_empty() && !self.session_first_observed
    }

    fn take(&mut self) -> Self {
        Self {
            session_first_observed: std::mem::take(&mut self.session_first_observed),
            units: std::mem::take(&mut self.units),
            encoded_bytes: std::mem::take(&mut self.encoded_bytes),
        }
    }
}

struct RawLine {
    bytes: Vec<u8>,
    observed_bytes: u64,
    terminated: bool,
    oversized: bool,
}

#[derive(Default)]
struct KimiInventory {
    paths: BTreeSet<PathBuf>,
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
}

pub(crate) fn import_kimi_nativepath_tree(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let configured_source_root = context
        .source_root
        .clone()
        .or_else(|| context.source_path.clone())
        .unwrap_or_else(|| path.to_path_buf());
    let inventory = discover_kimi_wire_files(path)?;
    let known_routes = known_kimi_routes(store, &context.machine_id, &configured_source_root)?;
    let sink = options.import_profile.sink().cloned();

    if options.import_profile.is_replay_only() {
        let mut summary = ProviderImportSummary::default();
        if replay_outputs(
            &inventory.paths,
            &configured_source_root,
            context.imported_at,
            sink.as_deref(),
        )? {
            record_output_behind(&mut summary);
        }
        return Ok(summary);
    }

    if inventory.paths.is_empty() && known_routes.is_empty() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "no Kimi Code CLI wire.jsonl transcripts found",
        });
    }

    let committed_store = Store::open_read_only(store.path())?;
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let mut summary = ProviderImportSummary::default();
        let mut changed_groups = 0_usize;
        for wire in &inventory.paths {
            let mut file_context = context.clone();
            file_context.source_path = Some(wire.clone());
            file_context.source_root = Some(configured_source_root.clone());
            let result = import_kimi_core_file(
                wire,
                store,
                &committed_store,
                &bulk_guard,
                file_context,
                &options,
                &mut changed_groups,
            )?;
            summary.merge_from(result);
            if summary.work_remaining {
                return Ok(summary);
            }
        }
        summary.merge_from(retire_missing_routes(
            store,
            &bulk_guard,
            &context.machine_id,
            context.imported_at,
            &known_routes,
            &inventory.paths,
            if inventory.root_missing {
                ProviderSourceRouteRetirementReason::RootMissing
            } else {
                ProviderSourceRouteRetirementReason::SourceMissing
            },
        )?);
        Ok(summary)
    })();
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    let mut summary = match (operation, finish) {
        (Ok(summary), Ok(())) => summary,
        (_, Err(error)) => return Err(error),
        (Err(error), Ok(())) => return Err(error),
    };
    if !summary.work_remaining
        && replay_outputs(
            &inventory.paths,
            &configured_source_root,
            context.imported_at,
            sink.as_deref(),
        )?
    {
        record_output_behind(&mut summary);
    }
    Ok(summary)
}

#[allow(clippy::too_many_arguments)]
fn import_kimi_core_file(
    path: &Path,
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    context: ProviderAdapterContext,
    options: &ProviderImportOptions,
    changed_groups: &mut usize,
) -> Result<ProviderImportSummary> {
    let observation = KimiWireObservation::read(path)?;
    let canonical_path = observation.canonical_path().to_path_buf();
    let locator_identity = provider_path_identity(&canonical_path)?;
    let route_sha256 = route_sha256(&locator_identity);
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::KimiCodeCli,
        KIMI_CODE_CLI_SOURCE_FORMAT,
        &locator_identity,
    );
    let committed = load_committed_source(store, &context.machine_id, &stream)?;
    let admission_scope_revision = kimi_admission_scope_revision(&context);
    let source_revision = effective_source_revision(
        &observation.source_revision(&admission_scope_revision),
        options.inventory_observation_token.as_deref(),
    );
    let (mut checkpoint, start_offset, start_ordinal, mut hasher, unchanged) = plan_core_scan(
        &canonical_path,
        &observation,
        route_sha256,
        admission_scope_revision,
        committed.as_ref(),
        &source_revision,
    )?;
    if unchanged {
        return Ok(replay_summary(&checkpoint));
    }

    let mut file = File::open(&canonical_path)?;
    if KimiFrozenFileMetadata::from_metadata(&file.metadata()?)? != *observation.wire() {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    file.seek(SeekFrom::Start(start_offset))?;
    let mut reader = BufReader::new(file);
    let mut offset = start_offset;
    let mut ordinal = start_ordinal;
    let mut page = KimiCorePage::default();
    let mut summary = ProviderImportSummary::default();
    let canonical_identity = provider_path_identity(&canonical_path)?;
    let content_revision =
        observation.complete_content_revision(&checkpoint.admission_scope_revision);
    let mut reached_eof = false;

    while !reached_eof {
        let checkpoint_before = checkpoint.clone();
        let hasher_before = hasher.clone();
        let raw = read_bounded_line(&mut reader, &mut hasher, MAX_PROVIDER_JSONL_LINE_BYTES)?;
        if raw.observed_bytes == 0 {
            reached_eof = true;
        } else if !raw.terminated {
            hasher = hasher_before;
            reached_eof = true;
        } else {
            let byte_start = offset;
            offset =
                offset
                    .checked_add(raw.observed_bytes)
                    .ok_or(CaptureError::SystemInvariant(
                        "Kimi NativePath byte offset overflowed",
                    ))?;
            let line_number = usize::try_from(ordinal)
                .ok()
                .and_then(|value| value.checked_add(1))
                .ok_or(CaptureError::SystemInvariant(
                    "Kimi NativePath line number overflowed",
                ))?;
            let next_ordinal = ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                "Kimi NativePath ordinal overflowed",
            ))?;
            checkpoint.complete_offset = offset;
            checkpoint.next_ordinal = next_ordinal;
            checkpoint.committed_prefix_sha256 = prefix_digest(&hasher);
            checkpoint.observed_file_len = observation.wire().length;
            checkpoint.wire_revision = observation.wire().revision_component();
            checkpoint.terminal = false;
            checkpoint.retired = false;

            let (mut units, session_first_observed) = if raw.oversized {
                checkpoint.rejected_records = checkpoint.rejected_records.saturating_add(1);
                (
                    vec![KimiCoreUnit::Rejection {
                        line: line_number,
                        reason: format!(
                            "provider record exceeds the {MAX_PROVIDER_JSONL_LINE_BYTES} byte limit (observed {} bytes)",
                            raw.observed_bytes
                        ),
                    }],
                    false,
                )
            } else {
                project_core_record(
                    &observation,
                    &context,
                    &canonical_identity,
                    &content_revision,
                    ordinal,
                    line_number,
                    byte_start,
                    offset,
                    json_record_bytes(&raw.bytes),
                    &mut checkpoint,
                )?
            };
            let mut encoded_bytes = serde_json::to_vec(&units)?.len();
            if units.len() > KIMI_NATIVE_PAGE_MAX_UNITS
                || encoded_bytes.saturating_add(64 * 1024) > KIMI_NATIVE_PAGE_MAX_BYTES
            {
                checkpoint.rejected_records = checkpoint.rejected_records.saturating_add(1);
                units = vec![KimiCoreUnit::Rejection {
                    line: line_number,
                    reason: "Kimi normalized record exceeds the NativePath page bound".to_owned(),
                }];
                encoded_bytes = serde_json::to_vec(&units)?.len();
            }
            if !page.can_push(&units, encoded_bytes) && !page.is_empty() {
                let pending = page.take();
                let page_summary = publish_core_page(
                    store,
                    committed_store,
                    bulk_guard,
                    &canonical_path,
                    &observation,
                    &context,
                    &source_revision,
                    &stream,
                    &checkpoint_before,
                    options.history_record_id,
                    pending,
                )?;
                if page_summary.work_result() == ProviderImportWorkResult::Changed {
                    *changed_groups = changed_groups.saturating_add(1);
                }
                summary.merge_from(page_summary);
                if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
                    && *changed_groups != 0
                {
                    summary.work_remaining = true;
                    return Ok(summary);
                }
            }
            page.session_first_observed |= session_first_observed;
            page.push(units, encoded_bytes);
            ordinal = next_ordinal;
            if page.units.len() >= KIMI_NATIVE_PAGE_MAX_UNITS {
                let pending = page.take();
                let page_summary = publish_core_page(
                    store,
                    committed_store,
                    bulk_guard,
                    &canonical_path,
                    &observation,
                    &context,
                    &source_revision,
                    &stream,
                    &checkpoint,
                    options.history_record_id,
                    pending,
                )?;
                if page_summary.work_result() == ProviderImportWorkResult::Changed {
                    *changed_groups = changed_groups.saturating_add(1);
                }
                summary.merge_from(page_summary);
                if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
                    && *changed_groups != 0
                {
                    summary.work_remaining = true;
                    return Ok(summary);
                }
            }
        }
    }

    checkpoint.terminal = offset == observation.wire().length;
    checkpoint.observed_file_len = observation.wire().length;
    let final_page = page.take();
    let page_summary = publish_core_page(
        store,
        committed_store,
        bulk_guard,
        &canonical_path,
        &observation,
        &context,
        &source_revision,
        &stream,
        &checkpoint,
        options.history_record_id,
        final_page,
    )?;
    if page_summary.work_result() == ProviderImportWorkResult::Changed {
        *changed_groups = changed_groups.saturating_add(1);
    }
    summary.merge_from(page_summary);
    Ok(summary)
}

fn plan_core_scan(
    path: &Path,
    observation: &KimiWireObservation,
    route_sha256: [u8; 32],
    admission_scope_revision: String,
    committed: Option<&KimiCommittedSource>,
    source_revision: &str,
) -> Result<(KimiNativeCheckpoint, u64, u64, Sha256, bool)> {
    let Some(committed) = committed else {
        let checkpoint =
            KimiNativeCheckpoint::initial(route_sha256, observation, admission_scope_revision);
        return Ok((checkpoint, 0, 0, initial_prefix_hasher(), false));
    };
    let Some(previous) = committed.checkpoint.as_ref() else {
        let checkpoint =
            KimiNativeCheckpoint::initial(route_sha256, observation, admission_scope_revision);
        return Ok((checkpoint, 0, 0, initial_prefix_hasher(), false));
    };
    let physical = observation.wire().physical_identity();
    let identity_matches = previous.version == KIMI_NATIVE_CURSOR_VERSION
        && !previous.retired
        && previous.route_sha256 == route_sha256
        && previous.physical_device == physical.0
        && previous.physical_inode == physical.1
        && previous.auxiliary_revision == observation.session.auxiliary_revision
        && previous.admission_scope_revision == admission_scope_revision
        && previous.complete_offset <= observation.wire().length;
    if !identity_matches {
        let checkpoint =
            KimiNativeCheckpoint::initial(route_sha256, observation, admission_scope_revision);
        return Ok((checkpoint, 0, 0, initial_prefix_hasher(), false));
    }
    let Some(hasher) = verify_prefix(path, previous)? else {
        let checkpoint =
            KimiNativeCheckpoint::initial(route_sha256, observation, admission_scope_revision);
        return Ok((checkpoint, 0, 0, initial_prefix_hasher(), false));
    };
    let unchanged = previous.terminal
        && previous.complete_offset == observation.wire().length
        && previous.wire_revision == observation.wire().revision_component()
        && committed.source_revision == source_revision;
    Ok((
        previous.clone(),
        previous.complete_offset,
        previous.next_ordinal,
        hasher,
        unchanged,
    ))
}

fn verify_prefix(path: &Path, checkpoint: &KimiNativeCheckpoint) -> Result<Option<Sha256>> {
    let mut file = File::open(path)?;
    let mut hasher = initial_prefix_hasher();
    let mut remaining = checkpoint.complete_offset;
    let mut buffer = [0_u8; 64 * 1024];
    while remaining != 0 {
        let limit = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| CaptureError::SystemInvariant("Kimi prefix length overflowed"))?;
        let read = file.read(&mut buffer[..limit])?;
        if read == 0 {
            return Ok(None);
        }
        hasher.update(&buffer[..read]);
        remaining = remaining.saturating_sub(read as u64);
    }
    Ok((prefix_digest(&hasher) == checkpoint.committed_prefix_sha256).then_some(hasher))
}

// These inputs are the explicit identity, source range, and checkpoint for one wire record;
// bundling them would obscure the provider projection boundary without simplifying ownership.
#[allow(clippy::too_many_arguments)]
fn project_core_record(
    observation: &KimiWireObservation,
    context: &ProviderAdapterContext,
    canonical_identity: &str,
    content_revision: &str,
    ordinal: u64,
    line_number: usize,
    byte_start: u64,
    byte_end_exclusive: u64,
    bytes: &[u8],
    checkpoint: &mut KimiNativeCheckpoint,
) -> Result<(Vec<KimiCoreUnit>, bool)> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok((Vec::new(), false));
    }
    let value = match serde_json::from_slice::<Value>(bytes) {
        Ok(value) => value,
        Err(error) => {
            checkpoint.rejected_records = checkpoint.rejected_records.saturating_add(1);
            return Ok((
                vec![KimiCoreUnit::Rejection {
                    line: line_number,
                    reason: format!("malformed JSONL: {error}"),
                }],
                false,
            ));
        }
    };
    let record_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if checkpoint.started_at.is_none() {
        checkpoint.started_at = if record_type == "metadata" {
            value
                .get("created_at")
                .and_then(Value::as_i64)
                .and_then(DateTime::<Utc>::from_timestamp_millis)
        } else {
            kimi_record_timestamp(&value, context.imported_at)
        };
    }
    let session_first_observed = !checkpoint.emitted_session;
    checkpoint.emitted_session = true;
    if record_type == "metadata" {
        return Ok((Vec::new(), session_first_observed));
    }

    let occurred_at =
        kimi_record_timestamp(&value, checkpoint.started_at.unwrap_or(context.imported_at))
            .unwrap_or(context.imported_at);
    let path = observation.canonical_path();
    let event_type = kimi_event_type(record_type, &value);
    let mut event = kimi_event(line_number, &value, occurred_at, path);
    let mut units = Vec::new();
    if event_type == EventType::ToolOutput {
        let output = kimi_output_metadata(&value, line_number, observation.session.cwd.as_deref());
        let retained_failure = matches!(
            output.outcome.outcome,
            OutputOutcome::Failure | OutputOutcome::Timeout
        );
        let content = retained_failure
            .then(|| kimi_result_content(&value))
            .flatten()
            .unwrap_or_default();
        let touch_outcome =
            collect_output_touches(ordinal, line_number, occurred_at, &value, &mut units)?;
        checkpoint.accepted_file_touches = checkpoint
            .accepted_file_touches
            .saturating_add(touch_outcome as u64);
        if !retained_failure {
            return Ok((units, session_first_observed));
        }
        if output.kind == OutputObservationKind::Command {
            event.event_type = EventType::CommandOutput;
        }
        let (preview, _) = provider_local_preview(&content, PROVIDER_MAX_PREVIEW_CHARS);
        event.payload = json!({
            "result_outcome": "failure",
            "output_bytes": content.len(),
            "output_preview": preview,
            "call_id": output.call_id,
            "exit_code": output.outcome.exit_code,
            "duration_ms": output.outcome.duration_ms,
            "timed_out": output.outcome.outcome == OutputOutcome::Timeout,
            "tool": output.command.as_ref().map(|command| command.tool_name.clone()),
            "command": output.command.as_ref().map(|command| command.command.clone()),
            "cwd": output.command.as_ref().and_then(|command| command.working_directory.clone()),
        });
        checkpoint.accepted_events = checkpoint.accepted_events.saturating_add(1);
        units.insert(
            0,
            KimiCoreUnit::Event {
                raw_ordinal: ordinal,
                event,
            },
        );
        return Ok((units, session_first_observed));
    }

    attach_kimi_message_locator(
        &mut event,
        &value,
        bytes,
        line_number,
        byte_start,
        byte_end_exclusive,
        content_revision,
        canonical_identity,
    )?;
    let touch_outcome = kimi_file_touches(
        &value,
        event.event_type,
        event.occurred_at,
        Some(event.provider_event_index),
        event.provider_event_index << 16,
        event_type_supports_structured_file_touches(event.event_type),
    )?;
    if touch_outcome.limit_exceeded() {
        checkpoint.rejected_records = checkpoint.rejected_records.saturating_add(1);
        units.push(KimiCoreUnit::Rejection {
            line: line_number,
            reason: PROVIDER_FILE_TOUCH_LIMIT_REJECTION.to_owned(),
        });
    }
    checkpoint.accepted_events = checkpoint.accepted_events.saturating_add(1);
    checkpoint.accepted_file_touches = checkpoint
        .accepted_file_touches
        .saturating_add(touch_outcome.emitted() as u64);
    units.push(KimiCoreUnit::Event {
        raw_ordinal: ordinal,
        event,
    });
    units.extend(
        touch_outcome
            .touches
            .into_iter()
            .map(KimiCoreUnit::FileTouch),
    );
    Ok((units, session_first_observed))
}

fn collect_output_touches(
    ordinal: u64,
    line_number: usize,
    occurred_at: DateTime<Utc>,
    value: &Value,
    units: &mut Vec<KimiCoreUnit>,
) -> Result<usize> {
    let outcome = kimi_file_touches(
        value,
        EventType::ToolOutput,
        occurred_at,
        Some(ordinal),
        ordinal << 16,
        false,
    )?;
    if outcome.limit_exceeded() {
        units.push(KimiCoreUnit::Rejection {
            line: line_number,
            reason: PROVIDER_FILE_TOUCH_LIMIT_REJECTION.to_owned(),
        });
    }
    let emitted = outcome.emitted();
    units.extend(outcome.touches.into_iter().map(KimiCoreUnit::FileTouch));
    Ok(emitted)
}

#[allow(clippy::too_many_arguments)]
fn publish_core_page(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    path: &Path,
    observation: &KimiWireObservation,
    context: &ProviderAdapterContext,
    source_revision: &str,
    stream: &str,
    checkpoint: &KimiNativeCheckpoint,
    history_record_id: Option<Uuid>,
    page: KimiCorePage,
) -> Result<ProviderImportSummary> {
    if !observation.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let current = store.get_sync_cursor(None, &context.machine_id, stream)?;
    let next = kimi_sync_cursor(
        &context.machine_id,
        stream.to_owned(),
        source_revision,
        checkpoint,
        context.imported_at,
    )?;
    let transition =
        NativePathCursorTransition::new(current.as_ref().map(|cursor| cursor.cursor.clone()), next);
    let publication_id = core_publication_id(path, &transition, checkpoint);
    let retained_bytes = serde_json::to_vec(&(checkpoint, &page))?
        .len()
        .saturating_add(64 * 1024);
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(
        admission,
        NativePathGroupAccounting::new(1, 1, retained_bytes)?,
    )?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        let mut summary = replay_page_summary(&page);
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }

    let raw_source_path = path.display().to_string();
    let source_root = context
        .source_root_display()
        .unwrap_or_else(|| raw_source_path.clone());
    let locator_identity = provider_path_identity(path)?;
    let proposed_source_identity = provider_source_identity(
        CaptureProvider::KimiCodeCli,
        KIMI_CODE_CLI_SOURCE_FORMAT,
        Some(&source_root),
        Some(&raw_source_path),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Kimi NativePath source has no canonical identity",
    ))?;
    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::KimiCodeCli,
            source_format: KIMI_CODE_CLI_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity,
            cursor_stream: stream.to_owned(),
            proposed_source_identity,
            raw_source_path: Some(raw_source_path.clone()),
            source_revision: source_revision.to_owned(),
            observed_at_ms: context.imported_at.timestamp_millis(),
        })?;
    if !checkpoint.emitted_session {
        let mut summary = ProviderImportSummary::default();
        for unit in &page.units {
            if let KimiCoreUnit::Rejection { line, reason } = unit {
                summary.record_failure(ProviderImportFailure {
                    line: *line,
                    error: reason.clone(),
                });
            }
        }
        if !observation.revalidate(path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        group.prepare_journal_checkpoint()?;
        group.publish_cursor_set()?;
        group.commit()?;
        summary.set_work_result(ProviderImportWorkResult::Changed);
        return Ok(summary);
    }
    let provider_session_id = &observation.session.provider_session_id;
    let source_id = committed_store
        .capture_source_by_canonical_identity_session(
            CaptureProvider::KimiCodeCli,
            KIMI_CODE_CLI_SOURCE_FORMAT,
            &context.machine_id,
            &resolution.canonical_source_identity,
            provider_session_id,
        )?
        .map(|source| source.id)
        .unwrap_or_else(|| {
            native_source_id(&resolution.canonical_source_identity, provider_session_id)
        });
    group.upsert_capture_source(&kimi_capture_source(
        context,
        &observation.session,
        checkpoint,
        source_id,
        &raw_source_path,
        &source_root,
        &resolution.canonical_source_identity,
        source_revision,
    ))?;
    group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;

    let mut summary = ProviderImportSummary::default();
    let session = canonical_kimi_session(
        committed_store,
        context,
        &observation.session,
        checkpoint,
        history_record_id,
        source_id,
        &resolution.canonical_source_identity,
    )?;
    for (id, external_session_id) in relationship_placeholders(&session, &observation.session) {
        if committed_store.get_session(id).is_err() {
            group.upsert_session(&relationship_placeholder(
                context,
                source_id,
                id,
                external_session_id,
                history_record_id,
                &resolution.canonical_source_identity,
            ))?;
            summary.imported_sessions = summary.imported_sessions.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        }
    }
    let session_existed = committed_store.get_session(session.id).is_ok();
    group.upsert_session(&session)?;
    if session_existed {
        summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    } else {
        summary.imported_sessions = summary.imported_sessions.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    }
    if let Some(parent_id) = session.parent_session_id {
        let edge = relationship_edge(
            context,
            source_id,
            &session,
            parent_id,
            &resolution.canonical_source_identity,
        );
        let edge_existed = committed_store.session_edge_exists(edge.id)?;
        group.upsert_projection_neutral_session_edge(&actor(&session), &edge)?;
        if edge_existed {
            summary.skipped_edges = summary.skipped_edges.saturating_add(1);
        } else {
            summary.imported_edges = summary.imported_edges.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        }
    }

    let mut event_ids = BTreeMap::<u64, Uuid>::new();
    for unit in &page.units {
        match unit {
            KimiCoreUnit::Event { raw_ordinal, event } => {
                let event_id = publish_kimi_event(
                    &mut group,
                    committed_store,
                    context,
                    source_id,
                    &session,
                    history_record_id,
                    *raw_ordinal,
                    event,
                    &mut summary,
                )?;
                event_ids.insert(event.provider_event_index, event_id);
            }
            KimiCoreUnit::FileTouch(touch) => {
                publish_kimi_file_touch(
                    &mut group,
                    committed_store,
                    context,
                    source_id,
                    &session,
                    history_record_id,
                    touch,
                    touch
                        .provider_event_index
                        .and_then(|index| event_ids.get(&index).copied()),
                )?;
                summary.accepted_content_records =
                    summary.accepted_content_records.saturating_add(1);
            }
            KimiCoreUnit::Rejection { line, reason } => {
                summary.record_failure(ProviderImportFailure {
                    line: *line,
                    error: reason.clone(),
                });
            }
        }
    }
    if !observation.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

fn kimi_sync_cursor(
    machine_id: &str,
    stream: String,
    source_revision: &str,
    checkpoint: &KimiNativeCheckpoint,
    observed_at: DateTime<Utc>,
) -> Result<SyncCursor> {
    let position = NativePosition::new(
        KIMI_NATIVE_POSITION_KIND,
        serde_json::to_vec(&checkpoint.frontier())?,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let cursor = CertifiedProviderCursor::new(
        source_revision,
        KIMI_NATIVE_CAPTURE_REVISION,
        KIMI_NATIVE_POLICY_REVISION,
        position,
        BoundedParserCheckpoint::from_serializable(checkpoint)?,
    )?
    .with_rejected_records(checkpoint.rejected_records);
    certified_provider_sync_cursor(
        CaptureProvider::KimiCodeCli,
        machine_id,
        stream,
        &cursor,
        observed_at,
    )
}

fn core_publication_id(
    path: &Path,
    transition: &NativePathCursorTransition,
    checkpoint: &KimiNativeCheckpoint,
) -> String {
    let mut digest = Sha256::new();
    digest.update(KIMI_PUBLICATION_DOMAIN);
    digest.update(path.as_os_str().as_encoded_bytes());
    digest.update(transition.key().stream().as_bytes());
    if let Some(expected) = transition.expected_cursor() {
        digest.update((expected.len() as u64).to_be_bytes());
        digest.update(expected.as_bytes());
    } else {
        digest.update(0_u64.to_be_bytes());
    }
    digest.update((transition.next().cursor.len() as u64).to_be_bytes());
    digest.update(transition.next().cursor.as_bytes());
    digest.update(checkpoint.complete_offset.to_be_bytes());
    format!("kimi-nativepath-v1:{:x}", digest.finalize())
}

fn native_source_id(source_identity: &str, provider_session_id: &str) -> Uuid {
    stable_capture_uuid(
        &serde_json::to_string(&(
            "native-path-provider-source-v1",
            CaptureProvider::KimiCodeCli.as_str(),
            KIMI_CODE_CLI_SOURCE_FORMAT,
            source_identity,
            provider_session_id,
        ))
        .expect("Kimi native source identity is serializable"),
        "source",
    )
}

#[allow(clippy::too_many_arguments)]
fn kimi_capture_source(
    context: &ProviderAdapterContext,
    session: &KimiWireSessionState,
    checkpoint: &KimiNativeCheckpoint,
    source_id: Uuid,
    raw_source_path: &str,
    source_root: &str,
    source_identity: &str,
    source_revision: &str,
) -> CaptureSource {
    CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::KimiCodeCli,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: session.cwd.clone(),
            raw_source_path: Some(raw_source_path.to_owned()),
            source_format: Some(KIMI_CODE_CLI_SOURCE_FORMAT.to_owned()),
            source_root: Some(source_root.to_owned()),
            source_identity: Some(source_identity.to_owned()),
            external_session_id: Some(session.provider_session_id.clone()),
        },
        started_at: checkpoint
            .started_at
            .or(session.started_at)
            .unwrap_or(context.imported_at),
        ended_at: session.ended_at,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": session.provider_session_id,
                "source_format": KIMI_CODE_CLI_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "source_identity": source_identity,
                "source_root": source_root,
                "source_revision": source_revision,
                "source_identity_key": provider_scoped_source_identity_key(
                    CaptureProvider::KimiCodeCli,
                    &session.provider_session_id,
                    KIMI_CODE_CLI_SOURCE_FORMAT,
                    Some(raw_source_path),
                ),
                "session_index": session.index_metadata,
            }),
        ),
    }
}

fn canonical_kimi_session(
    committed_store: &Store,
    context: &ProviderAdapterContext,
    session: &KimiWireSessionState,
    checkpoint: &KimiNativeCheckpoint,
    history_record_id: Option<Uuid>,
    source_id: Uuid,
    source_identity: &str,
) -> Result<Session> {
    let id = provider_import_session_uuid(
        committed_store,
        CaptureProvider::KimiCodeCli,
        &session.provider_session_id,
        source_id,
        Some(source_identity),
    )?;
    let parent_session_id = session
        .parent_provider_session_id
        .as_deref()
        .map(|parent| {
            provider_import_session_uuid(
                committed_store,
                CaptureProvider::KimiCodeCli,
                parent,
                source_id,
                Some(source_identity),
            )
        })
        .transpose()?;
    let root_session_id = session
        .root_provider_session_id
        .as_deref()
        .map(|root| {
            provider_import_session_uuid(
                committed_store,
                CaptureProvider::KimiCodeCli,
                root,
                source_id,
                Some(source_identity),
            )
        })
        .transpose()?
        .or(parent_session_id);
    Ok(Session {
        id,
        history_record_id,
        parent_session_id,
        root_session_id,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::KimiCodeCli,
        external_session_id: Some(session.provider_session_id.clone()),
        external_agent_id: Some(session.agent_id.clone()),
        agent_type: if session.is_primary {
            AgentType::Primary
        } else {
            AgentType::Subagent
        },
        role_hint: Some(if session.is_primary {
            "main".to_owned()
        } else {
            "subagent".to_owned()
        }),
        is_primary: session.is_primary,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: checkpoint
            .started_at
            .or(session.started_at)
            .unwrap_or(context.imported_at),
        ended_at: session.ended_at,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": session.provider_session_id,
                "parent_provider_session_id": session.parent_provider_session_id,
                "root_provider_session_id": session.root_provider_session_id,
                "source_format": KIMI_CODE_CLI_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "metadata": {
                    "agent_id": session.agent_id,
                    "state": session.state_metadata,
                    "agent_state": session.agent_state_metadata,
                    "title": session.title,
                    "last_prompt": session.last_prompt,
                    "archived": session.archived,
                },
            }),
        ),
    })
}

fn relationship_placeholders<'a>(
    canonical: &Session,
    native: &'a KimiWireSessionState,
) -> Vec<(Uuid, &'a str)> {
    let mut placeholders = Vec::new();
    if let (Some(id), Some(external)) = (
        canonical.parent_session_id,
        native.parent_provider_session_id.as_deref(),
    ) {
        placeholders.push((id, external));
    }
    if let (Some(id), Some(external)) = (
        canonical.root_session_id,
        native.root_provider_session_id.as_deref(),
    ) {
        if !placeholders.iter().any(|(existing, _)| *existing == id) {
            placeholders.push((id, external));
        }
    }
    placeholders
}

fn relationship_placeholder(
    context: &ProviderAdapterContext,
    source_id: Uuid,
    id: Uuid,
    external_session_id: &str,
    history_record_id: Option<Uuid>,
    source_identity: &str,
) -> Session {
    Session {
        id,
        history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::KimiCodeCli,
        external_session_id: Some(external_session_id.to_owned()),
        external_agent_id: None,
        agent_type: AgentType::Unknown,
        role_hint: Some("relationship_placeholder".to_owned()),
        is_primary: false,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: context.imported_at,
        ended_at: None,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Partial,
            json!({
                "provider_session_id": external_session_id,
                "source_format": KIMI_CODE_CLI_SOURCE_FORMAT,
                "source_identity": source_identity,
                "relationship_placeholder": true,
            }),
        ),
    }
}

fn relationship_edge(
    context: &ProviderAdapterContext,
    source_id: Uuid,
    session: &Session,
    parent_id: Uuid,
    source_identity: &str,
) -> SessionEdge {
    SessionEdge {
        id: stable_capture_uuid(
            &format!(
                "provider-source-root:{source_identity}:session:{}:parent_child",
                session.external_session_id.as_deref().unwrap_or_default()
            ),
            "session-edge",
        ),
        from_session_id: session.id,
        to_session_id: parent_id,
        edge_type: SessionEdgeType::ParentChild,
        confidence: Confidence::Explicit,
        source_id: Some(source_id),
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": session.external_session_id,
                "source_format": KIMI_CODE_CLI_SOURCE_FORMAT,
                "imported_at": context.imported_at,
            }),
        ),
    }
}

fn actor(session: &Session) -> CanonicalActor {
    CanonicalActor {
        direct_session_id: session.id,
        root_session_id: session.root_session_id.unwrap_or(session.id),
        parent_session_id: session.parent_session_id,
        external_session_id: session.external_session_id.clone(),
        external_agent_id: session.external_agent_id.clone(),
        agent_type: session.agent_type.as_str().to_owned(),
        role_hint: session.role_hint.clone(),
        is_primary: session.is_primary,
    }
}

#[allow(clippy::too_many_arguments)]
fn publish_kimi_event(
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    committed_store: &Store,
    context: &ProviderAdapterContext,
    source_id: Uuid,
    session: &Session,
    history_record_id: Option<Uuid>,
    raw_ordinal: u64,
    event: &KimiCoreEvent,
    summary: &mut ProviderImportSummary,
) -> Result<Uuid> {
    let fallback_hash = format!(
        "normalized-sha256-v1:{:x}",
        Sha256::digest(serde_json::to_vec(&event.payload)?)
    );
    let (event_hash, authority) = event.provider_event_hash.as_deref().map_or(
        (
            fallback_hash.as_str(),
            ProviderEventHashAuthority::NormalizedPayloadFallback,
        ),
        |hash| (hash, ProviderEventHashAuthority::ProviderSupplied),
    );
    let provider_session_id = session.external_session_id.as_deref().unwrap_or_default();
    let allow_legacy_provider_identity =
        session.id == provider_session_uuid(CaptureProvider::KimiCodeCli, provider_session_id);
    let identity = provider_event_import_identity_with_exact_legacy_source(
        committed_store,
        CaptureProvider::KimiCodeCli,
        provider_session_id,
        source_id,
        event.provider_event_index,
        raw_ordinal,
        event_hash,
        None,
        Some(raw_ordinal),
        allow_legacy_provider_identity,
    )?;
    let dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, event_hash)
            .unwrap_or(identity.dedupe_key);
    let run = kimi_command_run(
        source_id,
        session,
        history_record_id,
        event,
        event_hash,
        identity.run_source_id,
    )?;
    if let Some(run) = &run {
        group.upsert_run(run)?;
    }
    let mut provider_metadata = event.metadata.clone();
    let verified_locators = provider_metadata
        .as_object_mut()
        .and_then(|metadata| metadata.remove(VERIFIED_CONTENT_LOCATORS_METADATA_KEY));
    let mut sync_metadata = json!({
        "provider_session_id": provider_session_id,
        "provider_event_index": event.provider_event_index,
        "provider_event_hash": event_hash,
        "provider_event_hash_authority": authority.as_str(),
        "cursor": event.cursor,
        "source_format": KIMI_CODE_CLI_SOURCE_FORMAT,
        "source_trust": "provider_native",
        "fixture_line": raw_ordinal.saturating_add(1),
        "source_record_ordinal": raw_ordinal,
        "source_record_subrecord_index": 0,
        "imported_at": context.imported_at,
        "metadata": provider_metadata,
    });
    if let (Some(metadata), Some(locators)) = (sync_metadata.as_object_mut(), verified_locators) {
        metadata.insert(VERIFIED_CONTENT_LOCATORS_METADATA_KEY.to_owned(), locators);
    }
    let canonical = Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id,
        session_id: Some(session.id),
        run_id: run.as_ref().map(|run| run.id),
        event_type: event.event_type,
        role: event.role,
        occurred_at: event.occurred_at,
        capture_source_id: Some(source_id),
        payload: json!({
            "provider": CaptureProvider::KimiCodeCli.as_str(),
            "provider_session_id": provider_session_id,
            "provider_event_index": event.provider_event_index,
            "provider_event_hash": event_hash,
            "cursor": event.cursor,
            "artifacts": [],
            "body": compact_provider_result_payload(event.event_type, &event.payload),
        }),
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(event.fidelity, sync_metadata),
    };
    let inserted = group.reconcile_provider_event(&canonical, authority)?;
    if inserted {
        summary.imported_events = summary.imported_events.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    } else {
        summary.skipped_events = summary.skipped_events.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    }
    summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
    Ok(canonical.id)
}

#[allow(clippy::too_many_arguments)]
fn publish_kimi_file_touch(
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    committed_store: &Store,
    context: &ProviderAdapterContext,
    source_id: Uuid,
    session: &Session,
    history_record_id: Option<Uuid>,
    touch: &KimiFileTouch,
    event_id: Option<Uuid>,
) -> Result<()> {
    let provider_session_id = session.external_session_id.as_deref().unwrap_or_default();
    let allow_legacy_provider_identity =
        session.id == provider_session_uuid(CaptureProvider::KimiCodeCli, provider_session_id);
    let id = provider_file_touch_import_id(
        committed_store,
        CaptureProvider::KimiCodeCli,
        provider_session_id,
        source_id,
        touch.provider_event_index,
        touch.provider_touch_index,
        allow_legacy_provider_identity,
    )?;
    group.upsert_file_touched(&FileTouched {
        id,
        history_record_id,
        run_id: None,
        event_id,
        vcs_workspace_id: None,
        path: touch.path.clone(),
        change_kind: touch.change_kind,
        old_path: touch.old_path.clone(),
        line_count_delta: None,
        confidence: touch.confidence,
        timestamps: timestamps(touch.occurred_at),
        source_id: Some(source_id),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider": CaptureProvider::KimiCodeCli.as_str(),
                "provider_session_id": provider_session_id,
                "provider_touch_index": touch.provider_touch_index,
                "provider_event_index": touch.provider_event_index,
                "source_format": KIMI_CODE_CLI_SOURCE_FORMAT,
                "session_id": session.id,
                "imported_at": context.imported_at,
                "metadata": touch.metadata,
            }),
        ),
    })?;
    Ok(())
}

fn kimi_command_run(
    source_id: Uuid,
    session: &Session,
    history_record_id: Option<Uuid>,
    event: &KimiCoreEvent,
    event_hash: &str,
    run_source_id: Option<Uuid>,
) -> Result<Option<Run>> {
    if event.event_type != EventType::CommandOutput {
        return Ok(None);
    }
    let provider_session_id = session.external_session_id.as_deref().unwrap_or_default();
    let call_id = event.payload.get("call_id").and_then(Value::as_str);
    let run_key = call_id.unwrap_or(event_hash);
    let id = run_source_id.map_or_else(
        || {
            stable_capture_uuid(
                &format!(
                    "provider:{}:{provider_session_id}:run:{run_key}",
                    CaptureProvider::KimiCodeCli.as_str()
                ),
                "run",
            )
        },
        |run_source_id| {
            stable_capture_uuid(
                &format!("provider-source:{run_source_id}:run:{run_key}"),
                "run",
            )
        },
    );
    let started_at = kimi_command_started_at(event)?;
    Ok(Some(Run {
        id,
        history_record_id,
        session_id: Some(session.id),
        run_type: RunType::Command,
        status: kimi_command_run_status(&event.payload),
        started_at,
        ended_at: Some(event.occurred_at),
        exit_code: event
            .payload
            .get("exit_code")
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok()),
        cwd: event
            .payload
            .get("workdir")
            .or_else(|| event.payload.get("cwd"))
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned),
        command_preview: event
            .payload
            .get("command")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned),
        input_blob_id: None,
        output_blob_id: None,
        timestamps: timestamps(event.occurred_at),
        source_id: Some(source_id),
        sync: provider_sync_metadata(
            event.fidelity,
            json!({
                "provider_session_id": provider_session_id,
                "provider_event_index": event.provider_event_index,
                "provider_event_hash": event_hash,
                "call_id": call_id,
                "source": "provider_command_output",
            }),
        ),
    }))
}

fn kimi_command_started_at(event: &KimiCoreEvent) -> Result<DateTime<Utc>> {
    let Some(value) = event.payload.get("duration_ms") else {
        return Ok(event.occurred_at);
    };
    if value.is_null() {
        return Ok(event.occurred_at);
    }
    let duration_ms = value
        .as_i64()
        .ok_or_else(|| CaptureError::InvalidPayload("duration_ms must be an integer".to_owned()))?;
    if duration_ms < 0 {
        return Err(CaptureError::InvalidPayload(format!(
            "duration_ms must be nonnegative, got {duration_ms}"
        )));
    }
    let duration = chrono::Duration::try_milliseconds(duration_ms).ok_or_else(|| {
        CaptureError::InvalidPayload(format!(
            "duration_ms is not representable as milliseconds: {duration_ms}"
        ))
    })?;
    event
        .occurred_at
        .checked_sub_signed(duration)
        .ok_or_else(|| {
            CaptureError::InvalidPayload(format!(
                "duration_ms moves command start before representable time: {duration_ms}"
            ))
        })
}

fn kimi_command_run_status(payload: &Value) -> RunStatus {
    if payload
        .get("timed_out")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return RunStatus::Cancelled;
    }
    match payload.get("exit_code").and_then(Value::as_i64) {
        Some(0) => RunStatus::Succeeded,
        Some(_) => RunStatus::Failed,
        None => match payload
            .get("result_outcome")
            .or_else(|| payload.get("outcome"))
            .or_else(|| payload.get("status"))
            .and_then(Value::as_str)
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("timeout" | "timed_out" | "timedout" | "cancelled" | "canceled") => {
                RunStatus::Cancelled
            }
            Some("failure" | "failed" | "error" | "errored") => RunStatus::Failed,
            Some("success" | "succeeded" | "complete" | "completed" | "ok" | "passed") => {
                RunStatus::Succeeded
            }
            _ => RunStatus::Partial,
        },
    }
}

fn replay_page_summary(page: &KimiCorePage) -> ProviderImportSummary {
    let mut summary = ProviderImportSummary {
        skipped_sessions: usize::from(page.session_first_observed),
        ..ProviderImportSummary::default()
    };
    let mut skipped_file_touches = 0_usize;
    for unit in &page.units {
        match unit {
            KimiCoreUnit::Event { .. } => {
                summary.skipped_events = summary.skipped_events.saturating_add(1);
                summary.accepted_content_records =
                    summary.accepted_content_records.saturating_add(1);
            }
            KimiCoreUnit::FileTouch(_) => {
                skipped_file_touches = skipped_file_touches.saturating_add(1);
                summary.accepted_content_records =
                    summary.accepted_content_records.saturating_add(1);
            }
            KimiCoreUnit::Rejection { line, reason } => {
                summary.record_failure(ProviderImportFailure {
                    line: *line,
                    error: reason.clone(),
                });
            }
        }
    }
    summary.skipped = summary
        .skipped_sessions
        .saturating_add(summary.skipped_events)
        .saturating_add(skipped_file_touches);
    summary
}

fn replay_summary(checkpoint: &KimiNativeCheckpoint) -> ProviderImportSummary {
    let skipped_sessions = usize::from(checkpoint.emitted_session);
    let skipped_events = usize::try_from(checkpoint.accepted_events).unwrap_or(usize::MAX);
    let skipped_file_touches =
        usize::try_from(checkpoint.accepted_file_touches).unwrap_or(usize::MAX);
    ProviderImportSummary {
        skipped: skipped_sessions
            .saturating_add(skipped_events)
            .saturating_add(skipped_file_touches),
        skipped_sessions,
        skipped_events,
        accepted_content_records: skipped_events.saturating_add(skipped_file_touches),
        ..ProviderImportSummary::default()
    }
}

fn load_committed_source(
    store: &Store,
    machine_id: &str,
    stream: &str,
) -> Result<Option<KimiCommittedSource>> {
    let Some(raw) = store.get_sync_cursor(None, machine_id, stream)? else {
        return Ok(None);
    };
    let Ok(committed) = decode_native_path_committed_cursor(&raw.cursor) else {
        // Released pre-NativePath cursors are intentionally migration-only:
        // retain their CAS authority, but rebuild provider-owned state from byte zero.
        return Ok(Some(KimiCommittedSource {
            checkpoint: None,
            source_revision: String::new(),
        }));
    };
    let certified = CertifiedProviderCursor::decode(committed.provider_cursor())?;
    let source_revision = certified.source_revision().to_owned();
    let checkpoint = if certified.parser_revision() == KIMI_NATIVE_CAPTURE_REVISION
        && certified.policy_revision() == KIMI_NATIVE_POLICY_REVISION
        && certified.native_position().kind() == KIMI_NATIVE_POSITION_KIND
    {
        let checkpoint = certified
            .parser_checkpoint()
            .deserialize::<KimiNativeCheckpoint>()?;
        let frontier =
            serde_json::from_slice::<KimiNativeFrontier>(certified.native_position().value())?;
        (checkpoint.version == KIMI_NATIVE_CURSOR_VERSION
            && checkpoint.frontier() == frontier
            && checkpoint.rejected_records == certified.rejected_records())
        .then_some(checkpoint)
    } else {
        None
    };
    Ok(Some(KimiCommittedSource {
        checkpoint,
        source_revision,
    }))
}

fn known_kimi_routes(
    store: &Store,
    machine_id: &str,
    source_root: &Path,
) -> Result<Vec<KnownKimiRoute>> {
    let source_root = source_root.display().to_string();
    let mut routes = BTreeMap::<String, KnownKimiRoute>::new();
    for source in store.list_capture_sources()? {
        if source.descriptor.provider != CaptureProvider::KimiCodeCli
            || source.descriptor.machine_id != machine_id
            || source.descriptor.source_format.as_deref() != Some(KIMI_CODE_CLI_SOURCE_FORMAT)
            || source.descriptor.source_root.as_deref() != Some(source_root.as_str())
        {
            continue;
        }
        let (Some(raw_source_path), Some(canonical_source_identity)) = (
            source.descriptor.raw_source_path.as_deref(),
            source.descriptor.source_identity.as_deref(),
        ) else {
            continue;
        };
        let path = PathBuf::from(raw_source_path);
        let locator_identity = provider_path_identity(&path)?;
        let stream = provider_source_cursor_stream_for_path(
            CaptureProvider::KimiCodeCli,
            KIMI_CODE_CLI_SOURCE_FORMAT,
            &locator_identity,
        );
        let Some(current_cursor) = store.get_sync_cursor(None, machine_id, &stream)? else {
            continue;
        };
        let committed = load_committed_source(store, machine_id, &stream)?;
        let checkpoint = committed.and_then(|committed| committed.checkpoint);
        let Some(source_revision) = source
            .sync
            .metadata
            .get("source_revision")
            .and_then(Value::as_str)
        else {
            continue;
        };
        let route = KnownKimiRoute {
            path,
            locator_identity: locator_identity.clone(),
            canonical_source_identity: canonical_source_identity.to_owned(),
            source_revision: source_revision.to_owned(),
            current_cursor,
            checkpoint,
        };
        if routes.insert(locator_identity, route).is_some() {
            return Err(CaptureError::SystemInvariant(
                "Kimi persisted duplicate current routes for one transcript",
            ));
        }
    }
    Ok(routes.into_values().collect())
}

fn retire_missing_routes(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    machine_id: &str,
    retired_at: DateTime<Utc>,
    known_routes: &[KnownKimiRoute],
    live_paths: &BTreeSet<PathBuf>,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<ProviderImportSummary> {
    let mut summary = ProviderImportSummary::default();
    for route in known_routes
        .iter()
        .filter(|route| !live_paths.contains(&route.path))
    {
        if retire_kimi_route(store, bulk_guard, machine_id, retired_at, route, reason)? {
            summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
            summary.set_work_result(ProviderImportWorkResult::Changed);
        }
    }
    Ok(summary)
}

fn retire_kimi_route(
    store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    machine_id: &str,
    retired_at: DateTime<Utc>,
    route: &KnownKimiRoute,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<bool> {
    let mut checkpoint = route.checkpoint.clone().unwrap_or(KimiNativeCheckpoint {
        version: KIMI_NATIVE_CURSOR_VERSION,
        route_sha256: route_sha256(&route.locator_identity),
        physical_device: None,
        physical_inode: None,
        observed_file_len: 0,
        wire_revision: String::new(),
        auxiliary_revision: 0,
        admission_scope_revision: String::new(),
        complete_offset: 0,
        next_ordinal: 0,
        committed_prefix_sha256: initial_prefix_sha256(),
        started_at: None,
        emitted_session: false,
        accepted_events: 0,
        accepted_file_touches: 0,
        rejected_records: 0,
        terminal: true,
        retired: true,
    });
    checkpoint.terminal = true;
    checkpoint.retired = true;
    let stream = route.current_cursor.stream.clone();
    let transition = NativePathCursorTransition::new(
        Some(route.current_cursor.cursor.clone()),
        kimi_sync_cursor(
            machine_id,
            stream.clone(),
            &route.source_revision,
            &checkpoint,
            retired_at,
        )?,
    );
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::KimiCodeCli,
        source_format: KIMI_CODE_CLI_SOURCE_FORMAT.to_owned(),
        machine_id: machine_id.to_owned(),
        locator_identity: route.locator_identity.clone(),
        cursor_stream: stream,
        expected_canonical_source_identity: route.canonical_source_identity.clone(),
        expected_source_revision: route.source_revision.clone(),
        retired_at_ms: retired_at.timestamp_millis(),
        reason,
    };
    let publication_id = retirement_publication_id(&retirement);
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store
        .begin_native_path_publication_group(admission, NativePathGroupAccounting::new(0, 1, 0)?)?;
    let changed =
        match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
            NativePathCursorSetClassification::AllExpected => {
                let disposition = group.retire_provider_source_route(&retirement)?;
                group.prepare_journal_checkpoint()?;
                group.publish_cursor_set()?;
                matches!(
                    disposition,
                    ProviderSourceRouteRetirementDisposition::Retired
                )
            }
            NativePathCursorSetClassification::AllNextSameGroup { .. } => false,
        };
    group.commit()?;
    Ok(changed)
}

fn retirement_publication_id(retirement: &ProviderSourceRouteRetirement) -> String {
    let mut digest = Sha256::new();
    digest.update(KIMI_RETIREMENT_DOMAIN);
    digest.update(retirement.provider.as_str().as_bytes());
    digest.update(retirement.source_format.as_bytes());
    digest.update(retirement.machine_id.as_bytes());
    digest.update(retirement.locator_identity.as_bytes());
    digest.update(retirement.cursor_stream.as_bytes());
    digest.update(retirement.expected_canonical_source_identity.as_bytes());
    digest.update(retirement.expected_source_revision.as_bytes());
    digest.update(format!("{:?}", retirement.reason).as_bytes());
    format!("kimi-nativepath-retirement-v1:{:x}", digest.finalize())
}

fn discover_kimi_wire_files(root: &Path) -> Result<KimiInventory> {
    let metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(KimiInventory {
                paths: BTreeSet::new(),
                root_missing: true,
            });
        }
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: root.to_path_buf(),
            reason: "Kimi transcript roots must not be symbolic links",
        });
    }
    if metadata.is_file() {
        if !kimi_wire_file_is_selected(root) {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: root.to_path_buf(),
                reason: "expected a Kimi Code CLI wire.jsonl transcript",
            });
        }
        return Ok(KimiInventory {
            paths: BTreeSet::from([fs::canonicalize(root)?]),
            root_missing: false,
        });
    }
    if !metadata.is_dir() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: root.to_path_buf(),
            reason: "Kimi transcript root is neither a file nor directory",
        });
    }
    let mut paths = BTreeSet::new();
    let mut entries = 0_usize;
    discover_kimi_directory(root, 0, &mut entries, &mut paths)?;
    Ok(KimiInventory {
        paths,
        root_missing: false,
    })
}

fn discover_kimi_directory(
    directory: &Path,
    depth: usize,
    entries: &mut usize,
    paths: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    if depth > KIMI_NATIVE_DISCOVERY_MAX_DEPTH {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: directory.to_path_buf(),
            reason: "Kimi transcript tree exceeds the discovery depth bound",
        });
    }
    let mut children = fs::read_dir(directory)?.collect::<std::result::Result<Vec<_>, _>>()?;
    children.sort_by_key(fs::DirEntry::file_name);
    for child in children {
        *entries = entries.saturating_add(1);
        if *entries > KIMI_NATIVE_DISCOVERY_MAX_ENTRIES {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: directory.to_path_buf(),
                reason: "Kimi transcript tree exceeds the discovery entry bound",
            });
        }
        let path = child.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            discover_kimi_directory(&path, depth.saturating_add(1), entries, paths)?;
        } else if metadata.is_file() && kimi_wire_file_is_selected(&path) {
            paths.insert(fs::canonicalize(path)?);
        }
    }
    Ok(())
}

fn kimi_wire_file_is_selected(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "wire.jsonl")
}

#[allow(clippy::too_many_arguments)]
fn attach_kimi_message_locator(
    event: &mut KimiCoreEvent,
    value: &Value,
    record_bytes: &[u8],
    line_number: usize,
    byte_start: u64,
    byte_end_exclusive: u64,
    source_revision: &str,
    path_identity: &str,
) -> Result<()> {
    if event.event_type != EventType::Message {
        return Ok(());
    }
    let record_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let text = kimi_event_text(record_type, value, event.event_type);
    if text.chars().count() <= PROVIDER_MAX_TEXT_CHARS
        || text.len() > COMPLETE_CONTENT_MAX_BODY_BYTES
    {
        return Ok(());
    }
    let Some(content_ref) = ContentRef::from_bytes(text.as_bytes()) else {
        return Ok(());
    };
    let Some(profile) = verified_content_profile(
        CaptureProvider::KimiCodeCli,
        KIMI_CODE_CLI_SOURCE_FORMAT,
        CompleteContentSourceFamily::Jsonl,
        VerifiedContentRole::MessageBody,
    ) else {
        return Err(CaptureError::SystemInvariant(
            "supported Kimi complete-content route has no verified profile",
        ));
    };
    let mut encoded = Vec::with_capacity(80);
    encoded.extend_from_slice(&byte_start.to_be_bytes());
    encoded.extend_from_slice(&byte_end_exclusive.to_be_bytes());
    encoded.extend_from_slice(&domain_digest(
        SOURCE_REVISION_DIGEST_DOMAIN,
        source_revision,
    ));
    encoded.extend_from_slice(&domain_digest(PATH_IDENTITY_DIGEST_DOMAIN, path_identity));
    let native_record_id = event
        .provider_event_hash
        .clone()
        .unwrap_or_else(|| format!("line-{line_number}"));
    let Some(locator) = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Jsonl,
        KIMI_JSONL_LOCATOR_KIND,
        &encoded,
        native_record_id,
        CompleteContentBodyDigest::from_bytes(record_bytes),
    ) else {
        return Ok(());
    };
    attach_verified_content_locator(&mut event.metadata, locator).ok_or(
        CaptureError::SystemInvariant("Kimi verified-content locator collection is malformed"),
    )?;
    Ok(())
}

fn read_bounded_line(
    reader: &mut BufReader<File>,
    hasher: &mut Sha256,
    max_bytes: usize,
) -> Result<RawLine> {
    let mut bytes = Vec::new();
    let mut observed_bytes = 0_u64;
    let mut terminated = false;
    let mut oversized = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            break;
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index.saturating_add(1));
        let chunk = &available[..take];
        hasher.update(chunk);
        observed_bytes =
            observed_bytes
                .checked_add(chunk.len() as u64)
                .ok_or(CaptureError::SystemInvariant(
                    "Kimi JSONL line length overflowed",
                ))?;
        if bytes.len() < max_bytes.saturating_add(2) {
            let remaining = max_bytes.saturating_add(2).saturating_sub(bytes.len());
            bytes.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        }
        oversized |= observed_bytes > max_bytes as u64;
        terminated = chunk.last() == Some(&b'\n');
        reader.consume(take);
        if terminated {
            break;
        }
    }
    Ok(RawLine {
        bytes,
        observed_bytes,
        terminated,
        oversized,
    })
}

fn json_record_bytes(bytes: &[u8]) -> &[u8] {
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    bytes.strip_suffix(b"\r").unwrap_or(bytes)
}

fn initial_prefix_hasher() -> Sha256 {
    let mut hasher = Sha256::new();
    hasher.update(KIMI_PREFIX_DOMAIN);
    hasher
}

fn initial_prefix_sha256() -> [u8; 32] {
    prefix_digest(&initial_prefix_hasher())
}

fn prefix_digest(hasher: &Sha256) -> [u8; 32] {
    hasher.clone().finalize().into()
}

fn route_sha256(locator_identity: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(KIMI_ROUTE_DOMAIN);
    digest.update(locator_identity.as_bytes());
    digest.finalize().into()
}

fn domain_digest(domain: &[u8], value: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value.as_bytes());
    digest.finalize().into()
}

fn effective_source_revision(revision: &str, inventory_token: Option<&str>) -> String {
    let Some(token) = inventory_token else {
        return revision.to_owned();
    };
    let mut digest = Sha256::new();
    digest.update(b"ctx-kimi-inventory-observation-v1\0");
    digest.update((revision.len() as u64).to_be_bytes());
    digest.update(revision.as_bytes());
    digest.update((token.len() as u64).to_be_bytes());
    digest.update(token.as_bytes());
    format!("inventory-observation-sha256-v1:{:x}", digest.finalize())
}

fn replay_outputs(
    paths: &BTreeSet<PathBuf>,
    source_root: &Path,
    imported_at: DateTime<Utc>,
    sink: Option<&dyn ProOutputSink>,
) -> Result<bool> {
    let Some(sink) = sink else {
        return Ok(false);
    };
    replay_kimi_outputs(paths, source_root, imported_at, sink)
}

fn replay_kimi_outputs(
    paths: &BTreeSet<PathBuf>,
    source_root: &Path,
    imported_at: DateTime<Utc>,
    sink: &dyn ProOutputSink,
) -> Result<bool> {
    let mut output_behind = false;
    for path in paths {
        let locator_identity = provider_path_identity(path)?;
        let source = OutputSourceIdentity {
            provider: CaptureProvider::KimiCodeCli.as_str().to_owned(),
            namespace_id: source_root.display().to_string(),
            source_id: locator_identity.clone(),
        };
        let progress = match sink.observe_source(&source) {
            Ok(progress) => progress,
            Err(_) => {
                sink.mark_behind(ProOutputSinkError::new(
                    "kimi_output_progress",
                    "Kimi Pro output progress is unavailable",
                ));
                output_behind = true;
                continue;
            }
        };
        output_behind |= replay_kimi_source(
            path,
            source_root,
            imported_at,
            sink,
            source,
            locator_identity,
            progress,
        )?;
    }
    Ok(output_behind)
}

#[allow(clippy::too_many_arguments)]
fn replay_kimi_source(
    path: &Path,
    source_root: &Path,
    imported_at: DateTime<Utc>,
    sink: &dyn ProOutputSink,
    output_source: OutputSourceIdentity,
    locator_identity: String,
    progress: Option<ProOutputProgress>,
) -> Result<bool> {
    let observation = KimiWireObservation::read(path)?;
    let scope_revision =
        kimi_admission_scope_revision_for_display(Some(source_root.display().to_string()));
    let observed_revision = observation.source_revision(&scope_revision);
    let route_sha256 = route_sha256(&locator_identity);
    let progress_checkpoint = progress
        .as_ref()
        .and_then(|progress| progress.cursor.as_ref())
        .filter(|cursor| cursor.version == KIMI_OUTPUT_FRONTIER_VERSION)
        .and_then(|cursor| serde_json::from_slice::<KimiNativeCheckpoint>(&cursor.payload).ok());
    let parser_matches = progress.as_ref().is_some_and(|progress| {
        progress.parser_revision == KIMI_OUTPUT_PARSER_REVISION
            && progress.materializer_revision == sink.materializer_revision()
    });
    let (mut checkpoint, start_offset, start_ordinal, mut hasher, source_can_resume) =
        plan_output_scan(
            path,
            &observation,
            route_sha256,
            scope_revision,
            parser_matches
                .then_some(progress_checkpoint.as_ref())
                .flatten(),
        )?;
    if source_can_resume
        && progress.as_ref().is_some_and(|progress| {
            progress.terminal && progress.observed_revision == observed_revision
        })
        && checkpoint.terminal
    {
        return Ok(false);
    }
    let mut state = match KimiOutputState::new(
        output_source,
        progress,
        source_can_resume,
        sink.materializer_revision(),
    ) {
        Ok(state) => state,
        Err(_) => {
            sink.mark_behind(ProOutputSinkError::new(
                "kimi_output_progress",
                "Kimi Pro output progress is invalid",
            ));
            return Ok(true);
        }
    };
    let mut file = File::open(path)?;
    if KimiFrozenFileMetadata::from_metadata(&file.metadata()?)? != *observation.wire() {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    file.seek(SeekFrom::Start(start_offset))?;
    let mut reader = BufReader::new(file);
    let mut offset = start_offset;
    let mut ordinal = start_ordinal;
    let mut expected_checkpoint = checkpoint.clone();
    let mut observations = Vec::new();
    let mut page_bytes: usize = 64 * 1024;
    let mut page_units = 0_usize;
    let mut reached_eof = false;

    while !reached_eof {
        let checkpoint_before = checkpoint.clone();
        let hasher_before = hasher.clone();
        let raw = read_bounded_line(&mut reader, &mut hasher, MAX_PROVIDER_JSONL_LINE_BYTES)?;
        if raw.observed_bytes == 0 {
            reached_eof = true;
        } else if !raw.terminated {
            hasher = hasher_before;
            reached_eof = true;
        } else {
            let next_record_bytes = 512_usize;
            if page_units != 0
                && (page_units >= KIMI_OUTPUT_PAGE_MAX_OBSERVATIONS
                    || page_bytes.saturating_add(next_record_bytes) > KIMI_OUTPUT_PAGE_MAX_BYTES)
            {
                if !publish_output_page(
                    sink,
                    &observation,
                    &locator_identity,
                    &observed_revision,
                    &mut state,
                    &expected_checkpoint,
                    &checkpoint_before,
                    false,
                    page_units,
                    page_bytes,
                    std::mem::take(&mut observations),
                )? {
                    return Ok(true);
                }
                expected_checkpoint = checkpoint_before;
                page_bytes = 64 * 1024;
                page_units = 0;
            }
            let byte_start = offset;
            offset =
                offset
                    .checked_add(raw.observed_bytes)
                    .ok_or(CaptureError::SystemInvariant(
                        "Kimi output byte offset overflowed",
                    ))?;
            let line_number = usize::try_from(ordinal)
                .ok()
                .and_then(|value| value.checked_add(1))
                .ok_or(CaptureError::SystemInvariant(
                    "Kimi output line number overflowed",
                ))?;
            let next_ordinal = ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                "Kimi output ordinal overflowed",
            ))?;
            checkpoint.complete_offset = offset;
            checkpoint.next_ordinal = next_ordinal;
            checkpoint.committed_prefix_sha256 = prefix_digest(&hasher);
            checkpoint.observed_file_len = observation.wire().length;
            checkpoint.wire_revision = observation.wire().revision_component();
            checkpoint.terminal = false;
            checkpoint.retired = false;
            page_units = page_units.saturating_add(1);
            page_bytes = page_bytes.saturating_add(next_record_bytes);
            if !raw.oversized {
                let record = json_record_bytes(&raw.bytes);
                if let Ok(value) = serde_json::from_slice::<Value>(record) {
                    let output = match kimi_output_observation(
                        &observation,
                        &locator_identity,
                        ordinal,
                        line_number,
                        byte_start,
                        offset,
                        &value,
                        imported_at,
                    ) {
                        Ok(output) => output,
                        Err(_) => {
                            sink.mark_behind(ProOutputSinkError::new(
                                "kimi_output_page",
                                "Kimi Pro output observation is invalid",
                            ));
                            return Ok(true);
                        }
                    };
                    if let Some(output) = output {
                        page_bytes = page_bytes
                            .saturating_add(output.content.len())
                            .saturating_add(2048);
                        observations.push(output);
                    }
                }
            }
            ordinal = next_ordinal;
        }
    }
    checkpoint.terminal = offset == observation.wire().length;
    Ok(!publish_output_page(
        sink,
        &observation,
        &locator_identity,
        &observed_revision,
        &mut state,
        &expected_checkpoint,
        &checkpoint,
        checkpoint.terminal,
        page_units.max(1),
        page_bytes,
        observations,
    )?)
}

fn plan_output_scan(
    path: &Path,
    observation: &KimiWireObservation,
    route_sha256: [u8; 32],
    scope_revision: String,
    previous: Option<&KimiNativeCheckpoint>,
) -> Result<(KimiNativeCheckpoint, u64, u64, Sha256, bool)> {
    let Some(previous) = previous else {
        let checkpoint = KimiNativeCheckpoint::initial(route_sha256, observation, scope_revision);
        return Ok((checkpoint, 0, 0, initial_prefix_hasher(), false));
    };
    let physical = observation.wire().physical_identity();
    let identity_matches = previous.version == KIMI_NATIVE_CURSOR_VERSION
        && !previous.retired
        && previous.route_sha256 == route_sha256
        && previous.physical_device == physical.0
        && previous.physical_inode == physical.1
        && previous.auxiliary_revision == observation.session.auxiliary_revision
        && previous.admission_scope_revision == scope_revision
        && previous.complete_offset <= observation.wire().length;
    if !identity_matches {
        let checkpoint = KimiNativeCheckpoint::initial(route_sha256, observation, scope_revision);
        return Ok((checkpoint, 0, 0, initial_prefix_hasher(), false));
    }
    let Some(hasher) = verify_prefix(path, previous)? else {
        let checkpoint = KimiNativeCheckpoint::initial(route_sha256, observation, scope_revision);
        return Ok((checkpoint, 0, 0, initial_prefix_hasher(), false));
    };
    Ok((
        previous.clone(),
        previous.complete_offset,
        previous.next_ordinal,
        hasher,
        true,
    ))
}

struct KimiOutputState {
    source: OutputSourceIdentity,
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_sink_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
}

impl KimiOutputState {
    fn new(
        source: OutputSourceIdentity,
        progress: Option<ProOutputProgress>,
        can_resume: bool,
        materializer_revision: &str,
    ) -> Result<Self> {
        let Some(progress) = progress else {
            return Ok(Self {
                source,
                source_epoch: 0,
                expected_source_epoch: None,
                expected_sink_frontier: None,
                disposition: ProOutputSourceDisposition::NewSource,
            });
        };
        let prior_frontier = progress
            .cursor
            .as_ref()
            .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload.clone()))
            .transpose()
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        let rewrite = !can_resume
            || progress.parser_revision != KIMI_OUTPUT_PARSER_REVISION
            || progress.materializer_revision != materializer_revision;
        Ok(Self {
            source,
            source_epoch: if rewrite {
                progress
                    .source_epoch
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Kimi output source epoch exhausted",
                    ))?
            } else {
                progress.source_epoch
            },
            expected_source_epoch: Some(progress.source_epoch),
            expected_sink_frontier: prior_frontier,
            disposition: if rewrite {
                ProOutputSourceDisposition::Rewrite
            } else {
                ProOutputSourceDisposition::AppendOrResume
            },
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn publish_output_page(
    sink: &dyn ProOutputSink,
    observation: &KimiWireObservation,
    locator_identity: &str,
    observed_revision: &str,
    state: &mut KimiOutputState,
    expected_checkpoint: &KimiNativeCheckpoint,
    next_checkpoint: &KimiNativeCheckpoint,
    terminal: bool,
    logical_units: usize,
    conservative_serialized_bytes: usize,
    observations: Vec<ProOutputObservation>,
) -> Result<bool> {
    if !observation.revalidate(observation.canonical_path())? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let output_page = (|| {
        let expected_frontier = expected_checkpoint.safe_frontier()?;
        let next_safe_frontier = next_checkpoint.safe_frontier()?;
        let output = NativeProOutputPage {
            inventory_generation: sink.inventory_generation(),
            source: state.source.clone(),
            source_epoch: state.source_epoch,
            observed_revision: observed_revision.to_owned(),
            parser_revision: KIMI_OUTPUT_PARSER_REVISION.to_owned(),
            materializer_revision: sink.materializer_revision().to_owned(),
            disposition: state.disposition,
            expected_prior_source_epoch: state.expected_source_epoch,
            expected_prior_frontier: state.expected_sink_frontier.clone(),
            observations,
        };
        let replay = NativeProReplayPage::new_with_source_identity(
            NativeSourceIdentity::new(CaptureProvider::KimiCodeCli.as_str(), locator_identity),
            expected_frontier,
            next_safe_frontier.clone(),
            terminal,
            NativePageAccounting {
                logical_units,
                conservative_serialized_bytes,
            },
            output,
        )
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        Ok::<_, CaptureError>((replay, next_safe_frontier))
    })();
    let (replay, next_safe_frontier) = match output_page {
        Ok(output_page) => output_page,
        Err(_) => {
            sink.mark_behind(ProOutputSinkError::new(
                "kimi_output_page",
                "Kimi Pro output page is invalid",
            ));
            return Ok(false);
        }
    };
    if process_pro_replay_only(replay, sink).is_err() {
        return Ok(false);
    }
    state.expected_source_epoch = Some(state.source_epoch);
    state.expected_sink_frontier = Some(next_safe_frontier);
    state.disposition = ProOutputSourceDisposition::AppendOrResume;
    Ok(true)
}

fn record_output_behind(summary: &mut ProviderImportSummary) {
    summary.record_failure(ProviderImportFailure {
        line: 0,
        error: "Kimi Pro output is behind committed Core".to_owned(),
    });
}

#[allow(clippy::too_many_arguments)]
fn kimi_output_observation(
    observation: &KimiWireObservation,
    locator_identity: &str,
    ordinal: u64,
    line_number: usize,
    byte_start: u64,
    byte_end_exclusive: u64,
    value: &Value,
    imported_at: DateTime<Utc>,
) -> Result<Option<ProOutputObservation>> {
    let record_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if kimi_event_type(record_type, value) != EventType::ToolOutput {
        return Ok(None);
    }
    let metadata = kimi_output_metadata(value, line_number, observation.session.cwd.as_deref());
    let content = kimi_result_content(value).unwrap_or_default().into_bytes();
    let occurred_at = kimi_record_timestamp(value, imported_at).unwrap_or(imported_at);
    let source_item = locator_identity.as_bytes();
    let source_len = u32::try_from(source_item.len()).map_err(|_| {
        CaptureError::InvalidPayload("Kimi output source identity exceeds u32".to_owned())
    })?;
    let mut locator = Vec::with_capacity(source_item.len().saturating_add(20));
    locator.extend_from_slice(&source_len.to_be_bytes());
    locator.extend_from_slice(source_item);
    locator.extend_from_slice(&byte_start.to_be_bytes());
    locator.extend_from_slice(&byte_end_exclusive.to_be_bytes());
    let direct_session_id = observation.session.provider_session_id.clone();
    Ok(Some(ProOutputObservation {
        kind: metadata.kind,
        coordinate: OutputNativeCoordinate {
            unit_key: format!("{}:0", metadata.native_record_id),
            native_sequence: ordinal,
            native_record_id: Some(metadata.native_record_id),
            source_record_ordinal: Some(ordinal),
            source_record_subrecord_index: Some(0),
            byte_start: Some(byte_start),
            byte_end_exclusive: Some(byte_end_exclusive),
        },
        occurred_at_unix_ms: Some(occurred_at.timestamp_millis()),
        associations: OutputAssociations {
            direct_session_id: direct_session_id.clone(),
            root_session_id: observation
                .session
                .root_provider_session_id
                .clone()
                .unwrap_or_else(|| direct_session_id.clone()),
            parent_session_id: observation.session.parent_provider_session_id.clone(),
            provider_session_id: Some(direct_session_id),
            agent_id: Some(observation.session.agent_id.clone()),
            repository: None,
        },
        call_id: metadata.call_id,
        command: metadata.command,
        outcome: metadata.outcome,
        locator: OutputSourceLocator {
            version: 1,
            kind: KIMI_OUTPUT_LOCATOR_KIND.to_owned(),
            payload: locator,
        },
        content,
    }))
}

struct KimiOutputMetadata {
    kind: OutputObservationKind,
    native_record_id: String,
    call_id: Option<String>,
    command: Option<OutputCommandContext>,
    outcome: OutputOutcomeMetadata,
}

fn kimi_output_metadata(
    value: &Value,
    line_number: usize,
    session_cwd: Option<&str>,
) -> KimiOutputMetadata {
    let event = value.get("event").unwrap_or(value);
    let call_id = [
        "call_id",
        "callId",
        "tool_call_id",
        "toolCallId",
        "tool_use_id",
        "toolUseId",
        "id",
    ]
    .into_iter()
    .find_map(|field| event.get(field).and_then(Value::as_str))
    .filter(|value| !value.trim().is_empty())
    .map(str::to_owned);
    let tool_name = event
        .get("toolName")
        .or_else(|| event.get("tool_name"))
        .or_else(|| event.get("name"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("tool")
        .to_owned();
    let kind = if tool_input::is_command_tool(&tool_name.to_ascii_lowercase()) {
        OutputObservationKind::Command
    } else {
        OutputObservationKind::Tool
    };
    let command = (kind == OutputObservationKind::Command).then(|| OutputCommandContext {
        tool_name: tool_name.clone(),
        command: event
            .get("input")
            .or_else(|| event.get("arguments"))
            .or_else(|| event.get("args"))
            .and_then(tool_input::command)
            .unwrap_or_default(),
        working_directory: event
            .get("input")
            .or_else(|| event.get("arguments"))
            .or_else(|| event.get("args"))
            .and_then(tool_input::working_directory)
            .or_else(|| session_cwd.map(str::to_owned)),
    });
    let timed_out = kimi_value_timed_out(event);
    let exit_code = kimi_i64_field(event, &["exit_code", "exitCode"])
        .and_then(|value| i32::try_from(value).ok());
    let duration_ms = kimi_i64_field(event, &["duration_ms", "durationMs"])
        .and_then(|value| u64::try_from(value).ok());
    let outcome = if timed_out {
        OutputOutcome::Timeout
    } else if provider_output_event_is_failure(event) {
        OutputOutcome::Failure
    } else if provider_result_outcome_evidence(EventType::ToolOutput, event).as_str()
        == Some("success")
    {
        OutputOutcome::Success
    } else {
        OutputOutcome::Unknown
    };
    let record_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let native_record_id = format!(
        "{}:{}",
        record_type,
        value
            .get("time")
            .and_then(Value::as_i64)
            .map(|time| time.to_string())
            .unwrap_or_else(|| line_number.to_string())
    );
    KimiOutputMetadata {
        kind,
        native_record_id,
        call_id,
        command,
        outcome: OutputOutcomeMetadata {
            outcome,
            exit_code,
            duration_ms,
        },
    }
}

fn kimi_value_timed_out(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(kimi_value_timed_out),
        Value::Object(values) => {
            values.iter().any(|(key, value)| {
                matches!(key.as_str(), "timed_out" | "timedOut" | "timeout")
                    && value.as_bool().unwrap_or(false)
                    || matches!(key.as_str(), "status" | "state" | "outcome")
                        && value.as_str().is_some_and(|value| {
                            matches!(
                                value.trim().to_ascii_lowercase().as_str(),
                                "timeout" | "timed_out" | "timedout"
                            )
                        })
            }) || values.values().any(kimi_value_timed_out)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

fn kimi_i64_field(value: &Value, fields: &[&str]) -> Option<i64> {
    match value {
        Value::Array(values) => values
            .iter()
            .find_map(|value| kimi_i64_field(value, fields)),
        Value::Object(values) => fields
            .iter()
            .find_map(|field| values.get(*field).and_then(Value::as_i64))
            .or_else(|| {
                values
                    .values()
                    .find_map(|value| kimi_i64_field(value, fields))
            }),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => None,
    }
}
