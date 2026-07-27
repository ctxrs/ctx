//! Production Mux NativePath ingestion.
//!
//! Mux owns discovery, source certification, parsing, identity, privacy, cursor,
//! and lifecycle policy here. Only certified Core mutations cross the typed
//! NativePath Store surface; successful output bytes are emitted solely through
//! the independent Pro replay lane.

use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    Confidence, Event, Fidelity, FileChangeKind, FileTouched, Session, SessionEdge,
    SessionEdgeType, SessionStatus, SyncCursor,
};
use ctx_history_store::{
    decode_native_path_committed_cursor, CanonicalActor, EventSearchBulkGuard,
    NativePathCursorSetClassification, NativePathCursorTransition, NativePathGroupAccounting,
    ProviderEventHashAuthority, ProviderSourceLocatorObservation, ProviderSourceRouteRetirement,
    ProviderSourceRouteRetirementReason, Store,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    provider::{
        file_touches::{
            event_type_supports_structured_file_touches,
            visit_provider_file_touch_drafts_with_limit, MAX_PACKED_PROVIDER_EVENT_INDEX,
            PROVIDER_FILE_TOUCH_LIMIT_REJECTION,
        },
        importer::{
            avoid_provider_source_event_seq_collision, compact_provider_result_payload,
            provider_file_touch_import_id, provider_path_identity,
            provider_source_cursor_stream_for_path, provider_source_event_import_identity,
            provider_source_identity, provider_source_root, provider_source_session_uuid,
            provider_sync_metadata, timestamps,
        },
        native_ingestion::{
            process_pro_replay_only, NativePageAccounting, NativeProOutputPage,
            NativeProReplayPage, NativeSafeFrontier,
        },
        providers::native_jsonl::native_jsonl_missing_reason,
    },
    stable_capture_uuid, CaptureError, CaptureWorkLimit, OutputAssociations,
    OutputNativeCoordinate, OutputObservationKind, OutputOutcome, OutputOutcomeMetadata,
    OutputSourceIdentity, OutputSourceLocator, ProOutputObservation, ProOutputSink,
    ProOutputSourceDisposition, ProviderAdapterContext, ProviderImportFailure,
    ProviderImportOptions, ProviderImportSummary, ProviderImportWorkResult, Result,
    MAX_PROVIDER_JSONL_LINE_BYTES, MUX_SOURCE_FORMAT,
};

use super::{
    metadata::{
        bounded_mux_failure, bounded_mux_id, mux_bounded_session_metadata,
        MuxBoundedSessionMetadata,
    },
    normalization::{
        apply_mux_core_output_diagnostic, mux_core_event, mux_event_type, mux_message_model,
        mux_message_timestamp_opt, mux_output_projection, mux_partial_event_index,
        mux_result_content, MuxCoreEvent, MuxMessageRow, MuxOutputOutcome,
    },
    source::{visit_mux_session_sources, MuxFileObservation, MuxSessionSource},
    MUX_CAPTURE_REVISION, MUX_POLICY_REVISION,
};

const MUX_CURSOR_VERSION: u32 = 1;
const MUX_FRONTIER_VERSION: u32 = 1;
const MUX_ROOT_MANIFEST_VERSION: u32 = 1;
const MUX_PAGE_MAX_RECORDS: usize = 8;
const MUX_PAGE_MAX_BYTES: usize = 4 * 1024 * 1024;
const MUX_MAX_FILE_TOUCHES_PER_EVENT: usize = 448;
const MUX_OUTPUT_PARSER_REVISION: &str = "mux-nativepath-output-v1";
const MUX_PUBLICATION_PREFIX: &str = "mux-nativepath-v1:";
const MUX_PARTIAL_NATIVE_ORDINAL: u64 = 1_u64 << 63;
const MUX_GENERATION_BITS: u32 = 16;
const MUX_ORDINAL_BITS: u32 = 47;
const MUX_MAX_GENERATION: u64 = (1_u64 << MUX_GENERATION_BITS) - 1;
const MUX_MAX_ORDINAL: u64 = (1_u64 << MUX_ORDINAL_BITS) - 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MuxStreamKind {
    Chat,
    Partial,
}

