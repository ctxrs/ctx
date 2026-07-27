use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    Confidence, ContentRef, Event, EventType, Fidelity, FileChangeKind, FileTouched, Run, Session,
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
    complete_content::structured::STRUCTURED_COMPLETE_CONTENT_LOCATOR_KIND,
    complete_content::{
        attach_verified_content_locator, verified_content_profile, CompleteContentBodyDigest,
        CompleteContentSourceFamily, VerifiedContentLocatorV1, VerifiedContentLocatorsV1,
        VerifiedContentRole, VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
    },
    provider::{
        file_touches::{
            event_type_supports_structured_file_touches,
            visit_provider_file_touch_drafts_with_limit, MAX_PACKED_PROVIDER_EVENT_INDEX,
            MAX_PROVIDER_FILE_TOUCHES_PER_EVENT, PROVIDER_FILE_TOUCH_LIMIT_REJECTION,
        },
        importer::{
            compact_provider_result_payload, provider_command_run,
            provider_event_import_identity_with_exact_legacy_source, provider_file_touch_import_id,
            provider_import_session_uuid, provider_path_identity,
            provider_scoped_source_identity_key, provider_source_cursor_stream_for_path,
            provider_source_edge_uuid, provider_source_identity, provider_source_root,
            provider_sync_metadata, timestamps, CertifiedProviderCursor,
        },
        native_ingestion::{
            process_pro_replay_only, NativeIngestionPageError, NativeOutputProFailure,
            NativePageAccounting, NativeProOutputPage, NativeProReplayPage, NativeSafeFrontier,
            NativeSourceIdentity,
        },
        normalization::{
            provider_block_text, provider_capped_json_value, provider_local_preview,
            provider_message_id, provider_output_event_is_failure,
            provider_result_outcome_evidence, provider_string_field,
            provider_timestamp_from_fields,
        },
        tool_input,
    },
    stable_capture_uuid, CaptureError, CaptureWorkLimit, OutputAssociations, OutputCommandContext,
    OutputNativeCoordinate, OutputObservationKind, OutputOutcome, OutputOutcomeMetadata,
    OutputSourceIdentity, OutputSourceLocator, ProOutputObservation, ProOutputProgress,
    ProOutputSink, ProOutputSinkError, ProOutputSourceDisposition, ProviderAdapterContext,
    ProviderImportFailure, ProviderImportOptions, ProviderImportSummary, ProviderImportWorkResult,
    Result, MAX_PROVIDER_JSONL_LINE_BYTES, PROVIDER_MAX_PREVIEW_CHARS, PROVIDER_MAX_TEXT_CHARS,
    ROVODEV_SOURCE_FORMAT,
};

use super::{
    event::{rovodev_event, rovodev_event_type, RovoDevCoreEvent},
    source::{
        discover_rovodev_session_sources, RovoDevDiscovery, RovoDevSessionObservation,
        RovoDevSessionSource,
    },
};

