use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, EventRole, EventType, SyncCursor};
use ctx_history_store::{
    NativePathCursorSetClassification, NativePathCursorTransition, NativePathGroupAccounting,
    ProviderSourceRouteRetirement, ProviderSourceRouteRetirementDisposition,
    ProviderSourceRouteRetirementReason, Store,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{
    provider::{
        importer::{provider_path_identity, provider_source_cursor_stream_for_path, timestamps},
        native_ingestion::{
            process_pro_replay_only, NativePageAccounting, NativeProOutputPage,
            NativeProReplayPage, NativeSafeFrontier, NativeSourceIdentity,
        },
        normalization::{provider_output_event_is_failure, provider_role, provider_value_text},
    },
    stable_capture_uuid, CaptureError, CaptureWorkLimit, ImportProfile, OutputAssociations,
    OutputNativeCoordinate, OutputObservationKind, OutputOutcome, OutputOutcomeMetadata,
    OutputSourceIdentity, OutputSourceLocator, ProOutputObservation, ProOutputProgress,
    ProOutputSink, ProOutputSinkError, ProOutputSourceDisposition, ProviderImportSummary,
    ProviderImportWorkResult, Result, QWEN_CODE_SOURCE_FORMAT,
};

use super::super::result_content::{NativeJsonlResultExtractionError, NativeJsonlResultSubrecord};
use super::{
    decode_direct_jsonl_cursor, decode_direct_jsonl_native_cursor, encode_direct_jsonl_cursor,
    open_direct_jsonl_pages, publish_direct_jsonl_group,
    reader::{direct_jsonl_source_revision, observe_file},
    DirectJsonlCheckpoint, DirectJsonlCursorDecode, DirectJsonlOutput, DirectJsonlPage,
    DirectJsonlPendingPage, DirectJsonlPublicationContext, DirectJsonlScanOutcome,
    DirectJsonlSourceChange, NativePathJsonlTreeImport,
};

const QWEN_CODE_MISSING_REASON: &str =
    "no Qwen Code chat JSONL transcripts found under projects/*/chats";
const QWEN_CODE_GROUP_MAX_PAGES: usize = 32;
const QWEN_CODE_GROUP_MAX_SOURCES: usize = 64;
const QWEN_CODE_GROUP_MAX_BYTES: usize = 6 * 1024 * 1024;
const QWEN_CODE_GROUP_MAX_ESTIMATED_MUTATIONS: usize = 3_000;
const QWEN_CODE_OUTPUT_FRONTIER_VERSION: u32 = 1;
const QWEN_CODE_OUTPUT_PARSER_REVISION: &str = "qwen-code-direct-native-jsonl-v1";

pub(crate) fn qwen_code_event_identity(value: &Value) -> Option<&str> {
    value
        .get("id")
        .or_else(|| value.get("uuid"))
        .and_then(Value::as_str)
        .filter(|event_id| !event_id.trim().is_empty())
}

pub(crate) fn import_qwen_code_nativepath_tree(
    store: &mut Store,
    request: NativePathJsonlTreeImport<'_>,
) -> Result<ProviderImportSummary> {
    let configured_source_root = request
        .source_root
        .clone()
        .or(request.source_path.clone())
        .unwrap_or_else(|| request.path.to_path_buf());
    let live_inventory = discover_live_transcripts(request.path)?;
    let known_routes = known_qwen_code_routes(store, &request.machine_id, &configured_source_root)?;
    let sink = request.import_profile.sink().cloned();

    if request.import_profile.is_replay_only() {
        replay_outputs_or_mark_behind(
            &live_inventory.paths,
            &configured_source_root,
            request.imported_at,
            sink.as_deref(),
        );
        return Ok(ProviderImportSummary::default());
    }

    if live_inventory.paths.is_empty() {
        if known_routes.is_empty() {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: request.path.to_path_buf(),
                reason: QWEN_CODE_MISSING_REASON,
            });
        }
        return retire_missing_routes(
            store,
            &request.machine_id,
            request.imported_at,
            &known_routes,
            &live_inventory.paths,
            if live_inventory.root_missing {
                ProviderSourceRouteRetirementReason::RootMissing
            } else {
                ProviderSourceRouteRetirementReason::SourceMissing
            },
        );
    }

    let mut summary = import_qwen_code_core(
        store,
        NativePathJsonlTreeImport {
            path: request.path,
            machine_id: request.machine_id.clone(),
            source_path: request.source_path,
            source_root: request.source_root,
            imported_at: request.imported_at,
            history_record_id: request.history_record_id,
            capture_work_limit: request.capture_work_limit,
            inventory_observation_token: request.inventory_observation_token,
            import_profile: ImportProfile::CoreOnly,
        },
    )?;
    if summary.work_remaining {
        return Ok(summary);
    }
    summary.merge_from(retire_missing_routes(
        store,
        &request.machine_id,
        request.imported_at,
        &known_routes,
        &live_inventory.paths,
        ProviderSourceRouteRetirementReason::SourceMissing,
    )?);
    replay_outputs_or_mark_behind(
        &live_inventory.paths,
        &configured_source_root,
        request.imported_at,
        sink.as_deref(),
    );
    Ok(summary)
}

struct QwenCodeInventory {
    paths: BTreeSet<PathBuf>,
    root_missing: bool,
}

fn discover_live_transcripts(root: &Path) -> Result<QwenCodeInventory> {
    match std::fs::symlink_metadata(root) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(QwenCodeInventory {
                paths: BTreeSet::new(),
                root_missing: true,
            });
        }
        Err(error) => return Err(error.into()),
    }
    let mut paths = BTreeSet::new();
    super::super::traversal::visit_jsonl_tree_files(
        root,
        &qwen_code_file_is_selected,
        &mut |path| {
            paths.insert(std::fs::canonicalize(path)?);
            Ok(())
        },
    )?;
    Ok(QwenCodeInventory {
        paths,
        root_missing: false,
    })
}

pub(crate) fn qwen_code_file_is_selected(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()) == Some("jsonl")
        && path
            .components()
            .any(|component| component.as_os_str() == "chats")
}