impl MuxStreamKind {
    fn label(self) -> &'static str {
        match self {
            Self::Chat => "chat-jsonl",
            Self::Partial => "partial-json",
        }
    }

    fn is_partial(self) -> bool {
        self == Self::Partial
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MuxFrontier {
    version: u32,
    next_offset: u64,
    next_ordinal: u64,
    prefix_sha256: [u8; 32],
    file_identity: Option<String>,
}

impl MuxFrontier {
    fn initial() -> Self {
        Self {
            version: MUX_FRONTIER_VERSION,
            next_offset: 0,
            next_ordinal: 0,
            prefix_sha256: Sha256::digest([]).into(),
            file_identity: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MuxCursorWire {
    version: u32,
    capture_revision: u32,
    policy_revision: u32,
    kind: MuxStreamKind,
    canonical_path: PathBuf,
    source_revision: String,
    metadata_revision: String,
    generation: u64,
    frontier: MuxFrontier,
    terminal: bool,
    retired: bool,
    accepted_events: u64,
    rejected_records: u64,
    first_failure: Option<MuxFailureWire>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MuxFailureWire {
    line: usize,
    error: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MuxRootManifest {
    version: u32,
    configured_root: PathBuf,
    sources: Vec<MuxManifestSource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MuxManifestSource {
    path: PathBuf,
    kind: MuxStreamKind,
    cursor_stream: String,
    locator_identity: String,
    canonical_source_identity: String,
    source_revision: String,
}

#[derive(Debug)]
struct MuxPreparedRow {
    line_number: usize,
    native_ordinal: u64,
    event: Option<MuxCoreEvent>,
    event_hash: Option<String>,
    file_touches: Vec<MuxFileTouch>,
}

#[derive(Debug)]
struct MuxFileTouch {
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

#[derive(Debug)]
struct MuxPreparedPage {
    rows: Vec<MuxPreparedRow>,
    expected: MuxFrontier,
    next: MuxFrontier,
    terminal: bool,
    deferred_incomplete: bool,
    source_bytes: usize,
    previous_rejected_records: u64,
    rejected_records: u64,
    first_failure: Option<MuxFailureWire>,
}

#[derive(Debug)]
struct MuxLoadedCursor {
    stored: SyncCursor,
    wire: Option<MuxCursorWire>,
}

#[derive(Debug)]
struct MuxSourcePlan {
    source: MuxSessionSource,
    path: PathBuf,
    kind: MuxStreamKind,
    observation: MuxFileObservation,
    path_identity: String,
    cursor_stream: String,
    canonical_source_identity: String,
    source_revision: String,
    metadata_revision: String,
    prior: Option<MuxLoadedCursor>,
    generation: u64,
    initial_frontier: MuxFrontier,
    accepted_events: u64,
    rejected_records: u64,
    first_failure: Option<MuxFailureWire>,
}

pub(crate) fn import_mux_native_path(
    path: &Path,
    store: &mut Store,
    mut context: ProviderAdapterContext,
    options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    if context.source_path.is_none() {
        context.source_path = Some(path.to_path_buf());
    }
    ensure_active_journal(store)?;
    let configured_root = context
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let prior_manifest = load_root_manifest(store, &context.machine_id, &configured_root)?;
    let mut sessions = discover_sessions(path)?;
    sessions.sort_by(|left, right| left.session_dir.cmp(&right.session_dir));
    if sessions.is_empty() && prior_manifest.is_none() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: native_jsonl_missing_reason(CaptureProvider::Mux),
        });
    }

    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let mut summary = ProviderImportSummary::default();
        let replay_only = options.import_profile.is_replay_only();
        let mut manifest_sources = Vec::new();
        let mut changed_groups = 0_usize;

        for session in sessions {
            for (kind, source_path) in [
                (MuxStreamKind::Chat, session.chat_path.clone()),
                (MuxStreamKind::Partial, session.partial_path.clone()),
            ] {
                let Some(source_path) = source_path else {
                    continue;
                };
                let plan = plan_source(
                    store,
                    &configured_root,
                    session.clone(),
                    source_path,
                    kind,
                    &context,
                )?;
                manifest_sources.push(plan.manifest_source());
                let core_output_ready = if replay_only {
                    verify_terminal_core(store, &context.machine_id, &plan)?;
                    true
                } else {
                    let source_summary = import_core_source(
                        store,
                        &bulk_guard,
                        &configured_root,
                        &context,
                        &options,
                        &plan,
                    )?;
                    let core_output_ready = !source_summary.work_remaining;
                    if source_summary.work_result() == ProviderImportWorkResult::Changed {
                        changed_groups = changed_groups.saturating_add(1);
                    }
                    summary.merge_from(source_summary);
                    core_output_ready
                };
                if core_output_ready {
                    if let Some(sink) = options.import_profile.sink() {
                        if let Err(error) = replay_source_outputs(&plan, &context, sink.as_ref()) {
                            sink.mark_behind(crate::ProOutputSinkError::new(
                                "mux_output_replay",
                                error.to_string(),
                            ));
                            summary.record_failure(ProviderImportFailure {
                                line: 0,
                                error: "Mux Pro output replay is behind Core".to_owned(),
                            });
                        }
                    }
                }
                if !replay_only
                    && options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
                    && changed_groups != 0
                {
                    summary.work_remaining = true;
                    return Ok(summary);
                }
            }
        }

        if !replay_only {
            manifest_sources.sort_by(|left, right| {
                left.path
                    .cmp(&right.path)
                    .then_with(|| stream_kind_rank(left.kind).cmp(&stream_kind_rank(right.kind)))
            });
            if let Some(prior) = prior_manifest.as_ref() {
                retire_missing_sources(
                    store,
                    &bulk_guard,
                    &context,
                    prior,
                    &manifest_sources,
                    &mut summary,
                )?;
            }
            let manifest = MuxRootManifest {
                version: MUX_ROOT_MANIFEST_VERSION,
                configured_root,
                sources: manifest_sources,
            };
            summary.merge_from(publish_root_manifest(
                store,
                &bulk_guard,
                &context,
                manifest,
            )?);
        }
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

impl MuxSourcePlan {
    fn manifest_source(&self) -> MuxManifestSource {
        MuxManifestSource {
            path: self.observation.canonical_path.clone(),
            kind: self.kind,
            cursor_stream: self.cursor_stream.clone(),
            locator_identity: self.path_identity.clone(),
            canonical_source_identity: self.canonical_source_identity.clone(),
            source_revision: self.source_revision.clone(),
        }
    }
}

fn stream_kind_rank(kind: MuxStreamKind) -> u8 {
    match kind {
        MuxStreamKind::Chat => 0,
        MuxStreamKind::Partial => 1,
    }
}

fn discover_sessions(path: &Path) -> Result<Vec<MuxSessionSource>> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    let mut sessions = Vec::new();
    visit_mux_session_sources(path, &mut |source| {
        sessions.push(source);
        Ok(())
    })?;
    Ok(sessions)
}

fn plan_source(
    store: &Store,
    configured_root: &Path,
    source: MuxSessionSource,
    path: PathBuf,
    kind: MuxStreamKind,
    context: &ProviderAdapterContext,
) -> Result<MuxSourcePlan> {
    let observation = MuxFileObservation::read(&path, source.metadata_path.as_deref())?;
    let path_identity = provider_path_identity(&observation.canonical_path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Mux,
        MUX_SOURCE_FORMAT,
        &path_identity,
    );
    let canonical_source_identity = mux_canonical_source_identity(configured_root, &path_identity);
    let source_revision = observation.source_revision(kind.label());
    let metadata_revision = observation.metadata_revision();
    let prior = load_source_cursor(store, &context.machine_id, &cursor_stream)?;
    let mut generation = 0;
    let mut initial_frontier = MuxFrontier::initial();
    let mut accepted_events = 0;
    let mut rejected_records = 0;
    let mut first_failure = None;
    if let Some(loaded) = prior.as_ref() {
        if let Some(wire) = loaded.wire.as_ref() {
            if wire.version != MUX_CURSOR_VERSION
                || wire.capture_revision != MUX_CAPTURE_REVISION
                || wire.policy_revision != MUX_POLICY_REVISION
                || wire.kind != kind
                || wire.canonical_path != observation.canonical_path
                || wire.frontier.version != MUX_FRONTIER_VERSION
            {
                return Err(CaptureError::InvalidPayload(
                    "Mux NativePath cursor identity is inconsistent".to_owned(),
                ));
            }
            generation = wire.generation;
            if !wire.retired
                && wire.source_revision == source_revision
                && prefix_matches(&path, &observation, &wire.frontier)?
            {
                initial_frontier = wire.frontier.clone();
                accepted_events = wire.accepted_events;
                rejected_records = wire.rejected_records;
                first_failure.clone_from(&wire.first_failure);
            } else if !wire.retired
                && kind == MuxStreamKind::Chat
                && wire.metadata_revision == metadata_revision
                && prefix_matches(&path, &observation, &wire.frontier)?
            {
                initial_frontier = wire.frontier.clone();
                accepted_events = wire.accepted_events;
                rejected_records = wire.rejected_records;
                first_failure.clone_from(&wire.first_failure);
            } else {
                generation = generation
                    .checked_add(1)
                    .ok_or(CaptureError::InvalidPayload(
                        "Mux NativePath source generation is exhausted".to_owned(),
                    ))?;
            }
        }
    }
    Ok(MuxSourcePlan {
        source,
        path,
        kind,
        observation,
        path_identity,
        cursor_stream,
        canonical_source_identity,
        source_revision,
        metadata_revision,
        prior,
        generation,
        initial_frontier,
        accepted_events,
        rejected_records,
        first_failure,
    })
}

fn load_source_cursor(
    store: &Store,
    machine_id: &str,
    stream: &str,
) -> Result<Option<MuxLoadedCursor>> {
    let Some(stored) = store.get_sync_cursor(None, machine_id, stream)? else {
        return Ok(None);
    };
    let wire = match decode_native_path_committed_cursor(&stored.cursor) {
        Ok(committed) => Some(
            serde_json::from_str::<MuxCursorWire>(committed.provider_cursor()).map_err(|_| {
                CaptureError::InvalidPayload("Mux NativePath cursor is corrupt".to_owned())
            })?,
        ),
        Err(_) => {
            // Released pre-NativePath cursors are accepted only as a migration
            // signal. Their parser position is never resumed by NativePath.
            match crate::provider::importer::CertifiedProviderCursor::decode_if_certified(
                &stored.cursor,
            )? {
                Some(_) => None,
                None => {
                    return Err(CaptureError::InvalidPayload(
                        "Mux cursor is neither NativePath nor a released migration cursor"
                            .to_owned(),
                    ));
                }
            }
        }
    };
    Ok(Some(MuxLoadedCursor { stored, wire }))
}

fn prefix_matches(
    path: &Path,
    observation: &MuxFileObservation,
    frontier: &MuxFrontier,
) -> Result<bool> {
    let content_identity = observation.content_identity();
    if frontier.file_identity.as_deref() != Some(content_identity.as_str()) {
        return Ok(false);
    }
    let metadata = fs::metadata(path)?;
    if metadata.len() < frontier.next_offset {
        return Ok(false);
    }
    let mut file = File::open(path)?;
    let mut remaining = frontier.next_offset;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    while remaining != 0 {
        let take = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| CaptureError::SystemInvariant("Mux prefix size exceeds usize"))?;
        let read = file.read(&mut buffer[..take])?;
        if read == 0 {
            return Ok(false);
        }
        hasher.update(&buffer[..read]);
        remaining = remaining.saturating_sub(read as u64);
    }
    Ok(<[u8; 32]>::from(hasher.finalize()) == frontier.prefix_sha256)
}

fn mux_canonical_source_identity(configured_root: &Path, path_identity: &str) -> String {
    let key = format!(
        "{}\0{}\0{}",
        CaptureProvider::Mux.as_str(),
        configured_root.display(),
        path_identity
    );
    format!(
        "mux-nativepath:{}",
        stable_capture_uuid(&key, "canonical-source")
    )
}

fn import_core_source(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    configured_root: &Path,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    plan: &MuxSourcePlan,
) -> Result<ProviderImportSummary> {
    if plan
        .prior
        .as_ref()
        .and_then(|prior| prior.wire.as_ref())
        .is_some_and(|wire| {
            wire.terminal
                && !wire.retired
                && wire.source_revision == plan.source_revision
                && wire.metadata_revision == plan.metadata_revision
        })
    {
        if !plan
            .observation
            .revalidate(&plan.path, plan.source.metadata_path.as_deref())?
        {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let wire = plan
            .prior
            .as_ref()
            .and_then(|prior| prior.wire.as_ref())
            .ok_or(CaptureError::SystemInvariant(
                "Mux replay lost its committed cursor",
            ))?;
        return Ok(replay_summary(wire));
    }

    let mut session =
        mux_bounded_session_metadata(&plan.source, &plan.metadata_revision, context.imported_at)?;
    let mut summary = ProviderImportSummary::default();
    let (mut reader, mut hasher) = open_reader_at_frontier(&plan.path, &plan.initial_frontier)?;
    let mut frontier = plan.initial_frontier.clone();
    let mut expected_store_cursor = plan.prior.as_ref().map(|prior| prior.stored.clone());
    let mut accepted_events = plan.accepted_events;
    let mut rejected_records = plan.rejected_records;
    let mut first_failure = plan.first_failure.clone();
    let mut emitted_page = false;

    loop {
        let page = read_core_page(
            &mut reader,
            &mut hasher,
            &mut session,
            plan,
            frontier.clone(),
            rejected_records,
            first_failure.clone(),
            context,
        )?;
        let Some(page) = page else {
            break;
        };
        if page.deferred_incomplete
            && plan
                .prior
                .as_ref()
                .and_then(|prior| prior.wire.as_ref())
                .is_some_and(|wire| {
                    !wire.terminal
                        && !wire.retired
                        && wire.source_revision == plan.source_revision
                        && wire.metadata_revision == plan.metadata_revision
                        && wire.generation == plan.generation
                        && wire.frontier == page.next
                })
        {
            summary.skipped = summary.skipped.saturating_add(1);
            summary.work_remaining = true;
            return Ok(summary);
        }
        emitted_page = true;
        rejected_records = page.rejected_records;
        first_failure.clone_from(&page.first_failure);
        let page_events = page.rows.iter().filter(|row| row.event.is_some()).count();
        accepted_events = accepted_events.saturating_add(
            u64::try_from(page_events)
                .map_err(|_| CaptureError::SystemInvariant("Mux event count exceeds u64"))?,
        );
        let page_summary = publish_core_page(
            store,
            bulk_guard,
            configured_root,
            context,
            options,
            plan,
            &session,
            &page,
            accepted_events,
            expected_store_cursor.as_ref(),
        )?;
        summary.merge_from(page_summary);
        frontier = page.next;
        expected_store_cursor =
            store.get_sync_cursor(None, &context.machine_id, &plan.cursor_stream)?;
        if page.terminal {
            break;
        }
        if page.deferred_incomplete {
            summary.work_remaining = true;
            break;
        }
        if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup {
            summary.work_remaining = true;
            break;
        }
    }

    if !emitted_page {
        return Err(CaptureError::SystemInvariant(
            "Mux changed source emitted no terminal authority page",
        ));
    }
    if summary.failed == 0 && rejected_records > plan.rejected_records {
        summary.failed =
            usize::try_from(rejected_records - plan.rejected_records).unwrap_or(usize::MAX);
    }
    if let Some(failure) = first_failure {
        if summary.failures.is_empty() {
            summary.failures.push(ProviderImportFailure {
                line: failure.line,
                error: failure.error,
            });
        }
    }
    Ok(summary)
}

fn replay_summary(wire: &MuxCursorWire) -> ProviderImportSummary {
    let skipped_events = usize::try_from(wire.accepted_events).unwrap_or(usize::MAX);
    let failed = usize::try_from(wire.rejected_records).unwrap_or(usize::MAX);
    ProviderImportSummary {
        skipped: skipped_events.saturating_add(1),
        failed,
        skipped_sessions: 1,
        skipped_events,
        accepted_content_records: skipped_events,
        failures: wire
            .first_failure
            .iter()
            .map(|failure| ProviderImportFailure {
                line: failure.line,
                error: failure.error.clone(),
            })
            .collect(),
        ..ProviderImportSummary::default()
    }
}

fn open_reader_at_frontier(
    path: &Path,
    frontier: &MuxFrontier,
) -> Result<(BufReader<File>, Sha256)> {
    let mut file = File::open(path)?;
    let mut remaining = frontier.next_offset;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    while remaining != 0 {
        let take = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| CaptureError::SystemInvariant("Mux prefix size exceeds usize"))?;
        let read = file.read(&mut buffer[..take])?;
        if read == 0 {
            return Err(CaptureError::InvalidPayload(
                "Mux cursor frontier exceeds its source".to_owned(),
            ));
        }
        hasher.update(&buffer[..read]);
        remaining = remaining.saturating_sub(read as u64);
    }
    if <[u8; 32]>::from(hasher.clone().finalize()) != frontier.prefix_sha256 {
        return Err(CaptureError::InvalidPayload(
            "Mux cursor prefix no longer matches its source".to_owned(),
        ));
    }
    file.seek(SeekFrom::Start(frontier.next_offset))?;
    Ok((BufReader::new(file), hasher))
}

#[allow(clippy::too_many_arguments)]
fn read_core_page(
    reader: &mut BufReader<File>,
    hasher: &mut Sha256,
    session: &mut MuxBoundedSessionMetadata,
    plan: &MuxSourcePlan,
    expected: MuxFrontier,
    mut rejected_records: u64,
    mut first_failure: Option<MuxFailureWire>,
    context: &ProviderAdapterContext,
) -> Result<Option<MuxPreparedPage>> {
    let previous_rejected_records = rejected_records;
    let mut rows = Vec::new();
    let mut source_bytes = 0_usize;
    let mut physical_records = 0_usize;
    let mut offset = expected.next_offset;
    let mut ordinal = expected.next_ordinal;
    let mut metadata_failure = if expected.next_offset == 0 {
        session.metadata_failure.take()
    } else {
        session.metadata_failure = None;
        None
    };
    let mut deferred_incomplete = false;
    let max_records = if plan.kind == MuxStreamKind::Partial {
        1
    } else {
        MUX_PAGE_MAX_RECORDS
    };

    while physical_records < max_records && source_bytes < MUX_PAGE_MAX_BYTES {
        let record_hasher = hasher.clone();
        let record = if plan.kind == MuxStreamKind::Partial {
            read_bounded_whole_record(reader, hasher, offset)?
        } else {
            read_bounded_record(reader, hasher, offset)?
        };
        let Some(record) = record else {
            break;
        };
        let rejected_before_record = rejected_records;
        let failure_before_record = first_failure.clone();
        let metadata_failure_for_record = metadata_failure.take();
        if plan.kind == MuxStreamKind::Partial && ordinal != 0 {
            return Err(CaptureError::InvalidPayload(
                "Mux partial cursor exceeds its one-record source".to_owned(),
            ));
        }
        offset = record.end;
        source_bytes = source_bytes.saturating_add(record.observed_bytes);
        physical_records = physical_records.saturating_add(1);
        let line_number = usize::try_from(ordinal)
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or(CaptureError::SystemInvariant(
                "Mux source ordinal exceeds platform limits",
            ))?;
        if let Some(error) = metadata_failure_for_record.as_ref() {
            record_rejection(
                line_number,
                error.clone(),
                &mut rejected_records,
                &mut first_failure,
            )?;
        }
        if record.oversized {
            record_rejection(
                line_number,
                format!(
                    "provider record exceeds the {} byte limit (observed {} bytes)",
                    MAX_PROVIDER_JSONL_LINE_BYTES, record.observed_bytes
                ),
                &mut rejected_records,
                &mut first_failure,
            )?;
            ordinal = ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                "Mux source ordinal overflowed",
            ))?;
            continue;
        }
        if record.payload.iter().all(u8::is_ascii_whitespace) {
            ordinal = ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                "Mux source ordinal overflowed",
            ))?;
            continue;
        }
        let value = match serde_json::from_slice::<Value>(&record.payload) {
            Ok(value) if value.is_object() => value,
            Ok(_) => {
                record_rejection(
                    line_number,
                    "Mux record must contain a JSON object".to_owned(),
                    &mut rejected_records,
                    &mut first_failure,
                )?;
                ordinal = ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                    "Mux source ordinal overflowed",
                ))?;
                continue;
            }
            Err(error) => {
                if !record.terminated {
                    reader.seek(SeekFrom::Start(record.start))?;
                    *hasher = record_hasher;
                    offset = record.start;
                    source_bytes = source_bytes.saturating_sub(record.observed_bytes);
                    physical_records = physical_records.saturating_sub(1);
                    rejected_records = rejected_before_record;
                    first_failure = failure_before_record;
                    metadata_failure = metadata_failure_for_record;
                    deferred_incomplete = true;
                    break;
                }
                record_rejection(
                    line_number,
                    format!("malformed Mux JSON record: {error}"),
                    &mut rejected_records,
                    &mut first_failure,
                )?;
                ordinal = ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
                    "Mux source ordinal overflowed",
                ))?;
                continue;
            }
        };
        if let Some(provider_session_id) = value
            .get("workspaceId")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
        {
            session.provider_session_id =
                bounded_mux_id(provider_session_id.to_owned(), &plan.path, "workspace id")?;
        }
        let row = prepare_core_row(
            value,
            &record,
            ordinal,
            line_number,
            session,
            plan,
            context,
            &mut rejected_records,
            &mut first_failure,
        )?;
        rows.push(row);
        ordinal = ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
            "Mux source ordinal overflowed",
        ))?;
        if plan.kind == MuxStreamKind::Partial {
            break;
        }
    }
    session.metadata_failure = metadata_failure;
    let terminal = reader.fill_buf()?.is_empty();
    if physical_records == 0 && !terminal && !deferred_incomplete {
        return Err(CaptureError::SystemInvariant(
            "Mux page reader made no progress",
        ));
    }
    let next = MuxFrontier {
        version: MUX_FRONTIER_VERSION,
        next_offset: offset,
        next_ordinal: ordinal,
        prefix_sha256: hasher.clone().finalize().into(),
        file_identity: Some(plan.observation.content_identity()),
    };
    Ok(Some(MuxPreparedPage {
        rows,
        expected,
        next,
        terminal,
        deferred_incomplete,
        source_bytes,
        previous_rejected_records,
        rejected_records,
        first_failure,
    }))
}