const ROVODEV_NATIVE_CURSOR_VERSION: u32 = 1;
const ROVODEV_NATIVE_FRONTIER_VERSION: u32 = 1;
const ROVODEV_NATIVE_PARSER_REVISION: &str = "rovodev-nativepath-v1";
const ROVODEV_NATIVE_POLICY_REVISION: u32 = 7;
const ROVODEV_OUTPUT_PARSER_REVISION: &str = "rovodev-output-nativepath-v1";
const ROVODEV_ROOT_CURSOR_FORMAT: &str = "rovodev-nativepath-root-v1";
const ROVODEV_NATIVE_LOCATOR_KIND: &str = "rovodev-session-context-message-v1";
const ROVODEV_PUBLICATION_DOMAIN: &[u8] = b"ctx-rovodev-nativepath-publication-v1\0";
const ROVODEV_ROOT_PUBLICATION_DOMAIN: &[u8] = b"ctx-rovodev-nativepath-root-v1\0";
const ROVODEV_RETIREMENT_PUBLICATION_DOMAIN: &[u8] = b"ctx-rovodev-nativepath-retirement-v1\0";
const ROVODEV_SOURCE_REVISION_DOMAIN: &[u8] = b"ctx-rovodev-native-source-v1\0";
const ROVODEV_PREFIX_DOMAIN: &[u8] = b"ctx-rovodev-message-prefix-v1\0";
const ROVODEV_PAGE_MAX_UNITS: usize = 64;
const ROVODEV_PAGE_MAX_BYTES: usize = 6 * 1024 * 1024;
const ROVODEV_MAX_FAILURES: usize = 4;
const ROVODEV_MAX_FAILURE_BYTES: usize = 4 * 1024;
const ROVODEV_MAX_JSON_DEPTH: usize = 128;
pub(super) const ROVODEV_NATIVE_MAX_COLLECTION_ELEMENTS: usize = 65_536;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RovoDevFrontier {
    version: u32,
    next_message_index: u64,
    prefix_sha256: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RovoDevOutputFrontier {
    version: u32,
    generation: u64,
    physical_identity: String,
    next_message_index: u64,
    prefix_sha256: [u8; 32],
}

impl RovoDevFrontier {
    fn start() -> Self {
        Self {
            version: ROVODEV_NATIVE_FRONTIER_VERSION,
            next_message_index: 0,
            prefix_sha256: prefix_sha256(&[]),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RovoDevFailure {
    line: usize,
    error: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RovoDevNativeCursor {
    version: u32,
    provider: String,
    source_identity: String,
    source_revision: String,
    physical_identity: String,
    locator_identity: String,
    source_id: Option<Uuid>,
    frontier: RovoDevFrontier,
    terminal: bool,
    missing: bool,
    generation: u64,
    accepted_sessions: u64,
    accepted_events: u64,
    accepted_file_touches: u64,
    rejected_records: u64,
    failures: Vec<RovoDevFailure>,
}

impl RovoDevNativeCursor {
    fn encode(&self) -> Result<String> {
        serde_json::to_string(self).map_err(|error| CaptureError::InvalidPayload(error.to_string()))
    }

    fn decode(encoded: &str) -> Result<Self> {
        let cursor: Self = serde_json::from_str(encoded)
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        if cursor.version != ROVODEV_NATIVE_CURSOR_VERSION
            || cursor.provider != CaptureProvider::RovoDev.as_str()
            || cursor.frontier.version != ROVODEV_NATIVE_FRONTIER_VERSION
            || cursor.source_identity.is_empty()
            || cursor.source_revision.is_empty()
            || cursor.physical_identity.is_empty()
            || cursor.locator_identity.is_empty()
            || cursor.failures.len() > ROVODEV_MAX_FAILURES
        {
            return Err(CaptureError::InvalidPayload(
                "RovoDev NativePath cursor is inconsistent".to_owned(),
            ));
        }
        Ok(cursor)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RovoDevManifestEntry {
    source_identity: String,
    cursor_stream: String,
    locator_identity: String,
    canonical_source_identity: Option<String>,
    source_revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RovoDevRootManifest {
    version: u32,
    root_identity: String,
    sources: Vec<RovoDevManifestEntry>,
}

#[derive(Debug)]
struct PreparedDocument {
    context_record: Vec<u8>,
    context_metadata: Value,
    metadata: Value,
    metadata_preview: Value,
    messages: Vec<Value>,
    provider_session_id: String,
    parent_provider_session_id: Option<String>,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    cwd: Option<String>,
    initial_failures: Vec<RovoDevFailure>,
}

#[derive(Debug)]
struct PreparedMessage {
    line: usize,
    event: Option<RovoDevCoreEvent>,
    touches: Vec<RovoDevFileTouch>,
    rejection: Option<RovoDevFailure>,
    estimated_bytes: usize,
}

#[derive(Debug)]
struct RovoDevFileTouch {
    provider_touch_index: u64,
    provider_event_index: Option<u64>,
    raw_source_path: Option<String>,
    source_root: Option<String>,
    path: String,
    change_kind: Option<FileChangeKind>,
    old_path: Option<String>,
    line_count_delta: Option<i64>,
    confidence: Confidence,
    occurred_at: DateTime<Utc>,
    metadata: Value,
}

impl RovoDevFileTouch {
    fn estimated_bytes(&self) -> usize {
        self.path
            .len()
            .saturating_add(self.old_path.as_ref().map_or(0, String::len))
            .saturating_add(self.raw_source_path.as_ref().map_or(0, String::len))
            .saturating_add(self.source_root.as_ref().map_or(0, String::len))
            .saturating_add(serde_json::to_vec(&self.metadata).map_or(0, |metadata| metadata.len()))
            .saturating_add(512)
    }
}

#[derive(Debug)]
struct PreparedPage {
    expected_frontier: RovoDevFrontier,
    next_frontier: RovoDevFrontier,
    terminal: bool,
    messages: Vec<PreparedMessage>,
    retained_bytes: usize,
}

#[derive(Debug)]
enum CursorPlan {
    AlreadyCommitted(RovoDevNativeCursor),
    Publish {
        expected: Option<String>,
        prior: Option<RovoDevNativeCursor>,
        generation: u64,
        start: usize,
        replacement: bool,
    },
}

#[derive(Debug)]
struct PublishedSource {
    cursor: RovoDevNativeCursor,
    summary: ProviderImportSummary,
    groups_changed: usize,
}

#[derive(Debug)]
struct ResolvedSource {
    source_id: Uuid,
    session: Session,
}

#[derive(Debug)]
struct OutputState {
    source: OutputSourceIdentity,
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_frontier: Option<NativeSafeFrontier>,
    source_start: usize,
    disposition: ProOutputSourceDisposition,
    requires_checkpoint: bool,
}

pub(crate) fn import_rovodev_native_path(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let root_identity = root_identity(path)?;
    let root_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::RovoDev,
        ROVODEV_ROOT_CURSOR_FORMAT,
        &root_identity,
    );
    let prior_root_cursor = store.get_sync_cursor(None, &context.machine_id, &root_stream)?;
    let mut manifest = load_manifest(prior_root_cursor.as_ref(), &root_identity)?;
    let discovery = discover_rovodev_session_sources(path)?;
    if discovery.sources().is_empty() && prior_root_cursor.is_none() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: if discovery.root_exists() {
                "no Rovo Dev session_context.json files found"
            } else {
                "Rovo Dev session root does not exist"
            },
        });
    }

    let configured_source_root = context
        .source_root
        .as_deref()
        .or(context.source_path.as_deref())
        .unwrap_or(path)
        .to_path_buf();
    let committed_store = Store::open_read_only(store.path())?;
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let mut summary = ProviderImportSummary::default();
        let mut changed_groups = 0_usize;
        let mut live_entries = BTreeMap::new();

        for source in discovery.sources() {
            let published = import_source(
                store,
                &committed_store,
                &bulk_guard,
                source,
                &configured_source_root,
                &root_stream,
                &mut manifest,
                &context,
                &options,
            )?;
            changed_groups = changed_groups.saturating_add(published.groups_changed);
            live_entries.insert(
                published.cursor.source_identity.clone(),
                manifest_entry(store, source, &published.cursor)?,
            );
            summary.merge_from(published.summary);
            if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup && changed_groups != 0 {
                summary.work_remaining = true;
                return Ok(summary);
            }
        }

        let live_identities = live_entries.keys().cloned().collect::<BTreeSet<_>>();
        let mut missing = manifest
            .sources
            .iter()
            .filter(|entry| !live_identities.contains(&entry.source_identity))
            .cloned()
            .collect::<Vec<_>>();
        missing.sort_by(|left, right| left.source_identity.cmp(&right.source_identity));
        for entry in missing {
            let retirement = retire_source(
                store,
                &bulk_guard,
                &context,
                &root_stream,
                &manifest,
                &entry,
                if discovery.root_exists() {
                    ProviderSourceRouteRetirementReason::SourceMissing
                } else {
                    ProviderSourceRouteRetirementReason::RootMissing
                },
            )?;
            manifest = retirement.0;
            changed_groups = changed_groups.saturating_add(retirement.1);
            summary.merge_from(retirement.2);
            if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup && changed_groups != 0 {
                summary.work_remaining = manifest
                    .sources
                    .iter()
                    .any(|source| !live_identities.contains(&source.source_identity));
                return Ok(summary);
            }
        }

        for entry in live_entries.into_values() {
            match manifest
                .sources
                .iter_mut()
                .find(|prior| prior.source_identity == entry.source_identity)
            {
                Some(prior) => *prior = entry,
                None => manifest.sources.push(entry),
            }
        }
        manifest
            .sources
            .sort_by(|left, right| left.source_identity.cmp(&right.source_identity));

        revalidate_discovery(path, &discovery)?;
        let manifest_summary = publish_manifest(
            store,
            &bulk_guard,
            &context,
            &root_stream,
            prior_root_cursor.as_ref(),
            &manifest,
        )?;
        summary.merge_from(manifest_summary);
        Ok(summary)
    })();
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    match (operation, finish) {
        (Ok(summary), Ok(())) => Ok(summary),
        (_, Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
    }
}

#[allow(clippy::too_many_arguments)]
fn import_source(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    source: &RovoDevSessionSource,
    configured_source_root: &Path,
    root_stream: &str,
    manifest: &mut RovoDevRootManifest,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
) -> Result<PublishedSource> {
    let observation = RovoDevSessionObservation::read(source)?;
    let context_oversized = observation.context_length() > MAX_PROVIDER_JSONL_LINE_BYTES as u64;
    let context_bytes = if context_oversized {
        None
    } else {
        Some(fs::read(&source.context_path)?)
    };
    let metadata_oversized = observation
        .metadata_length()
        .is_some_and(|length| length > MAX_PROVIDER_JSONL_LINE_BYTES as u64);
    let metadata_bytes = match source.metadata_path.as_deref() {
        Some(path) if !metadata_oversized => Some(fs::read(path)?),
        Some(_) | None => None,
    };
    let metadata_source = source.metadata_path.as_deref().map(|_| {
        (
            metadata_bytes.as_deref(),
            observation.metadata_length().unwrap_or(0),
        )
    });
    if !observation.revalidate(source)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }

    let path_identity = provider_path_identity(observation.canonical_path())?;
    let source_identity = format!("rovodev-session:{path_identity}");
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::RovoDev,
        ROVODEV_SOURCE_FORMAT,
        &path_identity,
    );
    let source_revision = source_revision(
        context_bytes.as_deref(),
        observation.context_length(),
        metadata_source,
        observation.revision_authority(),
        options.inventory_observation_token.as_deref(),
    );
    let physical_identity = observation.physical_identity();
    let document = if context_oversized {
        Err(failure(
            1,
            format!(
                "Rovo Dev session_context.json exceeds the {MAX_PROVIDER_JSONL_LINE_BYTES} byte limit"
            ),
        ))
    } else {
        prepare_document(
            source,
            context,
            context_bytes.as_deref().unwrap_or_default(),
            metadata_bytes.as_deref(),
            metadata_oversized.then(|| {
                failure(
                    1,
                    format!(
                        "Rovo Dev metadata.json exceeds the {MAX_PROVIDER_JSONL_LINE_BYTES} byte limit"
                    ),
                )
            }),
        )
    };
    let stored = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let plan = classify_cursor(
        stored.as_ref(),
        &source_identity,
        &source_revision,
        &physical_identity,
        document.as_ref().ok(),
    )?;

    let replay_only = options.import_profile.is_replay_only();
    let mut summary = ProviderImportSummary::default();
    let mut groups_changed = 0_usize;
    let final_cursor = match plan {
        CursorPlan::AlreadyCommitted(cursor) => {
            replay_cursor_summary(&cursor, &mut summary);
            cursor
        }
        CursorPlan::Publish {
            mut expected,
            prior,
            generation,
            start,
            replacement,
        } => {
            if replay_only {
                return Err(CaptureError::InvalidPayload(
                    "RovoDev output replay requires matching committed NativePath Core".to_owned(),
                ));
            }
            match document.as_ref() {
                Ok(document) => {
                    let mut next = start;
                    let mut prior_cursor = prior;
                    loop {
                        let page = prepare_page(source, context, document, next)?;
                        let cursor = publish_core_page(
                            store,
                            committed_store,
                            bulk_guard,
                            source,
                            configured_source_root,
                            root_stream,
                            manifest,
                            context,
                            options,
                            &observation,
                            &source_identity,
                            &source_revision,
                            &physical_identity,
                            &stream,
                            expected,
                            prior_cursor.as_ref(),
                            generation,
                            replacement && next == start,
                            document,
                            page,
                            &mut summary,
                        )?;
                        groups_changed = groups_changed.saturating_add(1);
                        expected = store
                            .get_sync_cursor(None, &context.machine_id, &stream)?
                            .map(|cursor| cursor.cursor);
                        next =
                            usize::try_from(cursor.frontier.next_message_index).map_err(|_| {
                                CaptureError::InvalidPayload(
                                    "RovoDev NativePath frontier exceeds usize".to_owned(),
                                )
                            })?;
                        let terminal = cursor.terminal;
                        prior_cursor = Some(cursor);
                        if terminal || options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
                        {
                            break prior_cursor.ok_or(CaptureError::SystemInvariant(
                                "RovoDev NativePath lost its committed cursor",
                            ))?;
                        }
                    }
                }
                Err(failure) => {
                    let cursor = publish_rejection_cursor(
                        store,
                        committed_store,
                        bulk_guard,
                        source,
                        root_stream,
                        manifest,
                        context,
                        &observation,
                        &source_identity,
                        &source_revision,
                        &physical_identity,
                        &stream,
                        expected,
                        prior.as_ref(),
                        generation,
                        replacement,
                        failure.clone(),
                    )?;
                    groups_changed = groups_changed.saturating_add(1);
                    replay_cursor_summary(&cursor, &mut summary);
                    summary.set_work_result(ProviderImportWorkResult::Changed);
                    cursor
                }
            }
        }
    };

    if let Some(sink) = options.import_profile.sink() {
        if let Ok(document) = document.as_ref() {
            if final_cursor.terminal && final_cursor.source_revision == source_revision {
                if let Err(error) = replay_outputs(
                    source,
                    document,
                    &source_identity,
                    &final_cursor,
                    sink.as_ref(),
                ) {
                    sink.mark_behind(error.clone());
                    summary.record_failure(ProviderImportFailure {
                        line: 0,
                        error: format!("RovoDev output replay is behind: {error}"),
                    });
                }
            }
        }
    }

    Ok(PublishedSource {
        cursor: final_cursor,
        summary,
        groups_changed,
    })
}

fn prepare_document(
    source: &RovoDevSessionSource,
    context: &ProviderAdapterContext,
    context_bytes: &[u8],
    metadata_bytes: Option<&[u8]>,
    metadata_acquisition_failure: Option<RovoDevFailure>,
) -> std::result::Result<PreparedDocument, RovoDevFailure> {
    let context_json = serde_json::from_slice::<Value>(context_bytes)
        .map_err(|error| failure(1, format!("invalid Rovo Dev session_context.json: {error}")))?;
    validate_json_bounds(&context_json)
        .map_err(|error| failure(1, format!("Rovo Dev session_context.json {error}")))?;
    let messages = message_history(&context_json).cloned().ok_or_else(|| {
        failure(
            1,
            "Rovo Dev session_context.json is missing message_history array",
        )
    })?;
    let context_metadata = metadata_without_transcripts(&context_json);

    let mut initial_failures = metadata_acquisition_failure.into_iter().collect::<Vec<_>>();
    let metadata = match metadata_bytes {
        Some(bytes) => match serde_json::from_slice::<Value>(bytes) {
            Ok(value) => match validate_json_bounds(&value) {
                Ok(()) => value,
                Err(error) => {
                    initial_failures.push(failure(1, format!("Rovo Dev metadata.json {error}")));
                    Value::Null
                }
            },
            Err(error) => {
                initial_failures.push(failure(
                    1,
                    format!("invalid Rovo Dev metadata.json: {error}"),
                ));
                Value::Null
            }
        },
        None => Value::Null,
    };
    let metadata_preview = metadata_without_transcripts(&metadata);
    let provider_session_id = provider_string_field(&metadata, &["session_id", "sessionId"])
        .or_else(|| provider_string_field(&context_json, &["session_id", "sessionId"]))
        .unwrap_or_else(|| source.provider_session_id.clone());
    let parent_provider_session_id = provider_string_field(
        &metadata,
        &[
            "parent_session_id",
            "parentSessionId",
            "forked_from_session_id",
            "forkedFromSessionId",
            "fork_parent_id",
        ],
    );
    let started_at = provider_timestamp_from_fields(
        &metadata,
        &["created_at", "createdAt", "started_at", "startedAt"],
    )
    .or_else(|| messages.iter().find_map(message_timestamp))
    .unwrap_or(context.imported_at);
    let ended_at = provider_timestamp_from_fields(
        &metadata,
        &["updated_at", "updatedAt", "last_updated", "lastUpdated"],
    )
    .or_else(|| messages.iter().rev().find_map(message_timestamp));
    let cwd = provider_string_field(
        &metadata,
        &[
            "workspace_path",
            "workspacePath",
            "working_directory",
            "workingDirectory",
            "cwd",
        ],
    );
    Ok(PreparedDocument {
        context_record: context_bytes.to_vec(),
        context_metadata,
        metadata,
        metadata_preview,
        messages,
        provider_session_id,
        parent_provider_session_id,
        started_at,
        ended_at,
        cwd,
        initial_failures,
    })
}

fn prepare_page(
    source: &RovoDevSessionSource,
    context: &ProviderAdapterContext,
    document: &PreparedDocument,
    start: usize,
) -> Result<PreparedPage> {
    if start > document.messages.len() {
        return Err(CaptureError::InvalidPayload(
            "RovoDev NativePath frontier exceeds the source".to_owned(),
        ));
    }
    let expected_frontier = frontier(&document.messages, start)?;
    let mut messages = Vec::new();
    let mut retained_bytes = 512_usize;
    let mut units = 5_usize;
    let mut next = start;
    while next < document.messages.len() {
        let prepared = prepare_message(source, context, document, next)?;
        let message_units = prepared
            .event
            .as_ref()
            .map_or(0, |event| {
                usize::from(event.event_type == EventType::CommandOutput) + 1
            })
            .saturating_add(prepared.touches.len())
            .saturating_add(usize::from(prepared.rejection.is_some()));
        if message_units > ROVODEV_PAGE_MAX_UNITS {
            let line = next.saturating_add(1);
            messages.push(PreparedMessage {
                line,
                event: None,
                touches: Vec::new(),
                rejection: Some(failure(
                    line,
                    "RovoDev message exceeds the bounded NativePath mutation page",
                )),
                estimated_bytes: 256,
            });
            next = next.saturating_add(1);
            break;
        }
        let next_units = units.saturating_add(message_units);
        let next_bytes = retained_bytes.saturating_add(prepared.estimated_bytes);
        if !messages.is_empty()
            && (next_units > ROVODEV_PAGE_MAX_UNITS || next_bytes > ROVODEV_PAGE_MAX_BYTES)
        {
            break;
        }
        if next_bytes > ROVODEV_PAGE_MAX_BYTES {
            let line = next.saturating_add(1);
            messages.push(PreparedMessage {
                line,
                event: None,
                touches: Vec::new(),
                rejection: Some(failure(
                    line,
                    "RovoDev message exceeds the bounded NativePath byte page",
                )),
                estimated_bytes: 256,
            });
            next = next.saturating_add(1);
            break;
        }
        units = next_units;
        retained_bytes = next_bytes;
        messages.push(prepared);
        next = next.saturating_add(1);
    }
    let terminal = next == document.messages.len();
    let next_frontier = frontier(&document.messages, next)?;
    Ok(PreparedPage {
        expected_frontier,
        next_frontier,
        terminal,
        messages,
        retained_bytes,
    })
}

fn prepare_message(
    source: &RovoDevSessionSource,
    context: &ProviderAdapterContext,
    document: &PreparedDocument,
    index: usize,
) -> Result<PreparedMessage> {
    let message = document
        .messages
        .get(index)
        .ok_or(CaptureError::SystemInvariant(
            "RovoDev NativePath message index escaped its document",
        ))?;
    let line = index.saturating_add(1);
    let event_index = u64::try_from(index)
        .map_err(|_| CaptureError::InvalidPayload("RovoDev event index exceeds u64".to_owned()))?;
    let occurred_at = message_timestamp(message).unwrap_or(document.started_at);
    let role_text = message
        .get("role")
        .or_else(|| message.get("kind"))
        .or_else(|| message.get("type"))
        .and_then(Value::as_str);
    let output = rovodev_event_type(message, role_text) == EventType::ToolOutput;
    let output_metadata =
        output.then(|| output_metadata(message, event_index, document.cwd.as_deref()));
    let retained_failure = output_metadata.as_ref().is_some_and(|metadata| {
        matches!(
            metadata.outcome.outcome,
            OutputOutcome::Failure | OutputOutcome::Timeout
        )
    });
    let mut event = if !output || retained_failure {
        let mut event = rovodev_event(event_index, message, occurred_at, source);
        event.metadata["source_record_ordinal"] = json!(0_u64);
        event.metadata["source_record_subrecord_index"] = json!(index);
        if let Some(metadata) = output_metadata.as_ref() {
            let content = super::rovodev_result_content(message).unwrap_or_default();
            if metadata.kind == OutputObservationKind::Command {
                event.event_type = EventType::CommandOutput;
            }
            let (preview, _) = provider_local_preview(&content, PROVIDER_MAX_PREVIEW_CHARS);
            event.payload["result_outcome"] = Value::String("failure".to_owned());
            event.payload["output_bytes"] = json!(content.len());
            event.payload["output_preview"] = Value::String(preview);
            event.payload["call_id"] = metadata
                .call_id
                .as_ref()
                .map_or(Value::Null, |value| Value::String(value.clone()));
            event.payload["exit_code"] = metadata
                .outcome
                .exit_code
                .map_or(Value::Null, |value| Value::from(i64::from(value)));
            event.payload["duration_ms"] = metadata
                .outcome
                .duration_ms
                .map_or(Value::Null, Value::from);
            event.payload["timed_out"] =
                Value::Bool(metadata.outcome.outcome == OutputOutcome::Timeout);
            if let Some(command) = &metadata.command {
                event.payload["tool"] = Value::String(command.tool_name.clone());
                event.payload["command"] = Value::String(command.command.clone());
                event.payload["cwd"] = command
                    .working_directory
                    .as_ref()
                    .map_or(Value::Null, |value| Value::String(value.clone()));
            }
        } else if let Some(complete_text) = provider_block_text(message) {
            let native_id = event.provider_event_hash.clone();
            attach_rovodev_complete_content_locator(
                &mut event,
                0,
                u32::try_from(index).map_err(|_| {
                    CaptureError::InvalidPayload("RovoDev event index exceeds u32".to_owned())
                })?,
                &native_id,
                &document.context_record,
                &complete_text,
            )?;
        }
        Some(event)
    } else {
        None
    };

    if let Some(event) = event.as_mut() {
        event.payload =
            provider_capped_json_value(&event.payload, MAX_PROVIDER_JSONL_LINE_BYTES / 4);
    }
    let source_root = context.source_root_display();
    let raw_source_path = source.context_path.display().to_string();
    let mut touches = Vec::new();
    let include_structured_touches = event
        .as_ref()
        .is_some_and(|event| event_type_supports_structured_file_touches(event.event_type));
    let event_supports_file_touches = event.as_ref().is_some_and(|event| {
        matches!(
            event.event_type,
            EventType::ToolCall
                | EventType::ToolOutput
                | EventType::CommandOutput
                | EventType::FileTouched
        )
    });
    let touch_limit_exceeded = (output || event_supports_file_touches)
        .then(|| {
            visit_provider_file_touch_drafts_with_limit(
                message,
                !output && include_structured_touches,
                MAX_PROVIDER_FILE_TOUCHES_PER_EVENT,
                |(ordinal, touch)| {
                    let provider_touch_index = if event_index > MAX_PACKED_PROVIDER_EVENT_INDEX {
                        ordinal
                    } else {
                        (event_index << 16) | ordinal
                    };
                    touches.push(RovoDevFileTouch {
                        provider_touch_index,
                        provider_event_index: Some(event_index),
                        raw_source_path: Some(raw_source_path.clone()),
                        source_root: source_root.clone(),
                        path: touch.path,
                        change_kind: touch.change_kind,
                        old_path: touch.old_path,
                        line_count_delta: None,
                        confidence: touch.confidence,
                        occurred_at,
                        metadata: touch.metadata,
                    });
                    Ok::<(), CaptureError>(())
                },
            )
        })
        .transpose()?
        .is_some_and(|outcome| outcome.limit_exceeded());
    let rejection =
        touch_limit_exceeded.then(|| failure(line, PROVIDER_FILE_TOUCH_LIMIT_REJECTION));
    let estimated_bytes = event
        .as_ref()
        .map_or(256, RovoDevCoreEvent::estimated_bytes)
        .saturating_add(
            touches
                .iter()
                .map(RovoDevFileTouch::estimated_bytes)
                .sum::<usize>(),
        )
        .saturating_add(256);
    Ok(PreparedMessage {
        line,
        event,
        touches,
        rejection,
        estimated_bytes,
    })
}

#[allow(clippy::too_many_arguments)]
fn publish_core_page(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    source: &RovoDevSessionSource,
    configured_source_root: &Path,
    root_stream: &str,
    manifest: &mut RovoDevRootManifest,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    observation: &RovoDevSessionObservation,
    source_identity: &str,
    source_revision: &str,
    physical_identity: &str,
    stream: &str,
    expected_cursor: Option<String>,
    prior: Option<&RovoDevNativeCursor>,
    generation: u64,
    replacement: bool,
    document: &PreparedDocument,
    page: PreparedPage,
    aggregate: &mut ProviderImportSummary,
) -> Result<RovoDevNativeCursor> {
    if !observation.revalidate(source)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let locator_identity = format!("{source_identity}:generation:{generation}");
    let next_cursor = next_cursor(
        source_identity,
        source_revision,
        physical_identity,
        &locator_identity,
        prior,
        generation,
        &page,
        document,
    )?;
    let next_sync_cursor = sync_cursor(
        context,
        stream,
        next_cursor.encode()?,
        CaptureProvider::RovoDev,
    );
    let source_transition = NativePathCursorTransition::new(expected_cursor, next_sync_cursor);
    let next_manifest = manifest_with_entry(
        manifest,
        manifest_entry_with_canonical(source, &next_cursor, None)?,
    );
    let root_transition =
        manifest_transition(store, context, root_stream, manifest, &next_manifest)?;
    let mut transitions = vec![source_transition];
    if let Some(transition) = root_transition {
        transitions.push(transition);
    }
    let publication_id = publication_id(source_identity, source_revision, &page, &transitions);
    let retained_bytes = transitions
        .iter()
        .map(|transition| transition.next().cursor.len())
        .sum::<usize>()
        .saturating_add(page.retained_bytes);
    let accounting = NativePathGroupAccounting::new(1, transitions.len(), retained_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    match group.classify_cursor_set(&publication_id, &transitions)? {
        NativePathCursorSetClassification::AllNextSameGroup { .. } => {
            group.commit()?;
            *manifest = next_manifest;
            return Ok(next_cursor);
        }
        NativePathCursorSetClassification::AllExpected => {}
    }

    if replacement {
        if let Some(prior) = prior {
            if let Some(source_id) = prior.source_id {
                let canonical = committed_store
                    .get_capture_source(source_id)?
                    .descriptor
                    .source_identity
                    .ok_or(CaptureError::SystemInvariant(
                        "RovoDev prior source lost its canonical identity",
                    ))?;
                let retirement = ProviderSourceRouteRetirement {
                    provider: CaptureProvider::RovoDev,
                    source_format: ROVODEV_SOURCE_FORMAT.to_owned(),
                    machine_id: context.machine_id.clone(),
                    locator_identity: prior.locator_identity.clone(),
                    cursor_stream: stream.to_owned(),
                    expected_canonical_source_identity: canonical,
                    expected_source_revision: prior.source_revision.clone(),
                    retired_at_ms: context.imported_at.timestamp_millis(),
                    reason: ProviderSourceRouteRetirementReason::Replaced,
                };
                group.retire_provider_source_route(&retirement)?;
            }
        }
    }

    let mut page_summary = ProviderImportSummary::default();
    let resolved = resolve_source(
        committed_store,
        &mut group,
        source,
        configured_source_root,
        context,
        options,
        source_identity,
        source_revision,
        &locator_identity,
        stream,
        document,
        &mut page_summary,
    )?;
    if let Some(resolved) = resolved.as_ref() {
        for message in &page.messages {
            publish_message(
                committed_store,
                &mut group,
                context,
                options,
                source,
                resolved,
                message,
                &mut page_summary,
            )?;
        }
    }
    for initial in document.initial_failures.iter().take(ROVODEV_MAX_FAILURES) {
        if page.expected_frontier.next_message_index == 0 {
            page_summary.record_failure(ProviderImportFailure {
                line: initial.line,
                error: initial.error.clone(),
            });
        }
    }
    for message in &page.messages {
        if let Some(rejection) = &message.rejection {
            page_summary.record_failure(ProviderImportFailure {
                line: rejection.line,
                error: rejection.error.clone(),
            });
        }
    }

    if !observation.revalidate(source)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    *manifest = next_manifest;
    page_summary.set_work_result(ProviderImportWorkResult::Changed);
    aggregate.merge_from(page_summary);
    Ok(next_cursor)
}

#[allow(clippy::too_many_arguments)]
fn resolve_source(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    source: &RovoDevSessionSource,
    configured_source_root: &Path,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    source_identity: &str,
    source_revision: &str,
    locator_identity: &str,
    stream: &str,
    document: &PreparedDocument,
    summary: &mut ProviderImportSummary,
) -> Result<Option<ResolvedSource>> {
    let raw_source_path = source.context_path.display().to_string();
    let source_root = configured_source_root.display().to_string();
    let proposed_source_identity = provider_source_identity(
        CaptureProvider::RovoDev,
        ROVODEV_SOURCE_FORMAT,
        Some(&source_root),
        Some(&raw_source_path),
        Some(source_identity),
        &json!({"native_source_id": source_identity}),
    )
    .ok_or(CaptureError::SystemInvariant(
        "RovoDev NativePath source has no canonical identity",
    ))?;
    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::RovoDev,
            source_format: ROVODEV_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity: locator_identity.to_owned(),
            cursor_stream: stream.to_owned(),
            proposed_source_identity,
            raw_source_path: Some(raw_source_path.clone()),
            source_revision: source_revision.to_owned(),
            observed_at_ms: context.imported_at.timestamp_millis(),
        })?;
    let source_id = native_source_id(
        source_identity,
        locator_identity,
        &document.provider_session_id,
    );
    group.upsert_capture_source(&capture_source(
        source_id,
        source,
        context,
        configured_source_root,
        source_revision,
        &resolution.canonical_source_identity,
        document,
    ))?;
    group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;

    let session = canonical_session(
        committed_store,
        source_id,
        &resolution.canonical_source_identity,
        context,
        options,
        document,
    )?;
    if let Some(parent_id) = session.parent_session_id {
        if committed_store.get_session(parent_id).is_err() {
            group.insert_session_if_absent(&relationship_placeholder(
                parent_id,
                source_id,
                context,
                options,
                document
                    .parent_provider_session_id
                    .as_deref()
                    .unwrap_or_default(),
            ))?;
        }
    }
    let existed = committed_store.get_session(session.id).is_ok();
    group.upsert_session(&session)?;
    if existed {
        summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    } else {
        summary.imported_sessions = summary.imported_sessions.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    }
    if let Some(parent_id) = session.parent_session_id {
        let actor = canonical_actor(&session);
        group.upsert_projection_neutral_session_edge(
            &actor,
            &relationship_edge(
                source_id,
                &resolution.canonical_source_identity,
                context,
                &session,
                parent_id,
            ),
        )?;
        summary.imported_edges = summary.imported_edges.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    }
    Ok(Some(ResolvedSource { source_id, session }))
}