fn import_qwen_code_core(
    store: &mut Store,
    request: NativePathJsonlTreeImport<'_>,
) -> Result<ProviderImportSummary> {
    let configured_source_root = request
        .source_root
        .clone()
        .or(request.source_path.clone())
        .unwrap_or_else(|| request.path.to_path_buf());
    let committed_store = Store::open_read_only(store.path())?;
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let context = DirectJsonlPublicationContext {
        provider: CaptureProvider::QwenCode,
        source_format: QWEN_CODE_SOURCE_FORMAT,
        machine_id: &request.machine_id,
        source_root: &configured_source_root,
        imported_at: request.imported_at,
        history_record_id: request.history_record_id,
        inventory_observation_token: request.inventory_observation_token.as_deref(),
    };
    let mut accumulator = QwenCodeGroupAccumulator::new(
        store,
        &committed_store,
        &bulk_guard,
        context,
        request.capture_work_limit,
    );
    let mut visited = 0_usize;
    let operation = super::super::traversal::visit_jsonl_tree_files(
        request.path,
        &qwen_code_file_is_selected,
        &mut |path| {
            visited = visited.saturating_add(1);
            if accumulator.stopped() {
                return Ok(());
            }
            if let Some(token) = request.inventory_observation_token.as_deref() {
                if crate::observe_ordinary_file(path)?.token_hex() != token {
                    return Err(CaptureError::SourceChangedDuringCapture);
                }
            }
            let observation = observe_file(path)?;
            let canonical_path = std::fs::canonicalize(path)?;
            let path_identity = provider_path_identity(&canonical_path)?;
            let stream = provider_source_cursor_stream_for_path(
                CaptureProvider::QwenCode,
                QWEN_CODE_SOURCE_FORMAT,
                &path_identity,
            );
            let stored = accumulator
                .store()
                .get_sync_cursor(None, &request.machine_id, &stream)?;
            let previous = stored
                .as_ref()
                .map(|cursor| {
                    decode_direct_jsonl_cursor(
                        &cursor.cursor,
                        CaptureProvider::QwenCode,
                        QWEN_CODE_SOURCE_FORMAT,
                        &canonical_path,
                        &observation,
                    )
                })
                .transpose()?
                .and_then(|decoded| match decoded {
                    DirectJsonlCursorDecode::Native(checkpoint)
                    | DirectJsonlCursorDecode::Migrated(checkpoint) => Some(checkpoint),
                    DirectJsonlCursorDecode::Reset => None,
                });
            let mut reader = open_direct_jsonl_pages(
                CaptureProvider::QwenCode,
                QWEN_CODE_SOURCE_FORMAT,
                &canonical_path,
                Some(configured_source_root.clone()),
                request.imported_at,
                false,
                previous.as_ref(),
            )?;
            let mut emitted_page = false;
            while let Some(page) = reader.next_page()? {
                emitted_page = true;
                accumulator.push(DirectJsonlPendingPage {
                    path: canonical_path.clone(),
                    page,
                })?;
                if accumulator.stopped() {
                    break;
                }
            }
            if !accumulator.stopped() && !emitted_page {
                if let Some(outcome) = reader.outcome() {
                    if outcome.source_change == DirectJsonlSourceChange::Unchanged {
                        accumulator.record_unchanged(outcome);
                    } else {
                        accumulator.push(DirectJsonlPendingPage {
                            path: canonical_path,
                            page: observation_only_page(outcome.checkpoint.clone()),
                        })?;
                    }
                }
            }
            Ok(())
        },
    );
    let operation = operation.and_then(|_| accumulator.finish());
    let stopped = accumulator.stopped();
    drop(accumulator);
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    match (operation, finish) {
        (Ok(mut summary), Ok(())) => {
            if visited == 0 {
                return Err(CaptureError::InvalidProviderTranscriptPath {
                    path: request.path.to_path_buf(),
                    reason: QWEN_CODE_MISSING_REASON,
                });
            }
            if stopped {
                summary.work_remaining = true;
            }
            Ok(summary)
        }
        (_, Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
    }
}

fn observation_only_page(checkpoint: DirectJsonlCheckpoint) -> DirectJsonlPage {
    DirectJsonlPage {
        expected_checkpoint: checkpoint.clone(),
        next_checkpoint: checkpoint.clone(),
        events: Vec::new(),
        outputs: Vec::new(),
        rejections: Vec::new(),
        logical_units: 1,
        conservative_serialized_bytes: 2 * 1024,
        terminal: checkpoint.terminal,
    }
}

struct QwenCodeGroupAccumulator<'a> {
    store: &'a mut Store,
    committed_store: &'a Store,
    bulk_guard: &'a ctx_history_store::EventSearchBulkGuard,
    context: DirectJsonlPublicationContext<'a>,
    work_limit: CaptureWorkLimit,
    pages: Vec<DirectJsonlPendingPage>,
    bytes: usize,
    estimated_mutations: usize,
    sources: BTreeSet<PathBuf>,
    summary: ProviderImportSummary,
    stopped: bool,
}

impl<'a> QwenCodeGroupAccumulator<'a> {
    fn new(
        store: &'a mut Store,
        committed_store: &'a Store,
        bulk_guard: &'a ctx_history_store::EventSearchBulkGuard,
        context: DirectJsonlPublicationContext<'a>,
        work_limit: CaptureWorkLimit,
    ) -> Self {
        Self {
            store,
            committed_store,
            bulk_guard,
            context,
            work_limit,
            pages: Vec::new(),
            bytes: 0,
            estimated_mutations: 0,
            sources: BTreeSet::new(),
            summary: ProviderImportSummary::default(),
            stopped: false,
        }
    }

    fn store(&self) -> &Store {
        self.store
    }

    fn stopped(&self) -> bool {
        self.stopped
    }

    fn record_unchanged(&mut self, outcome: &DirectJsonlScanOutcome) {
        let sessions = usize::from(outcome.checkpoint.session.is_some());
        let events = usize::try_from(outcome.accepted_events).unwrap_or(usize::MAX);
        self.summary.skipped_sessions = self.summary.skipped_sessions.saturating_add(sessions);
        self.summary.skipped_events = self.summary.skipped_events.saturating_add(events);
        self.summary.skipped = self
            .summary
            .skipped
            .saturating_add(sessions)
            .saturating_add(events);
    }