struct MuxRawRecord {
    payload: Vec<u8>,
    start: u64,
    end: u64,
    observed_bytes: usize,
    oversized: bool,
    terminated: bool,
}

fn read_bounded_record(
    reader: &mut BufReader<File>,
    hasher: &mut Sha256,
    start: u64,
) -> Result<Option<MuxRawRecord>> {
    let mut payload = Vec::new();
    let mut observed = 0_usize;
    let mut saw_any = false;
    let mut ended = false;
    while !ended {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            break;
        }
        saw_any = true;
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index.saturating_add(1));
        let chunk = &available[..take];
        hasher.update(chunk);
        observed = observed.saturating_add(chunk.len());
        if payload.len() <= MAX_PROVIDER_JSONL_LINE_BYTES {
            let remaining = MAX_PROVIDER_JSONL_LINE_BYTES
                .saturating_add(1)
                .saturating_sub(payload.len());
            payload.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        }
        ended = chunk.last() == Some(&b'\n');
        reader.consume(take);
    }
    if !saw_any {
        return Ok(None);
    }
    if payload.last() == Some(&b'\n') {
        payload.pop();
        if payload.last() == Some(&b'\r') {
            payload.pop();
        }
    }
    let end = start
        .checked_add(
            u64::try_from(observed)
                .map_err(|_| CaptureError::SystemInvariant("Mux record size exceeds u64"))?,
        )
        .ok_or(CaptureError::SystemInvariant(
            "Mux source offset overflowed",
        ))?;
    Ok(Some(MuxRawRecord {
        oversized: observed > MAX_PROVIDER_JSONL_LINE_BYTES,
        terminated: ended,
        payload,
        start,
        end,
        observed_bytes: observed,
    }))
}