#[allow(clippy::too_many_arguments)]
fn publish_message(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    _source: &RovoDevSessionSource,
    resolved: &ResolvedSource,
    message: &PreparedMessage,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let event_id = if let Some(event) = message.event.as_ref() {
        let event_hash = event.provider_event_hash.clone();
        let identity = provider_event_import_identity_with_exact_legacy_source(
            committed_store,
            CaptureProvider::RovoDev,
            resolved
                .session
                .external_session_id
                .as_deref()
                .unwrap_or_default(),
            resolved.source_id,
            event.provider_event_index,
            event.provider_event_index,
            &event_hash,
            None,
            Some(event.provider_event_index),
            resolved.session.id
                == crate::provider::importer::provider_session_uuid(
                    CaptureProvider::RovoDev,
                    resolved
                        .session
                        .external_session_id
                        .as_deref()
                        .unwrap_or_default(),
                ),
        )?;
        let (canonical_event, run) = rovodev_canonical_event(
            resolved
                .session
                .external_session_id
                .as_deref()
                .unwrap_or_default(),
            resolved.source_id,
            resolved.session.id,
            message.line,
            event,
            &event_hash,
            &identity,
            context,
            options,
        )?;
        if let Some(run) = run.as_ref() {
            group.upsert_run(run)?;
        }
        if group.reconcile_provider_event(
            &canonical_event,
            ProviderEventHashAuthority::ProviderSupplied,
        )? {
            summary.imported_events = summary.imported_events.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        } else {
            summary.skipped_events = summary.skipped_events.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        }
        summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
        Some(canonical_event.id)
    } else {
        None
    };

    for touch in &message.touches {
        let provider_session_id = resolved
            .session
            .external_session_id
            .as_deref()
            .unwrap_or_default();
        let touch_id = provider_file_touch_import_id(
            committed_store,
            CaptureProvider::RovoDev,
            provider_session_id,
            resolved.source_id,
            touch.provider_event_index,
            touch.provider_touch_index,
            resolved.session.id
                == crate::provider::importer::provider_session_uuid(
                    CaptureProvider::RovoDev,
                    provider_session_id,
                ),
        )?;
        let file = rovodev_canonical_file_touch(
            touch,
            provider_session_id,
            options.history_record_id,
            resolved.source_id,
            resolved.session.id,
            event_id,
            touch_id,
        );
        group.upsert_file_touched(&file)?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn rovodev_canonical_event(
    provider_session_id: &str,
    source_id: Uuid,
    session_id: Uuid,
    line_number: usize,
    event: &RovoDevCoreEvent,
    event_hash: &str,
    identity: &crate::provider::importer::ProviderEventImportIdentity,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
) -> Result<(Event, Option<Run>)> {
    let mut provider_metadata = event.metadata.clone();
    let source_record_ordinal = provider_metadata
        .as_object_mut()
        .and_then(|metadata| metadata.remove("source_record_ordinal"))
        .and_then(|value| value.as_u64())
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "RovoDev source record ordinal annotation is malformed".to_owned(),
            )
        })?;
    let source_record_subrecord_index = provider_metadata
        .as_object_mut()
        .and_then(|metadata| metadata.remove("source_record_subrecord_index"))
        .and_then(|value| value.as_u64())
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "RovoDev source record subrecord annotation is malformed".to_owned(),
            )
        })?;
    let verified_content_locators = provider_metadata
        .as_object_mut()
        .and_then(|metadata| metadata.remove(VERIFIED_CONTENT_LOCATORS_METADATA_KEY))
        .map(|value| {
            VerifiedContentLocatorsV1::from_metadata_value(&value)
                .map(|locators| locators.to_metadata_value())
                .ok_or_else(|| {
                    CaptureError::InvalidPayload(
                        "RovoDev verified content locator annotation is malformed".to_owned(),
                    )
                })
        })
        .transpose()?;
    let run = provider_command_run(
        CaptureProvider::RovoDev,
        provider_session_id,
        session_id,
        source_id,
        identity.run_source_id,
        options.history_record_id,
        event.event_type,
        event.occurred_at,
        Fidelity::Imported,
        event.provider_event_index,
        &event.payload,
        event_hash,
    )?;
    let dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, event_hash)
            .unwrap_or_else(|| identity.dedupe_key.clone());
    let mut sync_metadata = json!({
        "provider_session_id": provider_session_id,
        "provider_event_index": event.provider_event_index,
        "provider_event_hash": event_hash,
        "provider_event_hash_authority": ProviderEventHashAuthority::ProviderSupplied.as_str(),
        "cursor": event.cursor,
        "source_format": ROVODEV_SOURCE_FORMAT,
        "source_trust": "provider_native",
        "fixture_line": line_number,
        "imported_at": context.imported_at,
        "event_idempotency_key": format!(
            "provider-event:{}:{}:{}",
            CaptureProvider::RovoDev.as_str(),
            provider_session_id,
            event.provider_event_index,
        ),
        "source_record_ordinal": source_record_ordinal,
        "source_record_subrecord_index": source_record_subrecord_index,
        "metadata": provider_metadata,
    });
    if let (Some(metadata), Some(locators)) =
        (sync_metadata.as_object_mut(), verified_content_locators)
    {
        metadata.insert(VERIFIED_CONTENT_LOCATORS_METADATA_KEY.to_owned(), locators);
    }
    Ok((
        Event {
            id: identity.id,
            seq: identity.seq,
            history_record_id: options.history_record_id,
            session_id: Some(session_id),
            run_id: run.as_ref().map(|run| run.id),
            event_type: event.event_type,
            role: event.role,
            occurred_at: event.occurred_at,
            capture_source_id: Some(source_id),
            payload: json!({
                "provider": CaptureProvider::RovoDev.as_str(),
                "provider_session_id": provider_session_id,
                "provider_event_index": event.provider_event_index,
                "provider_event_hash": event_hash,
                "cursor": event.cursor,
                "artifacts": [],
                "body": compact_provider_result_payload(event.event_type, &event.payload),
            }),
            payload_blob_id: None,
            dedupe_key: Some(dedupe_key),
            sync: provider_sync_metadata(Fidelity::Imported, sync_metadata),
        },
        run,
    ))
}