    fn push(&mut self, pending: DirectJsonlPendingPage) -> Result<()> {
        let next_sources = self.sources.len() + usize::from(!self.sources.contains(&pending.path));
        let next_bytes = self
            .bytes
            .saturating_add(pending.page.conservative_serialized_bytes);
        let page_mutations = pending
            .page
            .events
            .iter()
            .map(|event| 1_usize.saturating_add(event.touches.len()))
            .sum::<usize>()
            .saturating_add(4);
        let next_mutations = self.estimated_mutations.saturating_add(page_mutations);
        if !self.pages.is_empty()
            && (self.pages.len() >= QWEN_CODE_GROUP_MAX_PAGES
                || next_sources > QWEN_CODE_GROUP_MAX_SOURCES
                || next_bytes > QWEN_CODE_GROUP_MAX_BYTES
                || next_mutations > QWEN_CODE_GROUP_MAX_ESTIMATED_MUTATIONS)
        {
            self.flush()?;
            if self.stopped {
                return Ok(());
            }
        }
        self.bytes = self
            .bytes
            .saturating_add(pending.page.conservative_serialized_bytes);
        self.estimated_mutations = self.estimated_mutations.saturating_add(page_mutations);
        self.sources.insert(pending.path.clone());
        self.pages.push(pending);
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        if self.pages.is_empty() {
            return Ok(());
        }
        let pages = std::mem::take(&mut self.pages);
        let summary = publish_direct_jsonl_group(
            self.store,
            self.committed_store,
            self.bulk_guard,
            &self.context,
            &pages,
        )?;
        self.summary.merge_from(summary);
        self.bytes = 0;
        self.estimated_mutations = 0;
        self.sources.clear();
        if self.work_limit == CaptureWorkLimit::OneSafeGroup {
            self.stopped = true;
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<ProviderImportSummary> {
        if !self.stopped {
            self.flush()?;
        }
        Ok(std::mem::take(&mut self.summary))
    }
}

#[derive(Clone)]
struct KnownQwenCodeRoute {
    path: PathBuf,
    locator_identity: String,
    canonical_source_identity: String,
    source_revision: String,
    current_cursor: SyncCursor,
    checkpoint: DirectJsonlCheckpoint,
}

fn known_qwen_code_routes(
    store: &Store,
    machine_id: &str,
    source_root: &Path,
) -> Result<Vec<KnownQwenCodeRoute>> {
    let source_root = source_root.display().to_string();
    let mut routes = BTreeMap::<String, KnownQwenCodeRoute>::new();
    for source in store.list_capture_sources()? {
        if source.descriptor.provider != CaptureProvider::QwenCode
            || source.descriptor.machine_id != machine_id
            || source.descriptor.source_format.as_deref() != Some(QWEN_CODE_SOURCE_FORMAT)
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
            CaptureProvider::QwenCode,
            QWEN_CODE_SOURCE_FORMAT,
            &locator_identity,
        );
        let Some(current_cursor) = store.get_sync_cursor(None, machine_id, &stream)? else {
            continue;
        };
        let Some(checkpoint) = decode_direct_jsonl_native_cursor(
            &current_cursor.cursor,
            CaptureProvider::QwenCode,
            QWEN_CODE_SOURCE_FORMAT,
        ) else {
            continue;
        };
        let checkpoint_session = checkpoint
            .session
            .as_ref()
            .map(|session| session.provider_session_id.as_str());
        if checkpoint.source_path != path
            || source.descriptor.external_session_id.as_deref() != checkpoint_session
        {
            continue;
        }
        let Some(source_revision) = source
            .sync
            .metadata
            .get("source_revision")
            .and_then(Value::as_str)
        else {
            continue;
        };
        let route = KnownQwenCodeRoute {
            path,
            locator_identity: locator_identity.clone(),
            canonical_source_identity: canonical_source_identity.to_owned(),
            source_revision: source_revision.to_owned(),
            current_cursor,
            checkpoint,
        };
        if routes.insert(locator_identity, route).is_some() {
            return Err(CaptureError::SystemInvariant(
                "Qwen Code persisted duplicate current routes for one transcript",
            ));
        }
    }
    Ok(routes.into_values().collect())
}

fn retire_missing_routes(
    store: &mut Store,
    machine_id: &str,
    retired_at: DateTime<Utc>,
    known_routes: &[KnownQwenCodeRoute],
    live_paths: &BTreeSet<PathBuf>,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<ProviderImportSummary> {
    let missing = known_routes
        .iter()
        .filter(|route| !live_paths.contains(&route.path))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(ProviderImportSummary::default());
    }
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let mut summary = ProviderImportSummary::default();
        for route in missing {
            if retire_route(store, &bulk_guard, machine_id, retired_at, route, reason)? {
                summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
                summary.skipped = summary.skipped.saturating_add(1);
                summary.set_work_result(ProviderImportWorkResult::Changed);
            }
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

fn retire_route(
    store: &Store,
    bulk_guard: &ctx_history_store::EventSearchBulkGuard,
    machine_id: &str,
    retired_at: DateTime<Utc>,
    route: &KnownQwenCodeRoute,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<bool> {
    let stream = route.current_cursor.stream.clone();
    let provider_cursor = encode_direct_jsonl_cursor(&route.checkpoint)?;
    let transition = NativePathCursorTransition::new(
        Some(route.current_cursor.cursor.clone()),
        provider_sync_cursor(
            machine_id,
            stream.clone(),
            provider_cursor.clone(),
            retired_at,
        ),
    );
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::QwenCode,
        source_format: QWEN_CODE_SOURCE_FORMAT.to_owned(),
        machine_id: machine_id.to_owned(),
        locator_identity: route.locator_identity.clone(),
        cursor_stream: stream,
        expected_canonical_source_identity: route.canonical_source_identity.clone(),
        expected_source_revision: route.source_revision.clone(),
        retired_at_ms: retired_at.timestamp_millis(),
        reason,
    };
    let publication_id = retirement_publication_id(&retirement);
    if super::direct_jsonl_cursor_matches_publication(
        &route.current_cursor.cursor,
        &publication_id,
        &provider_cursor,
    ) {
        return Ok(false);
    }
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

fn replay_outputs_or_mark_behind(
    paths: &BTreeSet<PathBuf>,
    source_root: &Path,
    imported_at: DateTime<Utc>,
    sink: Option<&dyn ProOutputSink>,
) {
    let Some(sink) = sink else {
        return;
    };
    if let Err(error) = replay_qwen_code_outputs(paths, source_root, imported_at, sink) {
        sink.mark_behind(ProOutputSinkError::new(
            "qwen_code_nativepath_output_replay",
            error.to_string(),
        ));
    }
}

fn replay_qwen_code_outputs(
    paths: &BTreeSet<PathBuf>,
    source_root: &Path,
    imported_at: DateTime<Utc>,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    for path in paths {
        let locator_identity = provider_path_identity(path)?;
        let source = OutputSourceIdentity {
            provider: CaptureProvider::QwenCode.as_str().to_owned(),
            namespace_id: source_root.display().to_string(),
            source_id: locator_identity.clone(),
        };
        let progress = match sink.observe_source(&source) {
            Ok(progress) => progress,
            Err(error) => {
                sink.mark_behind(error);
                continue;
            }
        };
        replay_qwen_code_source(path, imported_at, sink, source, locator_identity, progress)?;
    }
    Ok(())
}

fn replay_qwen_code_source(
    path: &Path,
    imported_at: DateTime<Utc>,
    sink: &dyn ProOutputSink,
    output_source: OutputSourceIdentity,
    locator_identity: String,
    progress: Option<ProOutputProgress>,
) -> Result<()> {
    let progress_cursor = progress
        .as_ref()
        .and_then(|progress| progress.cursor.as_ref())
        .filter(|cursor| cursor.version == QWEN_CODE_OUTPUT_FRONTIER_VERSION)
        .and_then(|cursor| serde_json::from_slice::<DirectJsonlCheckpoint>(&cursor.payload).ok())
        .filter(|checkpoint| {
            checkpoint.is_supported_for(CaptureProvider::QwenCode, QWEN_CODE_SOURCE_FORMAT)
        });
    let can_resume = progress.as_ref().is_some_and(|progress| {
        progress.parser_revision == QWEN_CODE_OUTPUT_PARSER_REVISION
            && progress.materializer_revision == sink.materializer_revision()
            && progress_cursor.is_some()
    });
    let previous = if can_resume {
        progress_cursor.as_ref()
    } else {
        None
    };
    let mut reader = open_direct_jsonl_pages(
        CaptureProvider::QwenCode,
        QWEN_CODE_SOURCE_FORMAT,
        path,
        None,
        imported_at,
        true,
        previous,
    )?;
    let source_change = reader.source_change();
    let observed_revision = direct_jsonl_source_revision(reader.observation());
    let mut output_state = QwenCodeOutputState::new(
        output_source,
        progress,
        source_change,
        can_resume,
        sink.materializer_revision(),
    )?;

    while let Some(page) = reader.next_page()? {
        let expected_frontier = safe_frontier(&page.expected_checkpoint)?;
        let next_safe_frontier = safe_frontier(&page.next_checkpoint)?;
        let observations = page
            .outputs
            .into_iter()
            .map(|output| output_observation(&page.next_checkpoint, path, output))
            .collect::<Vec<_>>();
        let accounting = NativePageAccounting {
            logical_units: page.logical_units.max(1),
            conservative_serialized_bytes: page.conservative_serialized_bytes,
        };
        let output = NativeProOutputPage {
            inventory_generation: sink.inventory_generation(),
            source: output_state.source.clone(),
            source_epoch: output_state.source_epoch,
            observed_revision: observed_revision.clone(),
            parser_revision: QWEN_CODE_OUTPUT_PARSER_REVISION.to_owned(),
            materializer_revision: sink.materializer_revision().to_owned(),
            disposition: output_state.disposition,
            expected_prior_source_epoch: output_state.expected_source_epoch,
            expected_prior_frontier: output_state.expected_sink_frontier.clone(),
            observations,
        };
        let replay = NativeProReplayPage::new_with_source_identity(
            NativeSourceIdentity::new(CaptureProvider::QwenCode.as_str(), &locator_identity),
            expected_frontier,
            next_safe_frontier.clone(),
            page.terminal,
            accounting,
            output,
        )
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        if process_pro_replay_only(replay, sink).is_err() {
            break;
        }
        output_state.expected_source_epoch = Some(output_state.source_epoch);
        output_state.expected_sink_frontier = Some(next_safe_frontier);
        output_state.disposition = ProOutputSourceDisposition::AppendOrResume;
    }
    Ok(())
}

struct QwenCodeOutputState {
    source: OutputSourceIdentity,
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_sink_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
}

impl QwenCodeOutputState {
    fn new(
        source: OutputSourceIdentity,
        progress: Option<ProOutputProgress>,
        source_change: DirectJsonlSourceChange,
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
            || progress.materializer_revision != materializer_revision
            || matches!(
                source_change,
                DirectJsonlSourceChange::Fresh
                    | DirectJsonlSourceChange::Rewrite
                    | DirectJsonlSourceChange::Truncation
                    | DirectJsonlSourceChange::Replacement
            );
        Ok(Self {
            source,
            source_epoch: if rewrite {
                progress
                    .source_epoch
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Qwen Code output source epoch exhausted",
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

fn safe_frontier(checkpoint: &DirectJsonlCheckpoint) -> Result<NativeSafeFrontier> {
    NativeSafeFrontier::new(
        QWEN_CODE_OUTPUT_FRONTIER_VERSION,
        serde_json::to_vec(checkpoint)?,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

fn output_observation(
    checkpoint: &DirectJsonlCheckpoint,
    path: &Path,
    output: DirectJsonlOutput,
) -> ProOutputObservation {
    let session = checkpoint.session.as_ref();
    let direct_session_id = session
        .map(|session| session.provider_session_id.clone())
        .unwrap_or_else(|| "unknown-session".to_owned());
    ProOutputObservation {
        kind: OutputObservationKind::Tool,
        coordinate: OutputNativeCoordinate {
            unit_key: format!("{}:{}", output.raw_ordinal, output.sub_ordinal),
            native_sequence: output.raw_ordinal,
            native_record_id: output.call_id.clone(),
            source_record_ordinal: Some(output.raw_ordinal),
            source_record_subrecord_index: Some(output.sub_ordinal),
            byte_start: Some(output.byte_start),
            byte_end_exclusive: Some(output.byte_end_exclusive),
        },
        occurred_at_unix_ms: None,
        associations: OutputAssociations {
            direct_session_id: direct_session_id.clone(),
            root_session_id: session
                .and_then(|session| session.root_provider_session_id.clone())
                .unwrap_or_else(|| direct_session_id.clone()),
            parent_session_id: session
                .and_then(|session| session.parent_provider_session_id.clone()),
            provider_session_id: Some(direct_session_id),
            agent_id: session.and_then(|session| session.external_agent_id.clone()),
            repository: None,
        },
        call_id: output.call_id,
        command: None,
        outcome: OutputOutcomeMetadata {
            outcome: output.outcome,
            exit_code: output.exit_code,
            duration_ms: output.duration_ms,
        },
        locator: OutputSourceLocator {
            version: 1,
            kind: "qwen-code-jsonl-range-v1".to_owned(),
            payload: serde_json::to_vec(&json!({
                "path": path,
                "byte_start": output.byte_start,
                "byte_end_exclusive": output.byte_end_exclusive,
            }))
            .unwrap_or_default(),
        },
        content: output.content,
    }
}

fn provider_sync_cursor(
    machine_id: &str,
    stream: String,
    cursor: String,
    observed_at: DateTime<Utc>,
) -> SyncCursor {
    SyncCursor {
        id: stable_capture_uuid(
            &format!(
                "provider-cursor:{}:{}:{}",
                CaptureProvider::QwenCode.as_str(),
                machine_id,
                stream
            ),
            "provider-sync-cursor",
        ),
        team_id: None,
        device_id: machine_id.to_owned(),
        stream,
        cursor,
        last_synced_at: Some(observed_at),
        timestamps: timestamps(observed_at),
    }
}

fn retirement_publication_id(retirement: &ProviderSourceRouteRetirement) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-qwen-code-nativepath-route-retirement-v1\0");
    digest.update(retirement.provider.as_str().as_bytes());
    digest.update(retirement.source_format.as_bytes());
    digest.update(retirement.machine_id.as_bytes());
    digest.update(retirement.locator_identity.as_bytes());
    digest.update(retirement.cursor_stream.as_bytes());
    digest.update(retirement.expected_canonical_source_identity.as_bytes());
    digest.update(retirement.expected_source_revision.as_bytes());
    digest.update(format!("{:?}", retirement.reason).as_bytes());
    format!("qwen-code-nativepath-retirement-v1:{:x}", digest.finalize())
}

pub(super) fn qwen_code_header_session_id(value: &Value) -> Option<String> {
    value
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
}

pub(super) fn qwen_code_header_cwd(value: &Value) -> Option<String> {
    value
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.trim().is_empty())
        .map(str::to_owned)
}

pub(super) fn qwen_code_event_type(value: &Value) -> EventType {
    match value.get("type").and_then(Value::as_str) {
        Some("user" | "assistant") if qwen_code_content_has(value, "tool_use") => {
            EventType::ToolCall
        }
        Some("tool_result") => EventType::ToolOutput,
        Some("user" | "assistant") => EventType::Message,
        Some("system") => EventType::Notice,
        _ if value.get("toolCallResult").is_some() => EventType::ToolOutput,
        _ => EventType::Notice,
    }
}

pub(super) fn qwen_code_role(value: &Value) -> EventRole {
    provider_role(
        value
            .pointer("/message/role")
            .or_else(|| value.get("type"))
            .and_then(Value::as_str),
    )
}

pub(super) fn qwen_code_event_text(value: &Value) -> String {
    value
        .pointer("/message/content")
        .or_else(|| value.get("message"))
        .and_then(provider_value_text)
        .or_else(|| value.get("toolCallResult").and_then(provider_value_text))
        .or_else(|| value.get("content").and_then(provider_value_text))
        .unwrap_or_default()
}

pub(super) fn qwen_code_model(value: &Value) -> Option<Value> {
    value
        .get("model")
        .cloned()
        .or_else(|| value.pointer("/message/model").cloned())
}

fn qwen_code_content_has(value: &Value, expected: &str) -> bool {
    value
        .pointer("/message/content")
        .or_else(|| value.get("content"))
        .and_then(Value::as_array)
        .is_some_and(|blocks| {
            blocks
                .iter()
                .any(|block| block.get("type").and_then(Value::as_str) == Some(expected))
        })
}

pub(super) fn enumerate_qwen_code_results(
    value: &Value,
) -> std::result::Result<Vec<NativeJsonlResultSubrecord<'_>>, NativeJsonlResultExtractionError> {
    if value.get("type").and_then(Value::as_str) != Some("tool_result")
        && value.get("toolCallResult").is_none()
    {
        return Ok(Vec::new());
    }
    if reject_redacted(value).is_err() {
        let count = result_block_count(value.pointer("/message/content"))?.max(1);
        return (0..count)
            .map(|index| {
                Ok(NativeJsonlResultSubrecord {
                    subrecord_index: u32::try_from(index)
                        .map_err(|_| NativeJsonlResultExtractionError::InvalidShape)?,
                    content: None,
                    call_id: None,
                    tool_name: None,
                    outcome: unknown_result_outcome(),
                })
            })
            .collect();
    }
    let blocks = enumerate_content_block_results(value.pointer("/message/content"), value)?;
    if !blocks.is_empty() {
        return Ok(blocks);
    }
    if let Some(result) = value.get("toolCallResult") {
        reject_redacted(result)?;
        return Ok(vec![NativeJsonlResultSubrecord {
            subrecord_index: 0,
            content: extract_result_ref(Some(result), &["output", "content", "text"])?,
            call_id: native_result_identity(result).or_else(|| native_result_identity(value)),
            tool_name: native_result_tool_name(result).or_else(|| native_result_tool_name(value)),
            outcome: native_result_outcome_with_record(result, value),
        }]);
    }
    Ok(vec![NativeJsonlResultSubrecord {
        subrecord_index: 0,
        content: extract_result_ref(value.get("content"), &[])?,
        call_id: native_result_identity(value),
        tool_name: native_result_tool_name(value),
        outcome: native_result_outcome(value),
    }])
}

fn enumerate_content_block_results<'a>(
    content: Option<&'a Value>,
    record: &'a Value,
) -> std::result::Result<Vec<NativeJsonlResultSubrecord<'a>>, NativeJsonlResultExtractionError> {
    let Some(content) = content else {
        return Ok(Vec::new());
    };
    content
        .as_array()
        .ok_or(NativeJsonlResultExtractionError::InvalidShape)?
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
        .enumerate()
        .map(|(index, block)| {
            let (content, redacted) =
                match extract_result_ref(Some(block), &["content", "output", "text"]) {
                    Ok(content) => (content, false),
                    Err(NativeJsonlResultExtractionError::Redacted) => (None, true),
                    Err(error) => return Err(error),
                };
            Ok(NativeJsonlResultSubrecord {
                subrecord_index: u32::try_from(index)
                    .map_err(|_| NativeJsonlResultExtractionError::InvalidShape)?,
                content,
                call_id: (!redacted).then(|| native_result_identity(block)).flatten(),
                tool_name: (!redacted)
                    .then(|| native_result_tool_name(block))
                    .flatten(),
                outcome: if redacted {
                    unknown_result_outcome()
                } else {
                    native_result_outcome_with_record(block, record)
                },
            })
        })
        .collect()
}

fn result_block_count(
    content: Option<&Value>,
) -> std::result::Result<usize, NativeJsonlResultExtractionError> {
    let Some(content) = content else {
        return Ok(0);
    };
    Ok(content
        .as_array()
        .ok_or(NativeJsonlResultExtractionError::InvalidShape)?
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
        .count())
}

fn extract_result_ref<'a>(
    value: Option<&'a Value>,
    object_fields: &[&str],
) -> std::result::Result<Option<&'a str>, NativeJsonlResultExtractionError> {
    let Some(value) = value else {
        return Ok(None);
    };
    reject_redacted(value)?;
    match value {
        Value::String(text) => Ok(Some(text)),
        Value::Null => Ok(None),
        Value::Object(object) => {
            for field in object_fields {
                if let Some(selected) = object.get(*field) {
                    return match selected {
                        Value::String(text) => Ok(Some(text)),
                        Value::Null => Ok(None),
                        _ => Err(NativeJsonlResultExtractionError::InvalidShape),
                    };
                }
            }
            Ok(None)
        }
        Value::Array(_) | Value::Bool(_) | Value::Number(_) => {
            Err(NativeJsonlResultExtractionError::InvalidShape)
        }
    }
}

fn native_result_identity(value: &Value) -> Option<&str> {
    [
        "call_id",
        "callId",
        "tool_call_id",
        "toolCallId",
        "tool_use_id",
        "toolUseId",
        "id",
    ]
    .into_iter()
    .find_map(|key| value.get(key).and_then(Value::as_str))
}

fn native_result_tool_name(value: &Value) -> Option<&str> {
    ["tool_name", "toolName", "name", "tool"]
        .into_iter()
        .find_map(|key| value.get(key).and_then(Value::as_str))
}

fn native_result_outcome_with_record(subrecord: &Value, record: &Value) -> OutputOutcomeMetadata {
    let mut outcome = native_result_outcome(subrecord);
    if outcome.outcome == OutputOutcome::Unknown {
        outcome = native_result_outcome(record);
    }
    outcome
}

fn native_result_outcome(value: &Value) -> OutputOutcomeMetadata {
    let timeout = native_result_has_timeout(value);
    let failure = provider_output_event_is_failure(value);
    let success = native_result_has_success(value);
    OutputOutcomeMetadata {
        outcome: if timeout {
            OutputOutcome::Timeout
        } else if failure {
            OutputOutcome::Failure
        } else if success {
            OutputOutcome::Success
        } else {
            OutputOutcome::Unknown
        },
        exit_code: native_result_i64(value, &["exit_code", "exitCode"])
            .and_then(|code| i32::try_from(code).ok()),
        duration_ms: native_result_u64(value, &["duration_ms", "durationMs", "duration"]),
    }
}

fn unknown_result_outcome() -> OutputOutcomeMetadata {
    OutputOutcomeMetadata {
        outcome: OutputOutcome::Unknown,
        exit_code: None,
        duration_ms: None,
    }
}

fn native_result_has_timeout(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(native_result_has_timeout),
        Value::Object(values) => {
            values.iter().any(|(key, value)| {
                matches!(normalized_result_key(key).as_str(), "timeout" | "timedout")
                    && value.as_bool().unwrap_or(false)
            }) || values.values().any(native_result_has_timeout)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

fn native_result_has_success(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(native_result_has_success),
        Value::Object(values) => {
            values.iter().any(|(key, value)| {
                let key = normalized_result_key(key);
                (matches!(key.as_str(), "success" | "ok") && value.as_bool() == Some(true))
                    || (key == "exitcode" && value.as_i64() == Some(0))
                    || (key == "statuscode"
                        && value
                            .as_i64()
                            .is_some_and(|code| (200..400).contains(&code)))
                    || (matches!(key.as_str(), "iserror" | "timedout" | "timeout")
                        && value.as_bool() == Some(false))
                    || (matches!(key.as_str(), "status" | "state" | "outcome")
                        && value.as_str().is_some_and(|status| {
                            matches!(
                                status.trim().to_ascii_lowercase().as_str(),
                                "success"
                                    | "succeeded"
                                    | "complete"
                                    | "completed"
                                    | "ok"
                                    | "passed"
                            )
                        }))
            }) || values.values().any(native_result_has_success)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

fn native_result_i64(value: &Value, expected_keys: &[&str]) -> Option<i64> {
    match value {
        Value::Array(values) => values
            .iter()
            .find_map(|value| native_result_i64(value, expected_keys)),
        Value::Object(values) => values
            .iter()
            .find_map(|(key, value)| {
                expected_keys
                    .iter()
                    .any(|expected| key == expected)
                    .then(|| value.as_i64())
                    .flatten()
            })
            .or_else(|| {
                values
                    .values()
                    .find_map(|value| native_result_i64(value, expected_keys))
            }),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => None,
    }
}

fn native_result_u64(value: &Value, expected_keys: &[&str]) -> Option<u64> {
    match value {
        Value::Array(values) => values
            .iter()
            .find_map(|value| native_result_u64(value, expected_keys)),
        Value::Object(values) => values
            .iter()
            .find_map(|(key, value)| {
                expected_keys
                    .iter()
                    .any(|expected| key == expected)
                    .then(|| value.as_u64())
                    .flatten()
            })
            .or_else(|| {
                values
                    .values()
                    .find_map(|value| native_result_u64(value, expected_keys))
            }),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => None,
    }
}

fn normalized_result_key(key: &str) -> String {
    key.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn reject_redacted(value: &Value) -> std::result::Result<(), NativeJsonlResultExtractionError> {
    let Some(object) = value.as_object() else {
        return Ok(());
    };
    let flag_is_redacted = ["redacted", "is_redacted", "isRedacted"]
        .iter()
        .filter_map(|field| object.get(*field))
        .any(|flag| flag.as_bool() != Some(false));
    let state_is_redacted = ["status", "state"]
        .iter()
        .filter_map(|field| object.get(*field).and_then(Value::as_str))
        .any(|state| matches!(state, "redacted" | "output-redacted"));
    if flag_is_redacted || state_is_redacted {
        Err(NativeJsonlResultExtractionError::Redacted)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        io::Write,
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc, Mutex,
        },
    };

    use crate::{ProOutputMaterializationPage, ProOutputPageResult, ProviderImportFailure};

    use super::*;

    const MACHINE: &str = "qwen-code-nativepath-test-machine";
    const SUCCESS_BODY: &str = "QWEN_SUCCESS_BODY_MUST_NOT_ENTER_CORE";

    #[test]
    fn production_lifecycle_covers_restart_append_rewrite_truncation_replacement_and_loss() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let root = temp.path().join(".qwen/projects");
        let transcript = transcript_path(&root);
        write_transcript(
            &transcript,
            &[
                message("qwen-life", "fresh-user", "user", "fresh-user"),
                tool_call("qwen-life", "fresh-call"),
            ],
        );
        let store_path = temp.path().join("work.sqlite");
        let mut store = Store::open(&store_path).unwrap();

        let fresh = import(&root, &mut store, ImportProfile::CoreOnly);
        assert_eq!(fresh.work_result(), ProviderImportWorkResult::Changed);
        assert_eq!(fresh.imported_sessions, 1);
        assert_eq!(fresh.imported_events, 2);
        let session = qwen_session(&store, "qwen-life");
        assert!(session.is_primary);
        assert!(session.parent_session_id.is_none());
        assert!(session.root_session_id.is_none());
        let original_events = store.events_for_session(session.id).unwrap();
        assert_eq!(original_events.len(), 2);
        let routed_event = original_events[0].id;
        assert!(store
            .authorized_source_route_for_event(routed_event)
            .is_ok());

        let previous = checkpoint(&store, &transcript);
        assert_eq!(
            classify(&transcript, &root, &previous),
            DirectJsonlSourceChange::Unchanged
        );
        assert_eq!(
            import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
            ProviderImportWorkResult::NoOp
        );

        drop(store);
        let mut store = Store::open(&store_path).unwrap();
        assert_eq!(
            import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
            ProviderImportWorkResult::NoOp
        );

        let previous = checkpoint(&store, &transcript);
        append_record(
            &transcript,
            &message("qwen-life", "append", "assistant", "append-assistant"),
        );
        assert_eq!(
            classify(&transcript, &root, &previous),
            DirectJsonlSourceChange::Append
        );
        let append = import(&root, &mut store, ImportProfile::CoreOnly);
        assert_eq!(append.work_result(), ProviderImportWorkResult::Changed);
        assert_eq!(append.imported_events, 1);

        let previous = checkpoint(&store, &transcript);
        write_transcript(
            &transcript,
            &[
                message(
                    "qwen-life",
                    "rewrite-user",
                    "user",
                    &"rewrite-user-content-".repeat(24),
                ),
                message(
                    "qwen-life",
                    "rewrite-assistant",
                    "assistant",
                    &"rewrite-assistant-content-".repeat(24),
                ),
            ],
        );
        assert_eq!(
            classify(&transcript, &root, &previous),
            DirectJsonlSourceChange::Rewrite
        );
        assert_eq!(
            import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
            ProviderImportWorkResult::Changed
        );

        let previous = checkpoint(&store, &transcript);
        write_transcript(
            &transcript,
            &[message("qwen-life", "short", "user", "short")],
        );
        assert_eq!(
            classify(&transcript, &root, &previous),
            DirectJsonlSourceChange::Truncation
        );
        assert_eq!(
            import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
            ProviderImportWorkResult::Changed
        );

        let previous = checkpoint(&store, &transcript);
        let replacement = transcript.with_extension("replacement");
        write_transcript(
            &replacement,
            &[message(
                "qwen-life",
                "replacement",
                "user",
                "replacement-generation",
            )],
        );
        fs::rename(&replacement, &transcript).unwrap();
        assert_eq!(
            classify(&transcript, &root, &previous),
            DirectJsonlSourceChange::Replacement
        );
        assert_eq!(
            import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
            ProviderImportWorkResult::Changed
        );

        fs::remove_file(&transcript).unwrap();
        let source_missing = import(&root, &mut store, ImportProfile::CoreOnly);
        assert_eq!(
            source_missing.work_result(),
            ProviderImportWorkResult::Changed
        );
        assert!(store
            .authorized_source_route_for_event(routed_event)
            .is_err());
        assert_eq!(
            import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
            ProviderImportWorkResult::NoOp
        );

        write_transcript(
            &transcript,
            &[message(
                "qwen-life",
                "reappeared",
                "user",
                "reappeared-generation",
            )],
        );
        assert_eq!(
            import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
            ProviderImportWorkResult::Changed
        );
        let reappeared_event = store
            .events_for_session(qwen_session(&store, "qwen-life").id)
            .unwrap()
            .into_iter()
            .find(|event| {
                serde_json::to_string(event)
                    .unwrap()
                    .contains("reappeared-generation")
            })
            .unwrap()
            .id;
        assert!(store
            .authorized_source_route_for_event(reappeared_event)
            .is_ok());

        fs::remove_dir_all(&root).unwrap();
        let root_missing = import(&root, &mut store, ImportProfile::CoreOnly);
        assert_eq!(
            root_missing.work_result(),
            ProviderImportWorkResult::Changed
        );
        assert!(store
            .authorized_source_route_for_event(reappeared_event)
            .is_err());
        assert_eq!(
            import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
            ProviderImportWorkResult::NoOp
        );
    }

    #[test]
    fn core_commits_before_failed_pro_and_later_output_replay_is_independent() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let root = temp.path().join(".qwen/projects");
        let transcript = transcript_path(&root);
        write_transcript(
            &transcript,
            &[
                message("qwen-core-first", "core-first", "user", "core-first"),
                tool_call("qwen-core-first", "call-with-output"),
                tool_result("qwen-core-first", "result-with-output", SUCCESS_BODY, false),
            ],
        );
        let store_path = temp.path().join("core.sqlite");
        let mut store = Store::open(&store_path).unwrap();
        let failing_sink = Arc::new(RecordingSink::new(store_path.clone(), true));

        let fresh = import(
            &root,
            &mut store,
            ImportProfile::CoreAndPro(failing_sink.clone()),
        );
        assert_eq!(fresh.work_result(), ProviderImportWorkResult::Changed);
        assert!(failing_sink.saw_core_before_page.load(Ordering::SeqCst));
        assert_eq!(failing_sink.behind.load(Ordering::SeqCst), 1);
        let core_events = store
            .events_for_session(qwen_session(&store, "qwen-core-first").id)
            .unwrap();
        assert_eq!(core_events.len(), 2);
        assert!(core_events.iter().all(|event| !matches!(
            event.event_type,
            EventType::ToolOutput | EventType::CommandOutput
        )));
        assert!(!serde_json::to_string(&core_events)
            .unwrap()
            .contains(SUCCESS_BODY));

        let replay_sink = Arc::new(RecordingSink::new(store_path.clone(), false));
        let replay = import(
            &root,
            &mut store,
            ImportProfile::ProReplayOnly(replay_sink.clone()),
        );
        assert_eq!(replay.work_result(), ProviderImportWorkResult::NoOp);
        assert!(replay_sink.saw_core_before_page.load(Ordering::SeqCst));
        assert!(replay_sink.pages.load(Ordering::SeqCst) > 0);
        assert_eq!(replay_sink.outputs.load(Ordering::SeqCst), 1);
        let pages_after_replay = replay_sink.pages.load(Ordering::SeqCst);
        assert_eq!(
            import(
                &root,
                &mut store,
                ImportProfile::ProReplayOnly(replay_sink.clone()),
            )
            .work_result(),
            ProviderImportWorkResult::NoOp
        );
        assert_eq!(replay_sink.pages.load(Ordering::SeqCst), pages_after_replay);

        let pro_only_path = temp.path().join("pro-only.sqlite");
        let mut pro_only_store = Store::open(&pro_only_path).unwrap();
        let pro_only_sink = Arc::new(RecordingSink::new(pro_only_path, false));
        assert_eq!(
            import(
                &root,
                &mut pro_only_store,
                ImportProfile::ProReplayOnly(pro_only_sink.clone()),
            )
            .work_result(),
            ProviderImportWorkResult::NoOp
        );
        assert!(pro_only_store.list_sessions().unwrap().is_empty());
        assert!(!pro_only_sink.saw_core_before_page.load(Ordering::SeqCst));
        assert_eq!(pro_only_sink.outputs.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn malformed_record_and_incomplete_tail_resume_at_the_exact_safe_frontier() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let root = temp.path().join(".qwen/projects");
        let transcript = transcript_path(&root);
        fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        let first =
            serde_json::to_vec(&message("qwen-partial", "first", "user", "first-valid")).unwrap();
        let second = serde_json::to_vec(&message(
            "qwen-partial",
            "second",
            "assistant",
            "second-valid",
        ))
        .unwrap();
        let tail = serde_json::to_vec(&message(
            "qwen-partial",
            "tail",
            "assistant",
            "completed-after-retry",
        ))
        .unwrap();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&first);
        bytes.push(b'\n');
        bytes.extend_from_slice(b"{malformed-json}\n");
        bytes.extend_from_slice(&second);
        bytes.push(b'\n');
        bytes.extend_from_slice(&tail[..tail.len() - 1]);
        fs::write(&transcript, bytes).unwrap();

        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let first_import = import(&root, &mut store, ImportProfile::CoreOnly);
        assert_eq!(first_import.imported_sessions, 1);
        assert_eq!(first_import.imported_events, 2);
        assert_eq!(first_import.failed, 1);
        assert!(matches!(
            first_import.failures.as_slice(),
            [ProviderImportFailure { line: 2, .. }]
        ));
        let partial_checkpoint = checkpoint(&store, &transcript);
        assert!(!partial_checkpoint.terminal);
        assert!(
            partial_checkpoint.complete_prefix_end < partial_checkpoint.source_observation.length
        );

        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&transcript)
            .unwrap();
        file.write_all(b"}\n").unwrap();
        drop(file);
        assert_eq!(
            classify(&transcript, &root, &partial_checkpoint),
            DirectJsonlSourceChange::Append
        );
        let completed = import(&root, &mut store, ImportProfile::CoreOnly);
        assert_eq!(completed.work_result(), ProviderImportWorkResult::Changed);
        assert_eq!(completed.imported_events, 1);
        assert_eq!(completed.failed, 0);
        let rendered = serde_json::to_string(
            &store
                .events_for_session(qwen_session(&store, "qwen-partial").id)
                .unwrap(),
        )
        .unwrap();
        assert!(rendered.contains("completed-after-retry"));
        assert_eq!(
            import(&root, &mut store, ImportProfile::CoreOnly).work_result(),
            ProviderImportWorkResult::NoOp
        );
    }

    #[test]
    fn provider_owned_parser_preserves_result_precedence_redaction_and_failure_policy() {
        let successful = tool_result(
            "qwen-parser",
            "successful",
            "higher-priority-content",
            false,
        );
        let results = enumerate_qwen_code_results(&successful).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, Some("higher-priority-content"));
        assert_eq!(results[0].call_id, Some("call-1"));
        assert_eq!(results[0].tool_name, None);
        assert_eq!(results[0].outcome.outcome, OutputOutcome::Success);

        let redacted = json!({
            "type": "tool_result",
            "sessionId": "qwen-parser",
            "redacted": true,
            "message": {
                "role": "tool",
                "content": [
                    {"type": "tool_result", "content": "must-not-escape"},
                    {"type": "tool_result", "content": "must-not-escape-either"}
                ]
            }
        });
        let redacted_results = enumerate_qwen_code_results(&redacted).unwrap();
        assert_eq!(redacted_results.len(), 2);
        assert!(redacted_results
            .iter()
            .all(|result| result.content.is_none()));

        let failed = tool_result(
            "qwen-parser",
            "failed",
            "diagnostic-retained-by-core-policy",
            true,
        );
        let failed_results = enumerate_qwen_code_results(&failed).unwrap();
        assert_eq!(failed_results[0].outcome.outcome, OutputOutcome::Failure);
        assert_eq!(
            qwen_code_event_type(&failed),
            ctx_history_core::EventType::ToolOutput
        );
    }

    struct RecordingSink {
        store_path: PathBuf,
        fail_pages: AtomicBool,
        progress: Mutex<Option<ProOutputProgress>>,
        pages: AtomicUsize,
        outputs: AtomicUsize,
        behind: AtomicUsize,
        saw_core_before_page: AtomicBool,
    }

    impl RecordingSink {
        fn new(store_path: PathBuf, fail_pages: bool) -> Self {
            Self {
                store_path,
                fail_pages: AtomicBool::new(fail_pages),
                progress: Mutex::new(None),
                pages: AtomicUsize::new(0),
                outputs: AtomicUsize::new(0),
                behind: AtomicUsize::new(0),
                saw_core_before_page: AtomicBool::new(false),
            }
        }
    }

    impl ProOutputSink for RecordingSink {
        fn inventory_generation(&self) -> u64 {
            1
        }

        fn materializer_revision(&self) -> &str {
            "qwen-code-nativepath-test-materializer-v1"
        }

        fn observe_source(
            &self,
            _source: &OutputSourceIdentity,
        ) -> std::result::Result<Option<ProOutputProgress>, ProOutputSinkError> {
            Ok(self.progress.lock().unwrap().clone())
        }

        fn materialize_page(
            &self,
            page: ProOutputMaterializationPage,
        ) -> std::result::Result<ProOutputPageResult, ProOutputSinkError> {
            let core = Store::open_read_only(&self.store_path)
                .map_err(|error| ProOutputSinkError::new("test_store", error.to_string()))?;
            if !core
                .list_sessions()
                .map_err(|error| ProOutputSinkError::new("test_sessions", error.to_string()))?
                .is_empty()
            {
                self.saw_core_before_page.store(true, Ordering::SeqCst);
            }
            self.pages.fetch_add(1, Ordering::SeqCst);
            self.outputs
                .fetch_add(page.observations.len(), Ordering::SeqCst);
            if self.fail_pages.load(Ordering::SeqCst) {
                return Err(ProOutputSinkError::new(
                    "injected_qwen_output_failure",
                    "injected output materialization failure",
                ));
            }
            let committed_cursor = page.next_safe_cursor.clone();
            *self.progress.lock().unwrap() = Some(ProOutputProgress {
                source_epoch: page.source_epoch,
                observed_revision: page.observed_revision.clone(),
                cursor: Some(committed_cursor.clone()),
                parser_revision: page.parser_revision.clone(),
                materializer_revision: page.materializer_revision.clone(),
                terminal: page.terminal,
            });
            Ok(ProOutputPageResult {
                source_epoch: page.source_epoch,
                committed_cursor,
                accepted_outputs: u32::try_from(page.observations.len()).unwrap(),
                materialized_facts: 0,
                replayed: false,
            })
        }

        fn mark_behind(&self, _error: ProOutputSinkError) {
            self.behind.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn import(
        root: &Path,
        store: &mut Store,
        import_profile: ImportProfile,
    ) -> ProviderImportSummary {
        import_qwen_code_nativepath_tree(
            store,
            NativePathJsonlTreeImport {
                path: root,
                machine_id: MACHINE.to_owned(),
                source_path: Some(root.to_path_buf()),
                source_root: None,
                imported_at: "2026-07-25T12:00:00Z".parse().unwrap(),
                history_record_id: None,
                capture_work_limit: CaptureWorkLimit::Drain,
                inventory_observation_token: None,
                import_profile,
            },
        )
        .unwrap()
    }

    fn qwen_session(store: &Store, provider_session_id: &str) -> ctx_history_core::Session {
        store
            .list_sessions()
            .unwrap()
            .into_iter()
            .find(|session| {
                session.provider == CaptureProvider::QwenCode
                    && session.external_session_id.as_deref() == Some(provider_session_id)
            })
            .unwrap()
    }

    fn transcript_path(root: &Path) -> PathBuf {
        root.join("sanitized-workspace/chats/qwen-life.jsonl")
    }

    fn message(session_id: &str, id: &str, kind: &str, content: &str) -> Value {
        json!({
            "uuid": id,
            "sessionId": session_id,
            "timestamp": "2026-07-25T12:00:01Z",
            "type": kind,
            "cwd": "/workspace/qwen",
            "message": {
                "role": kind,
                "content": [{"type": "text", "text": content}]
            },
            "model": "qwen3-coder",
        })
    }

    fn tool_call(session_id: &str, id: &str) -> Value {
        json!({
            "uuid": id,
            "sessionId": session_id,
            "timestamp": "2026-07-25T12:00:02Z",
            "type": "assistant",
            "cwd": "/workspace/qwen",
            "message": {
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "call-1",
                    "name": "Write",
                    "input": {"path": "src/qwen.txt", "content": "proof"}
                }]
            },
            "model": "qwen3-coder",
        })
    }

    fn tool_result(session_id: &str, id: &str, result: &str, is_error: bool) -> Value {
        json!({
            "uuid": id,
            "sessionId": session_id,
            "timestamp": "2026-07-25T12:00:03Z",
            "type": "tool_result",
            "cwd": "/workspace/qwen",
            "message": {
                "role": "tool",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "call-1",
                    "content": result,
                    "is_error": is_error
                }]
            },
            "toolCallResult": {
                "tool": "Write",
                "path": "src/qwen.txt",
                "output": "lower-priority-output",
                "is_error": is_error
            },
            "model": "qwen3-coder",
        })
    }

    fn write_transcript(path: &Path, records: &[Value]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut bytes = Vec::new();
        for record in records {
            serde_json::to_writer(&mut bytes, record).unwrap();
            bytes.push(b'\n');
        }
        fs::write(path, bytes).unwrap();
    }

    fn append_record(path: &Path, record: &Value) {
        let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
        serde_json::to_writer(&mut file, record).unwrap();
        file.write_all(b"\n").unwrap();
    }

    fn checkpoint(store: &Store, path: &Path) -> DirectJsonlCheckpoint {
        let canonical = fs::canonicalize(path).unwrap();
        let locator = provider_path_identity(&canonical).unwrap();
        let stream = provider_source_cursor_stream_for_path(
            CaptureProvider::QwenCode,
            QWEN_CODE_SOURCE_FORMAT,
            &locator,
        );
        let cursor = store
            .get_sync_cursor(None, MACHINE, &stream)
            .unwrap()
            .unwrap();
        decode_direct_jsonl_native_cursor(
            &cursor.cursor,
            CaptureProvider::QwenCode,
            QWEN_CODE_SOURCE_FORMAT,
        )
        .unwrap()
    }

    fn classify(
        path: &Path,
        root: &Path,
        previous: &DirectJsonlCheckpoint,
    ) -> DirectJsonlSourceChange {
        open_direct_jsonl_pages(
            CaptureProvider::QwenCode,
            QWEN_CODE_SOURCE_FORMAT,
            path,
            Some(root.to_path_buf()),
            "2026-07-25T12:01:00Z".parse().unwrap(),
            false,
            Some(previous),
        )
        .unwrap()
        .source_change()
    }
}