fn read_bounded_whole_record(
    reader: &mut BufReader<File>,
    hasher: &mut Sha256,
    start: u64,
) -> Result<Option<MuxRawRecord>> {
    let mut payload = Vec::new();
    let mut observed = 0_usize;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            break;
        }
        let take = available.len();
        hasher.update(available);
        observed = observed.saturating_add(take);
        if payload.len() <= MAX_PROVIDER_JSONL_LINE_BYTES {
            let remaining = MAX_PROVIDER_JSONL_LINE_BYTES
                .saturating_add(1)
                .saturating_sub(payload.len());
            payload.extend_from_slice(&available[..take.min(remaining)]);
        }
        reader.consume(take);
    }
    if observed == 0 {
        return Ok(None);
    }
    let end = start
        .checked_add(
            u64::try_from(observed)
                .map_err(|_| CaptureError::SystemInvariant("Mux partial size exceeds u64"))?,
        )
        .ok_or(CaptureError::SystemInvariant(
            "Mux partial offset overflowed",
        ))?;
    Ok(Some(MuxRawRecord {
        payload,
        start,
        end,
        observed_bytes: observed,
        oversized: observed > MAX_PROVIDER_JSONL_LINE_BYTES,
        terminated: true,
    }))
}

fn record_rejection(
    line: usize,
    error: String,
    rejected: &mut u64,
    first_failure: &mut Option<MuxFailureWire>,
) -> Result<()> {
    *rejected = rejected
        .checked_add(1)
        .ok_or(CaptureError::SystemInvariant(
            "Mux rejection count overflowed",
        ))?;
    if first_failure.is_none() {
        *first_failure = Some(MuxFailureWire {
            line,
            error: bounded_mux_failure(error),
        });
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn prepare_core_row(
    value: Value,
    record: &MuxRawRecord,
    ordinal: u64,
    line_number: usize,
    session: &MuxBoundedSessionMetadata,
    plan: &MuxSourcePlan,
    context: &ProviderAdapterContext,
    rejected_records: &mut u64,
    first_failure: &mut Option<MuxFailureWire>,
) -> Result<MuxPreparedRow> {
    let started_at = session
        .started_at
        .parse::<DateTime<Utc>>()
        .map_err(|_| CaptureError::InvalidPayload("Mux start time is invalid".to_owned()))?;
    let occurred_at = mux_message_timestamp_opt(&value).unwrap_or(started_at);
    let event_type = mux_event_type(&value);
    let output_projection = matches!(
        event_type,
        ctx_history_core::EventType::ToolOutput | ctx_history_core::EventType::CommandOutput
    )
    .then(|| mux_output_projection(&value))
    .flatten();
    let retain_core_output = output_projection.as_ref().is_some_and(|projection| {
        matches!(
            projection.outcome,
            MuxOutputOutcome::Failure | MuxOutputOutcome::Timeout
        )
    });
    let native_ordinal = mux_native_event_index(plan, record, ordinal)?;
    let row = MuxMessageRow {
        line_number,
        source_path: plan.path.clone(),
        value,
        is_partial: plan.kind.is_partial(),
    };
    let model = session
        .model
        .clone()
        .or_else(|| mux_message_model(&row.value));
    let event = if output_projection.is_none() || retain_core_output {
        let mut event = mux_core_event(native_ordinal, &row, occurred_at, model.as_deref());
        if retain_core_output {
            if let Some(projection) = output_projection.as_ref() {
                apply_mux_core_output_diagnostic(&mut event, &row.value, projection);
            }
        }
        Some(event)
    } else {
        None
    };
    let event_hash = event
        .as_ref()
        .map(|event| event.provider_event_hash.clone());
    let mut file_touches = Vec::new();
    if matches!(
        event_type,
        ctx_history_core::EventType::ToolCall
            | ctx_history_core::EventType::ToolOutput
            | ctx_history_core::EventType::CommandOutput
            | ctx_history_core::EventType::FileTouched
    ) {
        let raw_source_path = plan.path.display().to_string();
        let source_root = context
            .source_root
            .as_ref()
            .or(context.source_path.as_ref())
            .map(|path| path.display().to_string());
        let provider_event_index = event.as_ref().map(|_| native_ordinal);
        let limit_exceeded = match visit_provider_file_touch_drafts_with_limit(
            &row.value,
            event_type_supports_structured_file_touches(event_type),
            MUX_MAX_FILE_TOUCHES_PER_EVENT,
            |(ordinal, touch)| {
                let provider_touch_index = match provider_event_index {
                    Some(index) if index > MAX_PACKED_PROVIDER_EVENT_INDEX => ordinal,
                    _ => (native_ordinal << 16) | ordinal,
                };
                file_touches.push(MuxFileTouch {
                    provider_touch_index,
                    provider_event_index,
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
                Ok::<(), std::convert::Infallible>(())
            },
        ) {
            Ok(outcome) => outcome.limit_exceeded(),
            Err(never) => match never {},
        };
        if limit_exceeded {
            record_rejection(
                line_number,
                PROVIDER_FILE_TOUCH_LIMIT_REJECTION.to_owned(),
                rejected_records,
                first_failure,
            )?;
        }
    }
    Ok(MuxPreparedRow {
        line_number,
        native_ordinal,
        event,
        event_hash,
        file_touches,
    })
}

fn mux_native_event_index(
    plan: &MuxSourcePlan,
    record: &MuxRawRecord,
    ordinal: u64,
) -> Result<u64> {
    if plan.generation > MUX_MAX_GENERATION {
        return Err(CaptureError::InvalidPayload(
            "Mux source generation exceeds NativePath event identity capacity".to_owned(),
        ));
    }
    let ordinal = if plan.kind.is_partial() {
        mux_partial_event_index(&record.payload) & MUX_MAX_ORDINAL
    } else {
        if ordinal > MUX_MAX_ORDINAL {
            return Err(CaptureError::InvalidPayload(
                "Mux source ordinal exceeds NativePath event identity capacity".to_owned(),
            ));
        }
        ordinal
    };
    Ok(
        (u64::from(plan.kind.is_partial()) * MUX_PARTIAL_NATIVE_ORDINAL)
            | (plan.generation << MUX_ORDINAL_BITS)
            | ordinal,
    )
}

#[allow(clippy::too_many_arguments)]
fn publish_core_page(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    configured_root: &Path,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    plan: &MuxSourcePlan,
    session_metadata: &MuxBoundedSessionMetadata,
    page: &MuxPreparedPage,
    accepted_events: u64,
    expected_store_cursor: Option<&SyncCursor>,
) -> Result<ProviderImportSummary> {
    let wire = MuxCursorWire {
        version: MUX_CURSOR_VERSION,
        capture_revision: MUX_CAPTURE_REVISION,
        policy_revision: MUX_POLICY_REVISION,
        kind: plan.kind,
        canonical_path: plan.observation.canonical_path.clone(),
        source_revision: plan.source_revision.clone(),
        metadata_revision: plan.metadata_revision.clone(),
        generation: plan.generation,
        frontier: page.next.clone(),
        terminal: page.terminal,
        retired: false,
        accepted_events,
        rejected_records: page.rejected_records,
        first_failure: page.first_failure.clone(),
    };
    let next = mux_sync_cursor(context, &plan.cursor_stream, &wire)?;
    let transition = NativePathCursorTransition::new(
        expected_store_cursor.map(|cursor| cursor.cursor.clone()),
        next,
    );
    let accounting = NativePathGroupAccounting::new(1, 1, page.source_bytes.saturating_add(1024))?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut publication = store.begin_native_path_publication_group(admission, accounting)?;
    let publication_id = core_publication_id(plan, page);
    let classification =
        publication.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?;
    let mut summary = ProviderImportSummary::default();
    match classification {
        NativePathCursorSetClassification::AllExpected => {
            let locator = ProviderSourceLocatorObservation {
                provider: CaptureProvider::Mux,
                source_format: MUX_SOURCE_FORMAT.to_owned(),
                machine_id: context.machine_id.clone(),
                locator_identity: plan.path_identity.clone(),
                cursor_stream: plan.cursor_stream.clone(),
                proposed_source_identity: plan.canonical_source_identity.clone(),
                raw_source_path: Some(plan.path.display().to_string()),
                source_revision: plan.source_revision.clone(),
                observed_at_ms: context.imported_at.timestamp_millis(),
            };
            let resolution = publication.reconcile_provider_source_locator(&locator)?;
            let source_id = mux_source_uuid(&resolution.canonical_source_identity);
            publication.upsert_capture_source(&mux_capture_source(
                source_id,
                configured_root,
                context,
                plan,
                session_metadata,
                &resolution.canonical_source_identity,
            )?)?;
            publication
                .bind_capture_source_provider_route(source_id, &resolution.route_binding())?;
            let session = mux_session(
                source_id,
                configured_root,
                context,
                options.history_record_id,
                session_metadata,
            )?;
            publication.upsert_session(&session)?;
            if let Some(parent_session_id) = session.parent_session_id {
                publication.upsert_projection_neutral_session_edge(
                    &canonical_actor(&session),
                    &mux_parent_edge(
                        source_id,
                        configured_root,
                        context,
                        session_metadata,
                        &session,
                        parent_session_id,
                    ),
                )?;
                summary.imported_edges = summary.imported_edges.saturating_add(1);
            }
            for row in &page.rows {
                let Some(event) = row.event.as_ref() else {
                    continue;
                };
                let event_hash = row
                    .event_hash
                    .as_deref()
                    .ok_or(CaptureError::SystemInvariant(
                        "Mux retained event has no provider hash",
                    ))?;
                let identity = avoid_provider_source_event_seq_collision(
                    store,
                    provider_source_event_import_identity(
                        source_id,
                        row.native_ordinal,
                        event_hash,
                    ),
                    source_id,
                    row.native_ordinal,
                    row.native_ordinal,
                )?;
                let event = mux_canonical_event(
                    &session_metadata.provider_session_id,
                    source_id,
                    session.id,
                    row.line_number,
                    event,
                    event_hash,
                    &identity,
                    context,
                    options,
                );
                if publication.reconcile_provider_event(
                    &event,
                    ProviderEventHashAuthority::ProviderSupplied,
                )? {
                    summary.imported = summary.imported.saturating_add(1);
                    summary.imported_events = summary.imported_events.saturating_add(1);
                } else {
                    summary.skipped = summary.skipped.saturating_add(1);
                    summary.skipped_events = summary.skipped_events.saturating_add(1);
                }
                summary.accepted_content_records =
                    summary.accepted_content_records.saturating_add(1);
                for touch in &row.file_touches {
                    let touch_id = provider_file_touch_import_id(
                        store,
                        CaptureProvider::Mux,
                        &session_metadata.provider_session_id,
                        source_id,
                        touch.provider_event_index,
                        touch.provider_touch_index,
                        false,
                    )?;
                    publication.upsert_file_touched(&mux_canonical_file_touch(
                        touch,
                        &session_metadata.provider_session_id,
                        options.history_record_id,
                        source_id,
                        session.id,
                        Some(event.id),
                        touch_id,
                    ))?;
                    summary.accepted_content_records =
                        summary.accepted_content_records.saturating_add(1);
                }
            }
            publication.prepare_journal_checkpoint()?;
            revalidate_source(plan)?;
            publication.publish_cursor_set()?;
            summary.imported_sessions = usize::from(page.expected.next_offset == 0);
            summary.imported = summary.imported.saturating_add(summary.imported_sessions);
            summary.set_work_result(ProviderImportWorkResult::Changed);
        }
        NativePathCursorSetClassification::AllNextSameGroup { .. } => {
            revalidate_source(plan)?;
            summary.skipped = summary.skipped.saturating_add(page.rows.len());
            summary.skipped_events = summary
                .skipped_events
                .saturating_add(page.rows.iter().filter(|row| row.event.is_some()).count());
            summary.set_work_result(ProviderImportWorkResult::NoOp);
        }
    }
    publication.commit()?;
    let page_rejections = page
        .rejected_records
        .saturating_sub(page.previous_rejected_records);
    summary.failed = summary
        .failed
        .saturating_add(usize::try_from(page_rejections).unwrap_or(usize::MAX));
    Ok(summary)
}

#[allow(clippy::too_many_arguments)]
fn mux_canonical_event(
    provider_session_id: &str,
    source_id: Uuid,
    session_id: Uuid,
    line_number: usize,
    event: &MuxCoreEvent,
    event_hash: &str,
    identity: &crate::provider::importer::ProviderEventImportIdentity,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
) -> Event {
    let dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, event_hash)
            .unwrap_or_else(|| identity.dedupe_key.clone());
    Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id: options.history_record_id,
        session_id: Some(session_id),
        run_id: None,
        event_type: event.event_type,
        role: event.role,
        occurred_at: event.occurred_at,
        capture_source_id: Some(source_id),
        payload: json!({
            "provider": CaptureProvider::Mux.as_str(),
            "provider_session_id": provider_session_id,
            "provider_event_index": event.provider_event_index,
            "provider_event_hash": event_hash,
            "cursor": event.cursor,
            "artifacts": [],
            "body": compact_provider_result_payload(event.event_type, &event.payload),
        }),
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": provider_session_id,
                "provider_event_index": event.provider_event_index,
                "provider_event_hash": event_hash,
                "provider_event_hash_authority": ProviderEventHashAuthority::ProviderSupplied.as_str(),
                "cursor": event.cursor,
                "source_format": MUX_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "fixture_line": line_number,
                "imported_at": context.imported_at,
                "event_idempotency_key": format!(
                    "provider-event:{}:{}:{}",
                    CaptureProvider::Mux.as_str(),
                    provider_session_id,
                    event.provider_event_index,
                ),
                "source_record_ordinal": Value::Null,
                "source_record_subrecord_index": Value::Null,
                "metadata": event.metadata,
            }),
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn mux_canonical_file_touch(
    touch: &MuxFileTouch,
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
                "provider": CaptureProvider::Mux.as_str(),
                "provider_session_id": provider_session_id,
                "provider_touch_index": touch.provider_touch_index,
                "provider_event_index": touch.provider_event_index,
                "raw_source_path": touch.raw_source_path,
                "source_id": source_id,
                "source_format": MUX_SOURCE_FORMAT,
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

fn mux_sync_cursor(
    context: &ProviderAdapterContext,
    stream: &str,
    wire: &MuxCursorWire,
) -> Result<SyncCursor> {
    let cursor = serde_json::to_string(wire)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    Ok(SyncCursor {
        id: stable_capture_uuid(
            &format!(
                "provider-cursor:{}:{}:{}",
                CaptureProvider::Mux.as_str(),
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
    })
}

fn core_publication_id(plan: &MuxSourcePlan, page: &MuxPreparedPage) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx/mux-nativepath/core-page/v1\0");
    digest.update(plan.canonical_source_identity.as_bytes());
    digest.update(plan.source_revision.as_bytes());
    digest.update(plan.generation.to_le_bytes());
    digest.update(serde_json::to_vec(&page.expected).unwrap_or_default());
    digest.update(serde_json::to_vec(&page.next).unwrap_or_default());
    digest.update([u8::from(page.terminal)]);
    format!("{MUX_PUBLICATION_PREFIX}core:{}", hex(&digest.finalize()))
}

fn revalidate_source(plan: &MuxSourcePlan) -> Result<()> {
    if plan
        .observation
        .revalidate(&plan.path, plan.source.metadata_path.as_deref())?
    {
        Ok(())
    } else {
        Err(CaptureError::SourceChangedDuringCapture)
    }
}

fn mux_source_uuid(canonical_source_identity: &str) -> Uuid {
    stable_capture_uuid(canonical_source_identity, "mux-nativepath-capture-source")
}

fn mux_root_namespace(configured_root: &Path) -> Result<String> {
    provider_source_identity(
        CaptureProvider::Mux,
        MUX_SOURCE_FORMAT,
        Some(&configured_root.display().to_string()),
        None,
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Mux NativePath root identity is unavailable",
    ))
}

fn mux_capture_source(
    source_id: Uuid,
    configured_root: &Path,
    context: &ProviderAdapterContext,
    plan: &MuxSourcePlan,
    metadata: &MuxBoundedSessionMetadata,
    canonical_source_identity: &str,
) -> Result<CaptureSource> {
    let started_at = metadata
        .started_at
        .parse::<DateTime<Utc>>()
        .map_err(|_| CaptureError::InvalidPayload("Mux start time is invalid".to_owned()))?;
    Ok(CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Mux,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: metadata.cwd.clone(),
            raw_source_path: Some(plan.path.display().to_string()),
            source_format: Some(MUX_SOURCE_FORMAT.to_owned()),
            source_root: Some(configured_root.display().to_string()),
            source_identity: Some(canonical_source_identity.to_owned()),
            external_session_id: Some(metadata.provider_session_id.clone()),
        },
        started_at,
        ended_at: None,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": metadata.provider_session_id,
                "source_format": MUX_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "source_revision": plan.source_revision,
                "metadata_revision": metadata.metadata_revision,
                "nativepath_publication": "mux-v1",
            }),
        ),
    })
}