#[allow(clippy::too_many_arguments)]
fn rovodev_canonical_file_touch(
    touch: &RovoDevFileTouch,
    provider_session_id: &str,
    history_record_id: Option<Uuid>,
    source_id: Uuid,
    session_id: Uuid,
    event_id: Option<Uuid>,
    touch_id: Uuid,
) -> FileTouched {
    FileTouched {
        id: touch_id,
        history_record_id,
        run_id: None,
        event_id,
        vcs_workspace_id: None,
        path: touch.path.clone(),
        change_kind: touch.change_kind,
        old_path: touch.old_path.clone(),
        line_count_delta: touch.line_count_delta,
        confidence: touch.confidence,
        timestamps: timestamps(touch.occurred_at),
        source_id: Some(source_id),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider": CaptureProvider::RovoDev.as_str(),
                "provider_session_id": provider_session_id,
                "provider_touch_index": touch.provider_touch_index,
                "provider_event_index": touch.provider_event_index,
                "raw_source_path": touch.raw_source_path,
                "source_id": source_id,
                "source_format": ROVODEV_SOURCE_FORMAT,
                "source_root": provider_source_root(
                    touch.source_root.as_deref(),
                    touch.raw_source_path.as_deref(),
                ),
                "metadata": touch.metadata,
                "session_id": session_id,
            }),
        ),
    }
}

fn attach_rovodev_complete_content_locator(
    event: &mut RovoDevCoreEvent,
    source_record_ordinal: u64,
    source_record_subrecord_index: u32,
    native_record_id: &str,
    record_bytes: &[u8],
    complete_text: &str,
) -> Result<()> {
    if event.event_type != EventType::Message
        || complete_text.chars().count() <= PROVIDER_MAX_TEXT_CHARS
    {
        return Ok(());
    }
    if native_record_id.is_empty()
        || native_record_id.len() > 1_024
        || native_record_id.chars().any(char::is_control)
    {
        return Err(CaptureError::InvalidPayload(
            "RovoDev complete-content native record identity is invalid".to_owned(),
        ));
    }
    let locator = rovodev_structured_locator(
        source_record_ordinal,
        source_record_subrecord_index,
        native_record_id,
    )?;
    let record_sha256 =
        CompleteContentBodyDigest::parse(format!("{:x}", Sha256::digest(record_bytes))).ok_or(
            CaptureError::SystemInvariant("RovoDev SHA-256 formatting produced an invalid digest"),
        )?;
    let content_ref = ContentRef::from_bytes(complete_text.as_bytes()).ok_or(
        CaptureError::SystemInvariant("RovoDev content length exceeds ContentRef bounds"),
    )?;
    let profile = verified_content_profile(
        CaptureProvider::RovoDev,
        ROVODEV_SOURCE_FORMAT,
        CompleteContentSourceFamily::Structured,
        VerifiedContentRole::MessageBody,
    )
    .ok_or(CaptureError::SystemInvariant(
        "RovoDev message route must have a verified-content profile",
    ))?;
    let locator = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Structured,
        STRUCTURED_COMPLETE_CONTENT_LOCATOR_KIND,
        &locator,
        native_record_id,
        record_sha256,
    )
    .ok_or(CaptureError::SystemInvariant(
        "RovoDev complete-content locator exceeds its bounded schema",
    ))?;
    attach_verified_content_locator(&mut event.metadata, locator).ok_or(
        CaptureError::SystemInvariant("RovoDev verified-content locator collection is malformed"),
    )?;
    Ok(())
}

fn rovodev_structured_locator(
    ordinal: u64,
    subrecord: u32,
    native_record_id: &str,
) -> Result<Vec<u8>> {
    const MAGIC: &[u8; 4] = b"SC\0\x01";
    let provider = CaptureProvider::RovoDev.as_str().as_bytes();
    let provider_len = u8::try_from(provider.len())
        .map_err(|_| CaptureError::SystemInvariant("provider identity exceeds locator bounds"))?;
    let native_record_id = native_record_id.as_bytes();
    let native_len = u16::try_from(native_record_id.len()).map_err(|_| {
        CaptureError::InvalidPayload(
            "RovoDev complete-content native record identity is too long".to_owned(),
        )
    })?;
    let mut locator =
        Vec::with_capacity(MAGIC.len() + 1 + provider.len() + 8 + 4 + 2 + native_record_id.len());
    locator.extend_from_slice(MAGIC);
    locator.push(provider_len);
    locator.extend_from_slice(provider);
    locator.extend_from_slice(&ordinal.to_be_bytes());
    locator.extend_from_slice(&subrecord.to_be_bytes());
    locator.extend_from_slice(&native_len.to_be_bytes());
    locator.extend_from_slice(native_record_id);
    Ok(locator)
}

#[allow(clippy::too_many_arguments)]
fn publish_rejection_cursor(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    source: &RovoDevSessionSource,
    root_stream: &str,
    manifest: &mut RovoDevRootManifest,
    context: &ProviderAdapterContext,
    observation: &RovoDevSessionObservation,
    source_identity: &str,
    source_revision: &str,
    physical_identity: &str,
    stream: &str,
    expected: Option<String>,
    prior: Option<&RovoDevNativeCursor>,
    generation: u64,
    replacement: bool,
    rejection: RovoDevFailure,
) -> Result<RovoDevNativeCursor> {
    if !observation.revalidate(source)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let locator_identity = format!("{source_identity}:generation:{generation}");
    let cursor = RovoDevNativeCursor {
        version: ROVODEV_NATIVE_CURSOR_VERSION,
        provider: CaptureProvider::RovoDev.as_str().to_owned(),
        source_identity: source_identity.to_owned(),
        source_revision: source_revision.to_owned(),
        physical_identity: physical_identity.to_owned(),
        locator_identity,
        source_id: None,
        frontier: RovoDevFrontier::start(),
        terminal: true,
        missing: false,
        generation,
        accepted_sessions: 0,
        accepted_events: 0,
        accepted_file_touches: 0,
        rejected_records: 1,
        failures: vec![rejection],
    };
    let source_transition = NativePathCursorTransition::new(
        expected,
        sync_cursor(context, stream, cursor.encode()?, CaptureProvider::RovoDev),
    );
    let next_manifest = manifest_with_entry(
        manifest,
        manifest_entry_with_canonical(source, &cursor, None)?,
    );
    let root_transition =
        manifest_transition(store, context, root_stream, manifest, &next_manifest)?;
    let mut transitions = vec![source_transition];
    if let Some(transition) = root_transition {
        transitions.push(transition);
    }
    let publication_id = rejection_publication_id(source_identity, source_revision, &transitions);
    let retained_bytes = transitions
        .iter()
        .map(|transition| transition.next().cursor.len())
        .sum::<usize>()
        .saturating_add(256);
    let accounting = NativePathGroupAccounting::new(1, transitions.len(), retained_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    if matches!(
        group.classify_cursor_set(&publication_id, &transitions)?,
        NativePathCursorSetClassification::AllExpected
    ) {
        if replacement {
            if let Some(prior) = prior {
                if let Some(source_id) = prior.source_id {
                    let canonical = committed_store
                        .get_capture_source(source_id)?
                        .descriptor
                        .source_identity
                        .ok_or(CaptureError::SystemInvariant(
                            "RovoDev rejected prior source lost its canonical identity",
                        ))?;
                    group.retire_provider_source_route(&ProviderSourceRouteRetirement {
                        provider: CaptureProvider::RovoDev,
                        source_format: ROVODEV_SOURCE_FORMAT.to_owned(),
                        machine_id: context.machine_id.clone(),
                        locator_identity: prior.locator_identity.clone(),
                        cursor_stream: stream.to_owned(),
                        expected_canonical_source_identity: canonical,
                        expected_source_revision: prior.source_revision.clone(),
                        retired_at_ms: context.imported_at.timestamp_millis(),
                        reason: ProviderSourceRouteRetirementReason::Replaced,
                    })?;
                }
            }
        }
        group.prepare_journal_checkpoint()?;
        group.publish_cursor_set()?;
    }
    group.commit()?;
    *manifest = next_manifest;
    Ok(cursor)
}

fn classify_cursor(
    stored: Option<&SyncCursor>,
    source_identity: &str,
    source_revision: &str,
    physical_identity: &str,
    document: Option<&PreparedDocument>,
) -> Result<CursorPlan> {
    let Some(stored) = stored else {
        return Ok(CursorPlan::Publish {
            expected: None,
            prior: None,
            generation: 0,
            start: 0,
            replacement: false,
        });
    };
    let committed = match decode_native_path_committed_cursor(&stored.cursor) {
        Ok(committed) => committed,
        Err(_) => {
            // Released pre-NativePath cursors are decode-only migration input.
            // No new cursor is ever emitted in that format.
            if CertifiedProviderCursor::decode_if_certified(&stored.cursor)?.is_none() {
                return Err(CaptureError::InvalidPayload(
                    "RovoDev cursor is neither NativePath nor a released migration cursor"
                        .to_owned(),
                ));
            }
            return Ok(CursorPlan::Publish {
                expected: Some(stored.cursor.clone()),
                prior: None,
                generation: 0,
                start: 0,
                replacement: false,
            });
        }
    };
    let prior = RovoDevNativeCursor::decode(committed.provider_cursor())?;
    if prior.source_identity != source_identity {
        return Err(CaptureError::InvalidPayload(
            "RovoDev NativePath cursor belongs to another source".to_owned(),
        ));
    }
    if prior.source_revision == source_revision && !prior.missing {
        if prior.terminal {
            return Ok(CursorPlan::AlreadyCommitted(prior));
        }
        return Ok(CursorPlan::Publish {
            expected: Some(stored.cursor.clone()),
            start: usize::try_from(prior.frontier.next_message_index).map_err(|_| {
                CaptureError::InvalidPayload("RovoDev cursor frontier exceeds usize".to_owned())
            })?,
            generation: prior.generation,
            prior: Some(prior),
            replacement: false,
        });
    }

    let append = prior.physical_identity == physical_identity
        && document.is_some_and(|document| {
            usize::try_from(prior.frontier.next_message_index)
                .ok()
                .filter(|count| *count <= document.messages.len())
                .is_some_and(|count| {
                    frontier(&document.messages, count)
                        .ok()
                        .is_some_and(|frontier| {
                            frontier.prefix_sha256 == prior.frontier.prefix_sha256
                        })
                })
        })
        && !prior.missing;
    if append {
        let start = usize::try_from(prior.frontier.next_message_index).map_err(|_| {
            CaptureError::InvalidPayload("RovoDev cursor frontier exceeds usize".to_owned())
        })?;
        Ok(CursorPlan::Publish {
            expected: Some(stored.cursor.clone()),
            generation: prior.generation,
            prior: Some(prior),
            start,
            replacement: false,
        })
    } else {
        let generation = prior.generation.checked_add(1).ok_or_else(|| {
            CaptureError::InvalidPayload("RovoDev source generation is exhausted".to_owned())
        })?;
        Ok(CursorPlan::Publish {
            expected: Some(stored.cursor.clone()),
            prior: Some(prior),
            generation,
            start: 0,
            replacement: true,
        })
    }
}

// Cursor construction keeps all persisted identity components explicit.
#[allow(clippy::too_many_arguments)]
fn next_cursor(
    source_identity: &str,
    source_revision: &str,
    physical_identity: &str,
    locator_identity: &str,
    prior: Option<&RovoDevNativeCursor>,
    generation: u64,
    page: &PreparedPage,
    document: &PreparedDocument,
) -> Result<RovoDevNativeCursor> {
    let same_generation = prior.filter(|prior| prior.generation == generation && !prior.missing);
    let accepted_events = page
        .messages
        .iter()
        .filter(|message| message.event.is_some())
        .count();
    let accepted_file_touches = page
        .messages
        .iter()
        .map(|message| message.touches.len())
        .sum::<usize>();
    let page_rejections = page
        .messages
        .iter()
        .filter_map(|message| message.rejection.clone())
        .collect::<Vec<_>>();
    let mut failures = same_generation
        .map(|prior| prior.failures.clone())
        .unwrap_or_default();
    if page.expected_frontier.next_message_index == 0 {
        failures.extend(document.initial_failures.iter().cloned());
    }
    failures.extend(page_rejections.iter().cloned());
    failures.truncate(ROVODEV_MAX_FAILURES);
    Ok(RovoDevNativeCursor {
        version: ROVODEV_NATIVE_CURSOR_VERSION,
        provider: CaptureProvider::RovoDev.as_str().to_owned(),
        source_identity: source_identity.to_owned(),
        source_revision: source_revision.to_owned(),
        physical_identity: physical_identity.to_owned(),
        locator_identity: locator_identity.to_owned(),
        source_id: Some(native_source_id(
            source_identity,
            locator_identity,
            &document.provider_session_id,
        )),
        frontier: page.next_frontier.clone(),
        terminal: page.terminal,
        missing: false,
        generation,
        accepted_sessions: 1,
        accepted_events: same_generation
            .map_or(0, |prior| prior.accepted_events)
            .saturating_add(u64::try_from(accepted_events).unwrap_or(u64::MAX)),
        accepted_file_touches: same_generation
            .map_or(0, |prior| prior.accepted_file_touches)
            .saturating_add(u64::try_from(accepted_file_touches).unwrap_or(u64::MAX)),
        rejected_records: same_generation
            .map_or(0, |prior| prior.rejected_records)
            .saturating_add(u64::try_from(page_rejections.len()).unwrap_or(u64::MAX))
            .saturating_add(if page.expected_frontier.next_message_index == 0 {
                u64::try_from(document.initial_failures.len()).unwrap_or(u64::MAX)
            } else {
                0
            }),
        failures,
    })
}

fn replay_cursor_summary(cursor: &RovoDevNativeCursor, summary: &mut ProviderImportSummary) {
    summary.skipped_sessions = usize::try_from(cursor.accepted_sessions).unwrap_or(usize::MAX);
    summary.skipped_events = usize::try_from(cursor.accepted_events).unwrap_or(usize::MAX);
    summary.skipped = summary
        .skipped_sessions
        .saturating_add(summary.skipped_events);
    for failure in &cursor.failures {
        summary.record_failure(ProviderImportFailure {
            line: failure.line,
            error: failure.error.clone(),
        });
    }
    let rejected = usize::try_from(cursor.rejected_records).unwrap_or(usize::MAX);
    summary.failed = summary.failed.max(rejected);
    summary.set_work_result(ProviderImportWorkResult::NoOp);
}

fn replay_outputs(
    source: &RovoDevSessionSource,
    document: &PreparedDocument,
    source_identity: &str,
    cursor: &RovoDevNativeCursor,
    sink: &dyn ProOutputSink,
) -> std::result::Result<(), ProOutputSinkError> {
    let source_revision = &cursor.source_revision;
    let output_source = OutputSourceIdentity {
        provider: CaptureProvider::RovoDev.as_str().to_owned(),
        namespace_id: source_identity.to_owned(),
        source_id: document.provider_session_id.clone(),
    };
    let progress = sink.observe_source(&output_source)?;
    let mut state = output_state(
        output_source,
        progress,
        source_revision,
        sink.materializer_revision(),
        document,
        cursor.generation,
        &cursor.physical_identity,
    )?;
    let mut index = state.source_start;
    if index > document.messages.len() {
        return Err(ProOutputSinkError::new(
            "rovodev_output_cursor",
            "frontier exceeds current source",
        ));
    }
    while index < document.messages.len() || state.requires_checkpoint {
        let end = index
            .saturating_add(ROVODEV_PAGE_MAX_UNITS)
            .min(document.messages.len());
        let expected = frontier(&document.messages, index).map_err(|error| {
            ProOutputSinkError::new("rovodev_output_frontier", error.to_string())
        })?;
        let next = frontier(&document.messages, end).map_err(|error| {
            ProOutputSinkError::new("rovodev_output_frontier", error.to_string())
        })?;
        let mut observations = Vec::new();
        for message_index in index..end {
            let message = &document.messages[message_index];
            let role = message
                .get("role")
                .or_else(|| message.get("kind"))
                .or_else(|| message.get("type"))
                .and_then(Value::as_str);
            if rovodev_event_type(message, role) != EventType::ToolOutput {
                continue;
            }
            observations.push(output_observation(
                source,
                document,
                source_identity,
                source_revision,
                message,
                message_index,
            )?);
        }
        let terminal = end == document.messages.len();
        let expected_safe =
            output_safe_frontier(&expected, cursor.generation, &cursor.physical_identity).map_err(
                |error| ProOutputSinkError::new("rovodev_output_frontier", error.to_string()),
            )?;
        let next_safe = output_safe_frontier(&next, cursor.generation, &cursor.physical_identity)
            .map_err(|error| {
            ProOutputSinkError::new("rovodev_output_frontier", error.to_string())
        })?;
        let accounting = NativePageAccounting {
            logical_units: observations.len(),
            conservative_serialized_bytes: observations
                .iter()
                .map(|output| estimated_output_bytes(output).saturating_add(4 * 1024))
                .sum::<usize>()
                .saturating_add(4 * 1024),
        };
        let output = NativeProOutputPage {
            inventory_generation: sink.inventory_generation(),
            source: state.source.clone(),
            source_epoch: state.source_epoch,
            observed_revision: source_revision.to_owned(),
            parser_revision: ROVODEV_OUTPUT_PARSER_REVISION.to_owned(),
            materializer_revision: sink.materializer_revision().to_owned(),
            disposition: state.disposition,
            expected_prior_source_epoch: state.expected_source_epoch,
            expected_prior_frontier: state.expected_frontier.clone(),
            observations,
        };
        let page = NativeProReplayPage::new_with_source_identity(
            NativeSourceIdentity::new(CaptureProvider::RovoDev.as_str(), source_identity),
            expected_safe,
            next_safe.clone(),
            terminal,
            accounting,
            output,
        )
        .map_err(|error| ProOutputSinkError::new("rovodev_output_page", error.to_string()))?;
        if let Err(failure) = process_pro_replay_only(page, sink) {
            return Err(match failure.output_error {
                NativeOutputProFailure::Sink(error) => error,
                NativeOutputProFailure::ReceiptMismatch { .. } => ProOutputSinkError::new(
                    "rovodev_output_receipt",
                    "output sink acknowledgement did not match the requested cursor",
                ),
            });
        }
        state.expected_source_epoch = Some(state.source_epoch);
        state.expected_frontier = Some(next_safe);
        state.disposition = ProOutputSourceDisposition::AppendOrResume;
        state.requires_checkpoint = false;
        if terminal {
            break;
        }
        index = end;
    }
    Ok(())
}

fn output_state(
    source: OutputSourceIdentity,
    progress: Option<ProOutputProgress>,
    source_revision: &str,
    materializer_revision: &str,
    document: &PreparedDocument,
    generation: u64,
    physical_identity: &str,
) -> std::result::Result<OutputState, ProOutputSinkError> {
    let Some(progress) = progress else {
        return Ok(OutputState {
            source,
            source_epoch: 0,
            expected_source_epoch: None,
            expected_frontier: None,
            source_start: 0,
            disposition: ProOutputSourceDisposition::NewSource,
            requires_checkpoint: true,
        });
    };
    let prior_safe_frontier = progress
        .cursor
        .as_ref()
        .map(|cursor| {
            NativeSafeFrontier::new(cursor.version, cursor.payload.clone()).map_err(|error| {
                ProOutputSinkError::new("rovodev_output_cursor", error.to_string())
            })
        })
        .transpose()?;
    let prior_frontier = prior_safe_frontier
        .as_ref()
        .map(output_frontier)
        .transpose()?;
    let append = prior_frontier.as_ref().is_some_and(|prior| {
        prior.generation == generation
            && prior.physical_identity == physical_identity
            && usize::try_from(prior.next_message_index)
                .ok()
                .filter(|count| *count <= document.messages.len())
                .is_some_and(|count| {
                    frontier(&document.messages, count)
                        .ok()
                        .is_some_and(|current| current.prefix_sha256 == prior.prefix_sha256)
                })
    });
    let rewrite = progress.parser_revision != ROVODEV_OUTPUT_PARSER_REVISION
        || progress.materializer_revision != materializer_revision
        || !append;
    let requires_checkpoint = rewrite || progress.observed_revision != source_revision;
    let source_start = if rewrite {
        0
    } else {
        prior_frontier.as_ref().map_or(0, |frontier| {
            usize::try_from(frontier.next_message_index).unwrap_or(usize::MAX)
        })
    };
    Ok(OutputState {
        source,
        source_epoch: if rewrite {
            progress.source_epoch.checked_add(1).ok_or_else(|| {
                ProOutputSinkError::new("rovodev_output_epoch", "source epoch is exhausted")
            })?
        } else {
            progress.source_epoch
        },
        expected_source_epoch: Some(progress.source_epoch),
        expected_frontier: prior_safe_frontier,
        source_start,
        disposition: if rewrite {
            ProOutputSourceDisposition::Rewrite
        } else {
            ProOutputSourceDisposition::AppendOrResume
        },
        requires_checkpoint,
    })
}

fn output_observation(
    _source: &RovoDevSessionSource,
    document: &PreparedDocument,
    source_identity: &str,
    source_revision: &str,
    message: &Value,
    index: usize,
) -> std::result::Result<ProOutputObservation, ProOutputSinkError> {
    let event_index = u64::try_from(index)
        .map_err(|_| ProOutputSinkError::new("rovodev_output_index", "index exceeds u64"))?;
    let metadata = output_metadata(message, event_index, document.cwd.as_deref());
    let content = super::rovodev_result_content(message).unwrap_or_default();
    let locator_payload = serde_json::to_vec(&json!({
        "source_identity": source_identity,
        "source_revision": source_revision,
        "message_index": event_index,
    }))
    .map_err(|error| ProOutputSinkError::new("rovodev_output_locator", error.to_string()))?;
    let occurred_at = message_timestamp(message).unwrap_or(document.started_at);
    Ok(ProOutputObservation {
        kind: metadata.kind,
        coordinate: OutputNativeCoordinate {
            unit_key: metadata.native_record_id.clone(),
            native_sequence: event_index,
            native_record_id: Some(metadata.native_record_id),
            source_record_ordinal: Some(0),
            source_record_subrecord_index: u32::try_from(index).ok(),
            byte_start: None,
            byte_end_exclusive: None,
        },
        occurred_at_unix_ms: Some(occurred_at.timestamp_millis()),
        associations: OutputAssociations {
            direct_session_id: document.provider_session_id.clone(),
            root_session_id: document
                .parent_provider_session_id
                .clone()
                .unwrap_or_else(|| document.provider_session_id.clone()),
            parent_session_id: document.parent_provider_session_id.clone(),
            provider_session_id: Some(document.provider_session_id.clone()),
            agent_id: provider_string_field(&document.metadata, &["agent_id", "agentId"]),
            repository: None,
        },
        call_id: metadata.call_id,
        command: metadata.command,
        outcome: metadata.outcome,
        locator: OutputSourceLocator {
            version: 1,
            kind: ROVODEV_NATIVE_LOCATOR_KIND.to_owned(),
            payload: locator_payload,
        },
        content: content.into_bytes(),
    })
}

fn estimated_output_bytes(output: &ProOutputObservation) -> usize {
    output
        .content
        .len()
        .saturating_add(output.locator.payload.len())
        .saturating_add(output.coordinate.unit_key.len())
        .saturating_add(512)
}

fn retire_source(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    root_stream: &str,
    manifest: &RovoDevRootManifest,
    entry: &RovoDevManifestEntry,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<(RovoDevRootManifest, usize, ProviderImportSummary)> {
    let source_cursor = store
        .get_sync_cursor(None, &context.machine_id, &entry.cursor_stream)?
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "RovoDev root manifest references a missing source cursor".to_owned(),
            )
        })?;
    let committed = decode_native_path_committed_cursor(&source_cursor.cursor)?;
    let prior = RovoDevNativeCursor::decode(committed.provider_cursor())?;
    let cursor_canonical = prior
        .source_id
        .map(|source_id| {
            store
                .get_capture_source(source_id)
                .map(|source| source.descriptor.source_identity)
        })
        .transpose()?
        .flatten();
    if prior.locator_identity != entry.locator_identity
        || prior.source_revision != entry.source_revision
        || entry.canonical_source_identity.is_some()
            && entry.canonical_source_identity != cursor_canonical
    {
        return Err(CaptureError::InvalidPayload(
            "RovoDev root/source cursor authority diverged".to_owned(),
        ));
    }
    let mut next_manifest = manifest.clone();
    next_manifest
        .sources
        .retain(|source| source.source_identity != entry.source_identity);
    let root_stored = store.get_sync_cursor(None, &context.machine_id, root_stream)?;
    let root_next = sync_cursor(
        context,
        root_stream,
        serde_json::to_string(&next_manifest)
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
        CaptureProvider::RovoDev,
    );
    let root_transition = NativePathCursorTransition::new(
        root_stored.as_ref().map(|cursor| cursor.cursor.clone()),
        root_next,
    );
    let missing_cursor = RovoDevNativeCursor {
        source_revision: prior.source_revision.clone(),
        terminal: true,
        missing: true,
        generation: prior.generation.checked_add(1).ok_or_else(|| {
            CaptureError::InvalidPayload("RovoDev source generation is exhausted".to_owned())
        })?,
        ..prior.clone()
    };
    let source_transition = NativePathCursorTransition::new(
        Some(source_cursor.cursor),
        sync_cursor(
            context,
            &entry.cursor_stream,
            missing_cursor.encode()?,
            CaptureProvider::RovoDev,
        ),
    );
    let transitions = vec![source_transition, root_transition];
    let publication_id = retirement_publication_id(entry, &transitions);
    let retained_bytes = transitions
        .iter()
        .map(|transition| transition.next().cursor.len())
        .sum();
    let accounting = NativePathGroupAccounting::new(0, 2, retained_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    if matches!(
        group.classify_cursor_set(&publication_id, &transitions)?,
        NativePathCursorSetClassification::AllExpected
    ) {
        if let Some(canonical) = entry
            .canonical_source_identity
            .as_deref()
            .or(cursor_canonical.as_deref())
        {
            let disposition =
                group.retire_provider_source_route(&ProviderSourceRouteRetirement {
                    provider: CaptureProvider::RovoDev,
                    source_format: ROVODEV_SOURCE_FORMAT.to_owned(),
                    machine_id: context.machine_id.clone(),
                    locator_identity: entry.locator_identity.clone(),
                    cursor_stream: entry.cursor_stream.clone(),
                    expected_canonical_source_identity: canonical.to_owned(),
                    expected_source_revision: entry.source_revision.clone(),
                    retired_at_ms: context.imported_at.timestamp_millis(),
                    reason,
                })?;
            if disposition == ProviderSourceRouteRetirementDisposition::AlreadyRetired {
                return Err(CaptureError::InvalidPayload(
                    "RovoDev live root manifest retained an already-retired route".to_owned(),
                ));
            }
        }
        group.prepare_journal_checkpoint()?;
        group.publish_cursor_set()?;
    }
    group.commit()?;
    let mut summary = ProviderImportSummary::default();
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok((next_manifest, 1, summary))
}