fn mux_session(
    source_id: Uuid,
    configured_root: &Path,
    context: &ProviderAdapterContext,
    history_record_id: Option<Uuid>,
    metadata: &MuxBoundedSessionMetadata,
) -> Result<Session> {
    let namespace = mux_root_namespace(configured_root)?;
    let id = provider_source_session_uuid(&namespace, &metadata.provider_session_id);
    let parent_session_id = metadata
        .parent_provider_session_id
        .as_deref()
        .map(|parent| provider_source_session_uuid(&namespace, parent));
    let root_session_id = metadata
        .root_provider_session_id
        .as_deref()
        .or(metadata.parent_provider_session_id.as_deref())
        .map(|root| provider_source_session_uuid(&namespace, root))
        .unwrap_or(id);
    let started_at = metadata
        .started_at
        .parse::<DateTime<Utc>>()
        .map_err(|_| CaptureError::InvalidPayload("Mux start time is invalid".to_owned()))?;
    let is_primary = parent_session_id.is_none();
    Ok(Session {
        id,
        history_record_id,
        parent_session_id,
        root_session_id: Some(root_session_id),
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Mux,
        external_session_id: Some(metadata.provider_session_id.clone()),
        external_agent_id: None,
        agent_type: if is_primary {
            AgentType::Primary
        } else {
            AgentType::Subagent
        },
        role_hint: Some(if is_primary { "primary" } else { "subagent" }.to_owned()),
        is_primary,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at,
        ended_at: None,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": metadata.provider_session_id,
                "source_format": MUX_SOURCE_FORMAT,
                "model": metadata.model,
                "metadata": metadata.metadata,
                "nativepath_publication": "mux-v1",
            }),
        ),
    })
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