fn publish_manifest(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    stream: &str,
    initial_stored: Option<&SyncCursor>,
    manifest: &RovoDevRootManifest,
) -> Result<ProviderImportSummary> {
    let stored = store.get_sync_cursor(None, &context.machine_id, stream)?;
    if initial_stored.is_some() && stored.is_none() {
        return Err(CaptureError::InvalidPayload(
            "RovoDev root cursor disappeared during import".to_owned(),
        ));
    }
    let encoded = serde_json::to_string(manifest)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    if let Some(stored) = stored.as_ref() {
        let committed = decode_native_path_committed_cursor(&stored.cursor)?;
        if committed.provider_cursor() == encoded {
            let mut summary = ProviderImportSummary::default();
            summary.set_work_result(ProviderImportWorkResult::NoOp);
            return Ok(summary);
        }
    }
    let transition = NativePathCursorTransition::new(
        stored.as_ref().map(|cursor| cursor.cursor.clone()),
        sync_cursor(context, stream, encoded, CaptureProvider::RovoDev),
    );
    let publication_id = root_publication_id(manifest, &transition);
    let accounting = NativePathGroupAccounting::new(1, 1, transition.next().cursor.len())?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllExpected
    ) {
        group.prepare_journal_checkpoint()?;
        group.publish_cursor_set()?;
    }
    group.commit()?;
    let mut summary = ProviderImportSummary::default();
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