fn mux_parent_edge(
    source_id: Uuid,
    configured_root: &Path,
    context: &ProviderAdapterContext,
    metadata: &MuxBoundedSessionMetadata,
    session: &Session,
    parent_session_id: Uuid,
) -> SessionEdge {
    SessionEdge {
        id: stable_capture_uuid(
            &format!(
                "{}:{}:{}",
                configured_root.display(),
                metadata.provider_session_id,
                parent_session_id
            ),
            "mux-nativepath-parent-child",
        ),
        from_session_id: session.id,
        to_session_id: parent_session_id,
        edge_type: SessionEdgeType::ParentChild,
        confidence: Confidence::Explicit,
        source_id: Some(source_id),
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": metadata.provider_session_id,
                "parent_provider_session_id": metadata.parent_provider_session_id,
                "source_format": MUX_SOURCE_FORMAT,
                "nativepath_publication": "mux-v1",
            }),
        ),
    }
}

fn verify_terminal_core(store: &Store, machine_id: &str, plan: &MuxSourcePlan) -> Result<()> {
    let stored = store
        .get_sync_cursor(None, machine_id, &plan.cursor_stream)?
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Mux output replay requires committed NativePath Core".to_owned(),
            )
        })?;
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let wire: MuxCursorWire = serde_json::from_str(committed.provider_cursor())
        .map_err(|_| CaptureError::InvalidPayload("Mux NativePath cursor is corrupt".to_owned()))?;
    if wire.version != MUX_CURSOR_VERSION
        || wire.capture_revision != MUX_CAPTURE_REVISION
        || wire.policy_revision != MUX_POLICY_REVISION
        || wire.kind != plan.kind
        || wire.canonical_path != plan.observation.canonical_path
        || wire.source_revision != plan.source_revision
        || wire.metadata_revision != plan.metadata_revision
        || !wire.terminal
        || wire.retired
    {
        return Err(CaptureError::InvalidPayload(
            "Mux output replay source no longer matches committed Core authority".to_owned(),
        ));
    }
    revalidate_source(plan)
}

fn replay_source_outputs(
    plan: &MuxSourcePlan,
    context: &ProviderAdapterContext,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    let output_source = OutputSourceIdentity {
        provider: CaptureProvider::Mux.as_str().to_owned(),
        namespace_id: plan.canonical_source_identity.clone(),
        source_id: plan.path_identity.clone(),
    };
    let progress = sink
        .observe_source(&output_source)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let output_plan = MuxOutputPlan::new(plan, sink, progress.as_ref())?;
    if output_plan.noop {
        return revalidate_source(plan);
    }
    let mut session =
        mux_bounded_session_metadata(&plan.source, &plan.metadata_revision, context.imported_at)?;
    let (mut reader, mut hasher) =
        open_reader_at_frontier(&plan.path, &output_plan.start_frontier)?;
    let mut frontier = output_plan.start_frontier.clone();
    let mut expected_sink_frontier = output_plan.expected_sink_frontier.clone();
    let mut expected_source_epoch = output_plan.expected_source_epoch;
    let mut disposition = output_plan.disposition;
    loop {
        let Some(page) = read_output_page(
            &mut reader,
            &mut hasher,
            &mut session,
            plan,
            frontier.clone(),
        )?
        else {
            break;
        };
        let next_safe_frontier = safe_frontier(&page.next)?;
        let expected_frontier = safe_frontier(&page.expected)?;
        let estimated_output_bytes =
            page.observations
                .iter()
                .fold(16 * 1024_usize, |bytes, observation| {
                    bytes
                        .saturating_add(observation.content.len())
                        .saturating_add(observation.coordinate.unit_key.len())
                        .saturating_add(observation.locator.payload.len())
                        .saturating_add(512)
                });
        let output = NativeProOutputPage {
            inventory_generation: sink.inventory_generation(),
            source: output_source.clone(),
            source_epoch: output_plan.source_epoch,
            observed_revision: plan.source_revision.clone(),
            parser_revision: MUX_OUTPUT_PARSER_REVISION.to_owned(),
            materializer_revision: sink.materializer_revision().to_owned(),
            disposition,
            expected_prior_source_epoch: expected_source_epoch,
            expected_prior_frontier: expected_sink_frontier.clone(),
            observations: page.observations,
        };
        let replay = NativeProReplayPage::new(
            expected_frontier,
            next_safe_frontier.clone(),
            page.terminal,
            NativePageAccounting {
                logical_units: page.physical_records,
                conservative_serialized_bytes: estimated_output_bytes,
            },
            output,
        )
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        revalidate_source(plan)?;
        process_pro_replay_only(replay, sink).map_err(|failure| {
            CaptureError::InvalidPayload(format!(
                "Mux output page failed: {:?}",
                failure.output_error
            ))
        })?;
        frontier = page.next;
        expected_sink_frontier = Some(next_safe_frontier);
        expected_source_epoch = Some(output_plan.source_epoch);
        disposition = ProOutputSourceDisposition::AppendOrResume;
        if page.terminal {
            break;
        }
    }
    revalidate_source(plan)
}

struct MuxOutputPlan {
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_sink_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
    start_frontier: MuxFrontier,
    noop: bool,
}

impl MuxOutputPlan {
    fn new(
        plan: &MuxSourcePlan,
        sink: &dyn ProOutputSink,
        progress: Option<&crate::ProOutputProgress>,
    ) -> Result<Self> {
        let Some(progress) = progress else {
            return Ok(Self {
                source_epoch: 0,
                expected_source_epoch: None,
                expected_sink_frontier: None,
                disposition: ProOutputSourceDisposition::NewSource,
                start_frontier: MuxFrontier::initial(),
                noop: false,
            });
        };
        let prior_frontier = progress
            .cursor
            .as_ref()
            .map(decode_output_frontier)
            .transpose()?;
        let expected_sink_frontier = progress
            .cursor
            .as_ref()
            .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload.clone()))
            .transpose()
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        let revisions_match = progress.parser_revision == MUX_OUTPUT_PARSER_REVISION
            && progress.materializer_revision == sink.materializer_revision();
        if revisions_match
            && progress.observed_revision == plan.source_revision
            && progress.terminal
        {
            return Ok(Self {
                source_epoch: progress.source_epoch,
                expected_source_epoch: Some(progress.source_epoch),
                expected_sink_frontier,
                disposition: ProOutputSourceDisposition::AppendOrResume,
                start_frontier: prior_frontier.unwrap_or_else(MuxFrontier::initial),
                noop: true,
            });
        }
        let append = if revisions_match {
            match prior_frontier.as_ref() {
                Some(frontier) => prefix_matches(&plan.path, &plan.observation, frontier)?,
                None => false,
            }
        } else {
            false
        };
        if append {
            return Ok(Self {
                source_epoch: progress.source_epoch,
                expected_source_epoch: Some(progress.source_epoch),
                expected_sink_frontier,
                disposition: ProOutputSourceDisposition::AppendOrResume,
                start_frontier: prior_frontier.unwrap_or_else(MuxFrontier::initial),
                noop: false,
            });
        }
        Ok(Self {
            source_epoch: progress.source_epoch.checked_add(1).ok_or(
                CaptureError::InvalidPayload("Mux output source epoch is exhausted".to_owned()),
            )?,
            expected_source_epoch: Some(progress.source_epoch),
            expected_sink_frontier,
            disposition: ProOutputSourceDisposition::Rewrite,
            start_frontier: MuxFrontier::initial(),
            noop: false,
        })
    }
}

struct MuxPreparedOutputPage {
    observations: Vec<ProOutputObservation>,
    expected: MuxFrontier,
    next: MuxFrontier,
    terminal: bool,
    physical_records: usize,
}

fn read_output_page(
    reader: &mut BufReader<File>,
    hasher: &mut Sha256,
    session: &mut MuxBoundedSessionMetadata,
    plan: &MuxSourcePlan,
    expected: MuxFrontier,
) -> Result<Option<MuxPreparedOutputPage>> {
    let mut observations = Vec::new();
    let mut source_bytes = 0_usize;
    let mut physical_records = 0_usize;
    let mut offset = expected.next_offset;
    let mut ordinal = expected.next_ordinal;
    let max_records = if plan.kind == MuxStreamKind::Partial {
        1
    } else {
        MUX_PAGE_MAX_RECORDS
    };
    while physical_records < max_records && source_bytes < MUX_PAGE_MAX_BYTES {
        let record = if plan.kind == MuxStreamKind::Partial {
            read_bounded_whole_record(reader, hasher, offset)?
        } else {
            read_bounded_record(reader, hasher, offset)?
        };
        let Some(record) = record else {
            break;
        };
        offset = record.end;
        source_bytes = source_bytes.saturating_add(record.observed_bytes);
        physical_records = physical_records.saturating_add(1);
        if !record.oversized && !record.payload.iter().all(u8::is_ascii_whitespace) {
            if let Ok(value) = serde_json::from_slice::<Value>(&record.payload) {
                if value.is_object() {
                    if let Some(provider_session_id) = value
                        .get("workspaceId")
                        .and_then(Value::as_str)
                        .filter(|id| !id.trim().is_empty())
                    {
                        session.provider_session_id = bounded_mux_id(
                            provider_session_id.to_owned(),
                            &plan.path,
                            "workspace id",
                        )?;
                    }
                    if let Some(observation) =
                        prepare_output_observation(&value, &record, ordinal, session, plan)?
                    {
                        observations.push(observation);
                    }
                }
            }
        }
        ordinal = ordinal.checked_add(1).ok_or(CaptureError::SystemInvariant(
            "Mux output ordinal overflowed",
        ))?;
        if plan.kind == MuxStreamKind::Partial {
            break;
        }
    }
    let terminal = reader.fill_buf()?.is_empty();
    let next = MuxFrontier {
        version: MUX_FRONTIER_VERSION,
        next_offset: offset,
        next_ordinal: ordinal,
        prefix_sha256: hasher.clone().finalize().into(),
        file_identity: Some(plan.observation.content_identity()),
    };
    Ok(Some(MuxPreparedOutputPage {
        observations,
        expected,
        next,
        terminal,
        physical_records,
    }))
}

fn prepare_output_observation(
    value: &Value,
    record: &MuxRawRecord,
    ordinal: u64,
    session: &MuxBoundedSessionMetadata,
    plan: &MuxSourcePlan,
) -> Result<Option<ProOutputObservation>> {
    let Some(projection) = mux_output_projection(value).filter(|output| output.body_available)
    else {
        return Ok(None);
    };
    let Some(content) = mux_result_content(value) else {
        return Ok(None);
    };
    let started_at = session
        .started_at
        .parse::<DateTime<Utc>>()
        .map_err(|_| CaptureError::InvalidPayload("Mux start time is invalid".to_owned()))?;
    let occurred_at = mux_message_timestamp_opt(value).unwrap_or(started_at);
    let native_sequence = if plan.kind.is_partial() {
        mux_partial_event_index(&record.payload).max(MUX_PARTIAL_NATIVE_ORDINAL)
    } else {
        ordinal
    };
    let outcome = match projection.outcome {
        MuxOutputOutcome::Success => OutputOutcome::Success,
        MuxOutputOutcome::Failure => OutputOutcome::Failure,
        MuxOutputOutcome::Timeout => OutputOutcome::Timeout,
        MuxOutputOutcome::Unknown => OutputOutcome::Unknown,
    };
    let native_record_id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty() && id.len() <= 4 * 1024)
        .map(str::to_owned);
    let locator_payload = serde_json::to_vec(&json!({
        "path": plan.observation.canonical_path,
        "byte_start": record.start,
        "byte_end_exclusive": record.end,
        "kind": plan.kind.label(),
    }))
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    Ok(Some(ProOutputObservation {
        kind: OutputObservationKind::Tool,
        coordinate: OutputNativeCoordinate {
            unit_key: format!("mux:{}:{native_sequence}:output", plan.kind.label()),
            native_sequence,
            native_record_id,
            source_record_ordinal: Some(ordinal),
            source_record_subrecord_index: Some(0),
            byte_start: Some(record.start),
            byte_end_exclusive: Some(record.end),
        },
        occurred_at_unix_ms: Some(occurred_at.timestamp_millis()),
        associations: OutputAssociations {
            direct_session_id: session.provider_session_id.clone(),
            root_session_id: session
                .root_provider_session_id
                .clone()
                .unwrap_or_else(|| session.provider_session_id.clone()),
            parent_session_id: session.parent_provider_session_id.clone(),
            provider_session_id: Some(session.provider_session_id.clone()),
            agent_id: None,
            repository: None,
        },
        call_id: match projection.call_ids.as_slice() {
            [call_id] => Some(call_id.clone()),
            _ => None,
        },
        command: None,
        outcome: OutputOutcomeMetadata {
            outcome,
            exit_code: projection.exit_code,
            duration_ms: None,
        },
        locator: OutputSourceLocator {
            version: 1,
            kind: "mux-native-source-range-v1".to_owned(),
            payload: locator_payload,
        },
        content: content.into_bytes(),
    }))
}