fn load_manifest(stored: Option<&SyncCursor>, root_identity: &str) -> Result<RovoDevRootManifest> {
    let Some(stored) = stored else {
        return Ok(RovoDevRootManifest {
            version: ROVODEV_NATIVE_CURSOR_VERSION,
            root_identity: root_identity.to_owned(),
            sources: Vec::new(),
        });
    };
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let manifest: RovoDevRootManifest = serde_json::from_str(committed.provider_cursor())
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    if manifest.version != ROVODEV_NATIVE_CURSOR_VERSION
        || manifest.root_identity != root_identity
        || manifest
            .sources
            .windows(2)
            .any(|sources| sources[0].source_identity >= sources[1].source_identity)
    {
        return Err(CaptureError::InvalidPayload(
            "RovoDev NativePath root manifest is inconsistent".to_owned(),
        ));
    }
    Ok(manifest)
}

fn manifest_entry(
    store: &Store,
    source: &RovoDevSessionSource,
    cursor: &RovoDevNativeCursor,
) -> Result<RovoDevManifestEntry> {
    let canonical_source_identity = cursor
        .source_id
        .map(|source_id| store.get_capture_source(source_id))
        .transpose()?
        .and_then(|source| source.descriptor.source_identity);
    manifest_entry_with_canonical(source, cursor, canonical_source_identity)
}

fn manifest_entry_with_canonical(
    source: &RovoDevSessionSource,
    cursor: &RovoDevNativeCursor,
    canonical_source_identity: Option<String>,
) -> Result<RovoDevManifestEntry> {
    let canonical = fs::canonicalize(&source.context_path)?;
    let path_identity = provider_path_identity(&canonical)?;
    Ok(RovoDevManifestEntry {
        source_identity: cursor.source_identity.clone(),
        cursor_stream: provider_source_cursor_stream_for_path(
            CaptureProvider::RovoDev,
            ROVODEV_SOURCE_FORMAT,
            &path_identity,
        ),
        locator_identity: cursor.locator_identity.clone(),
        canonical_source_identity,
        source_revision: cursor.source_revision.clone(),
    })
}

fn manifest_with_entry(
    manifest: &RovoDevRootManifest,
    entry: RovoDevManifestEntry,
) -> RovoDevRootManifest {
    let mut next = manifest.clone();
    match next
        .sources
        .iter_mut()
        .find(|prior| prior.source_identity == entry.source_identity)
    {
        Some(prior) => *prior = entry,
        None => next.sources.push(entry),
    }
    next.sources
        .sort_by(|left, right| left.source_identity.cmp(&right.source_identity));
    next
}

fn manifest_transition(
    store: &Store,
    context: &ProviderAdapterContext,
    stream: &str,
    expected_manifest: &RovoDevRootManifest,
    next_manifest: &RovoDevRootManifest,
) -> Result<Option<NativePathCursorTransition>> {
    let expected_encoded = serde_json::to_string(expected_manifest)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let next_encoded = serde_json::to_string(next_manifest)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    if expected_encoded == next_encoded {
        return Ok(None);
    }
    let stored = store.get_sync_cursor(None, &context.machine_id, stream)?;
    match stored.as_ref() {
        Some(stored) => {
            let committed = decode_native_path_committed_cursor(&stored.cursor)?;
            if committed.provider_cursor() != expected_encoded {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
        }
        None if !expected_manifest.sources.is_empty() => {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        None => {}
    }
    Ok(Some(NativePathCursorTransition::new(
        stored.map(|cursor| cursor.cursor),
        sync_cursor(context, stream, next_encoded, CaptureProvider::RovoDev),
    )))
}

fn revalidate_discovery(path: &Path, discovery: &RovoDevDiscovery) -> Result<()> {
    let current = discover_rovodev_session_sources(path)?;
    if current.root_exists() != discovery.root_exists()
        || current.canonical_context_paths()? != discovery.canonical_context_paths()?
    {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    Ok(())
}

fn capture_source(
    id: Uuid,
    source: &RovoDevSessionSource,
    context: &ProviderAdapterContext,
    configured_source_root: &Path,
    source_revision: &str,
    canonical_source_identity: &str,
    document: &PreparedDocument,
) -> CaptureSource {
    let raw_source_path = source.context_path.display().to_string();
    let source_root = configured_source_root.display().to_string();
    CaptureSource {
        id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::RovoDev,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: document.cwd.clone(),
            raw_source_path: Some(raw_source_path.clone()),
            source_format: Some(ROVODEV_SOURCE_FORMAT.to_owned()),
            source_root: Some(source_root.clone()),
            source_identity: Some(canonical_source_identity.to_owned()),
            external_session_id: Some(document.provider_session_id.clone()),
        },
        started_at: document.started_at,
        ended_at: document.ended_at,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": document.provider_session_id,
                "source_format": ROVODEV_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "source_identity": canonical_source_identity,
                "source_root": source_root,
                "source_revision": source_revision,
                "source_identity_key": provider_scoped_source_identity_key(
                    CaptureProvider::RovoDev,
                    &document.provider_session_id,
                    ROVODEV_SOURCE_FORMAT,
                    Some(&raw_source_path),
                ),
                "nativepath_parser": ROVODEV_NATIVE_PARSER_REVISION,
                "nativepath_policy_revision": ROVODEV_NATIVE_POLICY_REVISION,
            }),
        ),
    }
}

fn canonical_session(
    committed_store: &Store,
    source_id: Uuid,
    canonical_source_identity: &str,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    document: &PreparedDocument,
) -> Result<Session> {
    let id = provider_import_session_uuid(
        committed_store,
        CaptureProvider::RovoDev,
        &document.provider_session_id,
        source_id,
        Some(canonical_source_identity),
    )?;
    let parent_session_id = document
        .parent_provider_session_id
        .as_deref()
        .map(|parent| {
            provider_import_session_uuid(
                committed_store,
                CaptureProvider::RovoDev,
                parent,
                source_id,
                Some(canonical_source_identity),
            )
        })
        .transpose()?;
    let is_primary = parent_session_id.is_none();
    Ok(Session {
        id,
        history_record_id: options.history_record_id,
        parent_session_id,
        root_session_id: parent_session_id,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::RovoDev,
        external_session_id: Some(document.provider_session_id.clone()),
        external_agent_id: provider_string_field(
            &document.metadata,
            &["agent_id", "agentId", "agent_name", "agentName"],
        ),
        agent_type: if is_primary {
            AgentType::Primary
        } else {
            AgentType::Subagent
        },
        role_hint: Some(if is_primary { "primary" } else { "subagent" }.to_owned()),
        is_primary,
        status: if document.ended_at.is_some() {
            SessionStatus::Completed
        } else {
            SessionStatus::Imported
        },
        transcript_blob_id: None,
        started_at: document.started_at,
        ended_at: document.ended_at,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": document.provider_session_id,
                "parent_provider_session_id": document.parent_provider_session_id,
                "source_format": ROVODEV_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "metadata": {
                    "title": provider_string_field(&document.metadata, &["title", "name"]),
                    "workspace_path": provider_string_field(
                        &document.metadata,
                        &["workspace_path", "workspacePath"]
                    ),
                    "message_count": document.messages.len(),
                    "metadata": document.metadata_preview,
                    "context": document.context_metadata,
                    "nativepath_parser": ROVODEV_NATIVE_PARSER_REVISION,
                },
            }),
        ),
    })
}

fn relationship_placeholder(
    id: Uuid,
    source_id: Uuid,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    external_session_id: &str,
) -> Session {
    Session {
        id,
        history_record_id: options.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::RovoDev,
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
                "source_format": ROVODEV_SOURCE_FORMAT,
                "relationship_placeholder": true,
            }),
        ),
    }
}

fn relationship_edge(
    source_id: Uuid,
    canonical_source_identity: &str,
    context: &ProviderAdapterContext,
    session: &Session,
    parent_id: Uuid,
) -> SessionEdge {
    let provider_session_id = session.external_session_id.as_deref().unwrap_or_default();
    SessionEdge {
        id: provider_source_edge_uuid(
            canonical_source_identity,
            provider_session_id,
            "parent_child",
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
                "provider_session_id": provider_session_id,
                "source_format": ROVODEV_SOURCE_FORMAT,
                "imported_at": context.imported_at,
            }),
        ),
    }
}

fn canonical_actor(session: &Session) -> CanonicalActor {
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

fn native_source_id(
    source_identity: &str,
    locator_identity: &str,
    provider_session_id: &str,
) -> Uuid {
    stable_capture_uuid(
        &format!(
            "rovodev-native-source:{source_identity}:{locator_identity}:{provider_session_id}"
        ),
        "source",
    )
}

fn sync_cursor(
    context: &ProviderAdapterContext,
    stream: &str,
    cursor: String,
    provider: CaptureProvider,
) -> SyncCursor {
    SyncCursor {
        id: stable_capture_uuid(
            &format!(
                "provider-cursor:{}:{}:{}",
                provider.as_str(),
                context.machine_id,
                stream
            ),
            "provider-sync-cursor",
        ),
        team_id: None,
        device_id: context.machine_id.clone(),
        stream: stream.to_owned(),
        cursor,
        last_synced_at: Some(context.imported_at),
        timestamps: timestamps(context.imported_at),
    }
}

fn frontier(messages: &[Value], count: usize) -> Result<RovoDevFrontier> {
    if count > messages.len() {
        return Err(CaptureError::InvalidPayload(
            "RovoDev frontier exceeds the message history".to_owned(),
        ));
    }
    Ok(RovoDevFrontier {
        version: ROVODEV_NATIVE_FRONTIER_VERSION,
        next_message_index: u64::try_from(count).map_err(|_| {
            CaptureError::InvalidPayload("RovoDev message count exceeds u64".to_owned())
        })?,
        prefix_sha256: prefix_sha256(&messages[..count]),
    })
}

fn output_safe_frontier(
    frontier: &RovoDevFrontier,
    generation: u64,
    physical_identity: &str,
) -> std::result::Result<NativeSafeFrontier, NativeIngestionPageError> {
    let output_frontier = RovoDevOutputFrontier {
        version: ROVODEV_NATIVE_FRONTIER_VERSION,
        generation,
        physical_identity: physical_identity.to_owned(),
        next_message_index: frontier.next_message_index,
        prefix_sha256: frontier.prefix_sha256,
    };
    let bytes = serde_json::to_vec(&output_frontier)
        .map_err(|_| NativeIngestionPageError::FrontierTooLarge { bytes: usize::MAX })?;
    NativeSafeFrontier::new(ROVODEV_NATIVE_FRONTIER_VERSION, bytes)
}

fn output_frontier(
    frontier: &NativeSafeFrontier,
) -> std::result::Result<RovoDevOutputFrontier, ProOutputSinkError> {
    if frontier.version != ROVODEV_NATIVE_FRONTIER_VERSION {
        return Err(ProOutputSinkError::new(
            "rovodev_output_cursor",
            "unsupported frontier version",
        ));
    }
    let decoded: RovoDevOutputFrontier = serde_json::from_slice(&frontier.bytes)
        .map_err(|error| ProOutputSinkError::new("rovodev_output_cursor", error.to_string()))?;
    if decoded.version != ROVODEV_NATIVE_FRONTIER_VERSION || decoded.physical_identity.is_empty() {
        return Err(ProOutputSinkError::new(
            "rovodev_output_cursor",
            "inconsistent frontier version",
        ));
    }
    Ok(decoded)
}

fn prefix_sha256(messages: &[Value]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(ROVODEV_PREFIX_DOMAIN);
    for message in messages {
        let bytes = serde_json::to_vec(message).unwrap_or_default();
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
    }
    digest.finalize().into()
}

fn source_revision(
    context_bytes: Option<&[u8]>,
    context_length: u64,
    metadata_source: Option<(Option<&[u8]>, u64)>,
    frozen_revision_authority: [u8; 32],
    inventory_token: Option<&str>,
) -> String {
    let mut digest = Sha256::new();
    digest.update(ROVODEV_SOURCE_REVISION_DOMAIN);
    digest.update(ROVODEV_NATIVE_PARSER_REVISION.as_bytes());
    digest.update(ROVODEV_NATIVE_POLICY_REVISION.to_be_bytes());
    digest.update(frozen_revision_authority);
    digest.update(context_length.to_be_bytes());
    match context_bytes {
        Some(context) => {
            digest.update([1]);
            digest.update(context);
        }
        None => digest.update([0]),
    }
    if let Some((metadata, metadata_length)) = metadata_source {
        digest.update([1]);
        digest.update(metadata_length.to_be_bytes());
        match metadata {
            Some(metadata) => {
                digest.update([1]);
                digest.update(metadata);
            }
            None => digest.update([0]),
        }
    } else {
        digest.update([0]);
    }
    if let Some(token) = inventory_token {
        digest.update([1]);
        digest.update((token.len() as u64).to_be_bytes());
        digest.update(token.as_bytes());
    } else {
        digest.update([0]);
    }
    format!("rovodev-nativepath-sha256:{:x}", digest.finalize())
}

fn root_identity(path: &Path) -> Result<String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    let mut digest = Sha256::new();
    digest.update(b"ctx-rovodev-root-path-v1\0");
    digest.update(format!("{:?}", normalized.as_os_str()).as_bytes());
    Ok(format!("sha256:{:x}", digest.finalize()))
}

fn publication_id(
    source_identity: &str,
    source_revision: &str,
    page: &PreparedPage,
    transitions: &[NativePathCursorTransition],
) -> String {
    let mut digest = Sha256::new();
    digest.update(ROVODEV_PUBLICATION_DOMAIN);
    digest.update(source_identity.as_bytes());
    digest.update(source_revision.as_bytes());
    digest.update(page.expected_frontier.prefix_sha256);
    digest.update(page.next_frontier.prefix_sha256);
    digest.update([u8::from(page.terminal)]);
    for transition in transitions {
        digest.update(transition.key().stream().as_bytes());
        digest.update(transition.next().cursor.as_bytes());
    }
    format!("rovodev-nativepath-v1:{:x}", digest.finalize())
}

fn rejection_publication_id(
    source_identity: &str,
    source_revision: &str,
    transitions: &[NativePathCursorTransition],
) -> String {
    let mut digest = Sha256::new();
    digest.update(ROVODEV_PUBLICATION_DOMAIN);
    digest.update(b"rejection\0");
    digest.update(source_identity.as_bytes());
    digest.update(source_revision.as_bytes());
    for transition in transitions {
        digest.update(transition.key().stream().as_bytes());
        digest.update(transition.next().cursor.as_bytes());
    }
    format!("rovodev-nativepath-rejection-v1:{:x}", digest.finalize())
}

fn root_publication_id(
    manifest: &RovoDevRootManifest,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(ROVODEV_ROOT_PUBLICATION_DOMAIN);
    digest.update(manifest.root_identity.as_bytes());
    digest.update(transition.next().cursor.as_bytes());
    format!("rovodev-nativepath-root-v1:{:x}", digest.finalize())
}

fn retirement_publication_id(
    entry: &RovoDevManifestEntry,
    transitions: &[NativePathCursorTransition],
) -> String {
    let mut digest = Sha256::new();
    digest.update(ROVODEV_RETIREMENT_PUBLICATION_DOMAIN);
    digest.update(entry.source_identity.as_bytes());
    for transition in transitions {
        digest.update(transition.key().stream().as_bytes());
        digest.update(transition.next().cursor.as_bytes());
    }
    format!("rovodev-nativepath-retire-v1:{:x}", digest.finalize())
}

#[derive(Debug)]
struct RovoDevOutputMetadata {
    kind: OutputObservationKind,
    native_record_id: String,
    call_id: Option<String>,
    command: Option<OutputCommandContext>,
    outcome: OutputOutcomeMetadata,
}

fn output_metadata(
    value: &Value,
    event_index: u64,
    session_cwd: Option<&str>,
) -> RovoDevOutputMetadata {
    let call_id = recursive_string_field(
        value,
        &[
            "call_id",
            "callId",
            "tool_call_id",
            "toolCallId",
            "tool_use_id",
            "toolUseId",
        ],
    );
    let tool_name = recursive_string_field(value, &["tool_name", "toolName", "name", "tool"])
        .unwrap_or_else(|| "tool".to_owned());
    let kind = if tool_input::is_command_tool(&tool_name.to_ascii_lowercase()) {
        OutputObservationKind::Command
    } else {
        OutputObservationKind::Tool
    };
    let command = (kind == OutputObservationKind::Command).then(|| OutputCommandContext {
        tool_name: tool_name.clone(),
        command: tool_input::command(value).unwrap_or_default(),
        working_directory: tool_input::working_directory(value)
            .or_else(|| session_cwd.map(str::to_owned)),
    });
    let timed_out = value_timed_out(value);
    let exit_code =
        i64_field(value, &["exit_code", "exitCode"]).and_then(|value| i32::try_from(value).ok());
    let duration_ms = i64_field(value, &["duration_ms", "durationMs"])
        .and_then(|value| u64::try_from(value).ok());
    let outcome = if timed_out {
        OutputOutcome::Timeout
    } else if provider_output_event_is_failure(value) {
        OutputOutcome::Failure
    } else if provider_result_outcome_evidence(EventType::ToolOutput, value).as_str()
        == Some("success")
    {
        OutputOutcome::Success
    } else {
        OutputOutcome::Unknown
    };
    RovoDevOutputMetadata {
        kind,
        native_record_id: provider_message_id(value, event_index),
        call_id,
        command,
        outcome: OutputOutcomeMetadata {
            outcome,
            exit_code,
            duration_ms,
        },
    }
}

fn recursive_string_field(value: &Value, fields: &[&str]) -> Option<String> {
    match value {
        Value::Array(values) => values
            .iter()
            .find_map(|value| recursive_string_field(value, fields)),
        Value::Object(values) => fields
            .iter()
            .find_map(|field| values.get(*field).and_then(Value::as_str))
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
            .or_else(|| {
                values
                    .values()
                    .find_map(|value| recursive_string_field(value, fields))
            }),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => None,
    }
}

fn value_timed_out(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(value_timed_out),
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
            }) || values.values().any(value_timed_out)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

fn i64_field(value: &Value, fields: &[&str]) -> Option<i64> {
    match value {
        Value::Array(values) => values.iter().find_map(|value| i64_field(value, fields)),
        Value::Object(values) => fields
            .iter()
            .find_map(|field| values.get(*field).and_then(Value::as_i64))
            .or_else(|| values.values().find_map(|value| i64_field(value, fields))),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => None,
    }
}

fn message_history(value: &Value) -> Option<&Vec<Value>> {
    value
        .get("message_history")
        .or_else(|| value.pointer("/session_context/message_history"))
        .or_else(|| value.get("messages"))
        .or_else(|| value.pointer("/conversation/messages"))
        .and_then(Value::as_array)
}

fn message_timestamp(value: &Value) -> Option<DateTime<Utc>> {
    provider_timestamp_from_fields(
        value,
        &[
            "timestamp",
            "created_at",
            "createdAt",
            "updated_at",
            "updatedAt",
            "user_sent_time",
        ],
    )
}

#[derive(Debug, Clone, Copy)]
enum JsonBoundsError {
    Depth,
    CollectionElements,
}

impl std::fmt::Display for JsonBoundsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Depth => write!(
                formatter,
                "exceeds maximum JSON depth of {ROVODEV_MAX_JSON_DEPTH}"
            ),
            Self::CollectionElements => write!(
                formatter,
                "exceeds JSON collection element budget of {ROVODEV_NATIVE_MAX_COLLECTION_ELEMENTS}"
            ),
        }
    }
}

fn validate_json_bounds(value: &Value) -> std::result::Result<(), JsonBoundsError> {
    let mut stack = vec![(value, 0_usize)];
    let mut collection_elements = 0_usize;
    while let Some((value, depth)) = stack.pop() {
        if depth > ROVODEV_MAX_JSON_DEPTH {
            return Err(JsonBoundsError::Depth);
        }
        match value {
            Value::Array(values) => {
                collection_elements = collection_elements.saturating_add(values.len());
                if collection_elements > ROVODEV_NATIVE_MAX_COLLECTION_ELEMENTS {
                    return Err(JsonBoundsError::CollectionElements);
                }
                stack.extend(values.iter().map(|value| (value, depth.saturating_add(1))));
            }
            Value::Object(values) => {
                collection_elements = collection_elements.saturating_add(values.len());
                if collection_elements > ROVODEV_NATIVE_MAX_COLLECTION_ELEMENTS {
                    return Err(JsonBoundsError::CollectionElements);
                }
                stack.extend(
                    values
                        .values()
                        .map(|value| (value, depth.saturating_add(1))),
                );
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
    Ok(())
}

fn metadata_without_transcripts(value: &Value) -> Value {
    fn strip(value: &Value) -> Value {
        match value {
            Value::Array(values) => Value::Array(values.iter().map(strip).collect()),
            Value::Object(values) => Value::Object(
                values
                    .iter()
                    .filter(|(key, value)| {
                        !(value.is_array()
                            && matches!(key.as_str(), "message_history" | "messages"))
                    })
                    .map(|(key, value)| (key.clone(), strip(value)))
                    .collect(),
            ),
            _ => value.clone(),
        }
    }
    provider_capped_json_value(&strip(value), PROVIDER_MAX_PREVIEW_CHARS)
}

fn failure(line: usize, error: impl Into<String>) -> RovoDevFailure {
    let mut error = error.into();
    if error.len() > ROVODEV_MAX_FAILURE_BYTES {
        let mut boundary = ROVODEV_MAX_FAILURE_BYTES;
        while !error.is_char_boundary(boundary) {
            boundary = boundary.saturating_sub(1);
        }
        error.truncate(boundary);
    }
    RovoDevFailure { line, error }
}