fn safe_frontier(frontier: &MuxFrontier) -> Result<NativeSafeFrontier> {
    NativeSafeFrontier::new(
        MUX_FRONTIER_VERSION,
        serde_json::to_vec(frontier)
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

fn decode_output_frontier(cursor: &crate::OutputNativeCursor) -> Result<MuxFrontier> {
    if cursor.version != MUX_FRONTIER_VERSION {
        return Err(CaptureError::InvalidPayload(
            "Mux output cursor version is unsupported".to_owned(),
        ));
    }
    let frontier: MuxFrontier = serde_json::from_slice(&cursor.payload)
        .map_err(|_| CaptureError::InvalidPayload("Mux output cursor is corrupt".to_owned()))?;
    if frontier.version != MUX_FRONTIER_VERSION {
        return Err(CaptureError::InvalidPayload(
            "Mux output frontier is inconsistent".to_owned(),
        ));
    }
    Ok(frontier)
}

fn root_cursor_stream(configured_root: &Path) -> String {
    provider_source_cursor_stream_for_path(
        CaptureProvider::Mux,
        MUX_SOURCE_FORMAT,
        &format!("mux-nativepath-root:{}", configured_root.display()),
    )
}

fn load_root_manifest(
    store: &Store,
    machine_id: &str,
    configured_root: &Path,
) -> Result<Option<MuxRootManifest>> {
    let stream = root_cursor_stream(configured_root);
    let Some(stored) = store.get_sync_cursor(None, machine_id, &stream)? else {
        return Ok(None);
    };
    let committed = decode_native_path_committed_cursor(&stored.cursor).map_err(|_| {
        CaptureError::InvalidPayload("Mux NativePath root cursor is corrupt".to_owned())
    })?;
    let manifest: MuxRootManifest = serde_json::from_str(committed.provider_cursor())
        .map_err(|_| CaptureError::InvalidPayload("Mux root manifest is corrupt".to_owned()))?;
    if manifest.version != MUX_ROOT_MANIFEST_VERSION || manifest.configured_root != configured_root
    {
        return Err(CaptureError::InvalidPayload(
            "Mux root manifest identity is inconsistent".to_owned(),
        ));
    }
    Ok(Some(manifest))
}

fn publish_root_manifest(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    manifest: MuxRootManifest,
) -> Result<ProviderImportSummary> {
    let stream = root_cursor_stream(&manifest.configured_root);
    let stored = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let encoded = serde_json::to_string(&manifest)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    if let Some(stored) = stored.as_ref() {
        if let Ok(committed) = decode_native_path_committed_cursor(&stored.cursor) {
            if committed.provider_cursor() == encoded {
                let mut summary = ProviderImportSummary::default();
                summary.set_work_result(ProviderImportWorkResult::NoOp);
                return Ok(summary);
            }
        }
    }
    let next = SyncCursor {
        id: stable_capture_uuid(
            &format!(
                "provider-cursor:{}:{}:{}",
                CaptureProvider::Mux.as_str(),
                context.machine_id,
                stream
            ),
            "provider-sync-cursor",
        ),
        team_id: None,
        device_id: context.machine_id.clone(),
        stream: stream.clone(),
        cursor: encoded,
        last_synced_at: Some(context.imported_at),
        timestamps: timestamps(context.imported_at),
    };
    let transition =
        NativePathCursorTransition::new(stored.as_ref().map(|cursor| cursor.cursor.clone()), next);
    let accounting =
        NativePathGroupAccounting::new(1, 1, transition.next().cursor.len().saturating_add(256))?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    let publication_id = manifest_publication_id(&manifest);
    let already = matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    );
    if !already {
        group.prepare_journal_checkpoint()?;
        group.publish_cursor_set()?;
    }
    group.commit()?;
    let mut summary = ProviderImportSummary::default();
    summary.set_work_result(if already {
        ProviderImportWorkResult::NoOp
    } else {
        ProviderImportWorkResult::Changed
    });
    Ok(summary)
}

fn manifest_publication_id(manifest: &MuxRootManifest) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx/mux-nativepath/root-manifest/v1\0");
    digest.update(serde_json::to_vec(manifest).unwrap_or_default());
    format!(
        "{MUX_PUBLICATION_PREFIX}manifest:{}",
        hex(&digest.finalize())
    )
}

fn retire_missing_sources(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    prior: &MuxRootManifest,
    current: &[MuxManifestSource],
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let live = current
        .iter()
        .map(|source| (source.path.clone(), source.kind))
        .collect::<BTreeSet<_>>();
    for missing in prior
        .sources
        .iter()
        .filter(|source| !live.contains(&(source.path.clone(), source.kind)))
    {
        summary.merge_from(retire_missing_source(store, bulk_guard, context, missing)?);
    }
    Ok(())
}

fn retire_missing_source(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    missing: &MuxManifestSource,
) -> Result<ProviderImportSummary> {
    let stored = store
        .get_sync_cursor(None, &context.machine_id, &missing.cursor_stream)?
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Mux manifest source is missing its committed cursor".to_owned(),
            )
        })?;
    let committed = decode_native_path_committed_cursor(&stored.cursor).map_err(|_| {
        CaptureError::InvalidPayload("Mux route retirement requires a NativePath cursor".to_owned())
    })?;
    let prior: MuxCursorWire = serde_json::from_str(committed.provider_cursor())
        .map_err(|_| CaptureError::InvalidPayload("Mux NativePath cursor is corrupt".to_owned()))?;
    if prior.retired {
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }
    let next_wire = MuxCursorWire {
        version: MUX_CURSOR_VERSION,
        capture_revision: MUX_CAPTURE_REVISION,
        policy_revision: MUX_POLICY_REVISION,
        kind: missing.kind,
        canonical_path: missing.path.clone(),
        source_revision: format!("retired:{}", missing.source_revision),
        metadata_revision: prior.metadata_revision.clone(),
        generation: prior
            .generation
            .checked_add(1)
            .ok_or(CaptureError::InvalidPayload(
                "Mux NativePath source generation is exhausted".to_owned(),
            ))?,
        frontier: prior.frontier.clone(),
        terminal: true,
        retired: true,
        accepted_events: prior.accepted_events,
        rejected_records: prior.rejected_records,
        first_failure: prior.first_failure.clone(),
    };
    let next = mux_sync_cursor(context, &missing.cursor_stream, &next_wire)?;
    let transition = NativePathCursorTransition::new(Some(stored.cursor.clone()), next);
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::Mux,
        source_format: MUX_SOURCE_FORMAT.to_owned(),
        machine_id: context.machine_id.clone(),
        locator_identity: missing.locator_identity.clone(),
        cursor_stream: missing.cursor_stream.clone(),
        expected_canonical_source_identity: missing.canonical_source_identity.clone(),
        expected_source_revision: missing.source_revision.clone(),
        retired_at_ms: context.imported_at.timestamp_millis(),
        reason: if context
            .source_path
            .as_ref()
            .is_some_and(|root| !root.exists())
        {
            ProviderSourceRouteRetirementReason::RootMissing
        } else {
            ProviderSourceRouteRetirementReason::SourceMissing
        },
    };
    let accounting = NativePathGroupAccounting::new(1, 1, 1024)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    let publication_id = retirement_publication_id(missing, &next_wire);
    let classification =
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?;
    let mut changed = false;
    if matches!(
        classification,
        NativePathCursorSetClassification::AllExpected
    ) {
        changed = matches!(
            group.retire_provider_source_route(&retirement)?,
            ctx_history_store::ProviderSourceRouteRetirementDisposition::Retired
        );
        group.prepare_journal_checkpoint()?;
        group.publish_cursor_set()?;
    }
    group.commit()?;
    let mut summary = ProviderImportSummary::default();
    summary.set_work_result(if changed {
        ProviderImportWorkResult::Changed
    } else {
        ProviderImportWorkResult::NoOp
    });
    Ok(summary)
}

fn retirement_publication_id(source: &MuxManifestSource, wire: &MuxCursorWire) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx/mux-nativepath/retire/v1\0");
    digest.update(source.cursor_stream.as_bytes());
    digest.update(source.canonical_source_identity.as_bytes());
    digest.update(wire.generation.to_le_bytes());
    format!("{MUX_PUBLICATION_PREFIX}retire:{}", hex(&digest.finalize()))
}

fn ensure_active_journal(store: &Store) -> Result<()> {
    match store.projection_journal_snapshot(None) {
        Ok(_) => Ok(()),
        Err(ctx_history_store::StoreError::ProjectionJournalInactive) => {
            store.activate_projection_journal(ctx_pro_host_protocol::PROTOCOL_FINGERPRINT)?;
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        encoded.push(char::from(DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}
