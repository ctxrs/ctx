use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    Confidence, Event, Fidelity, FileChangeKind, FileTouched, Session, SessionEdge,
    SessionEdgeType, SessionStatus, SyncCursor,
};
use ctx_history_store::{
    CanonicalActor, EventSearchBulkGuard, NativePathCursorSetClassification,
    NativePathCursorTransition, NativePathGroupAccounting, ProviderSourceLocatorObservation,
    ProviderSourceRouteRetirement, ProviderSourceRouteRetirementDisposition,
    ProviderSourceRouteRetirementReason, Store, StoreError,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::provider::{
    importer::{
        provider_event_import_identity_with_exact_legacy_source, provider_file_touch_import_id,
        provider_import_session_uuid, provider_path_identity, provider_scoped_source_identity_key,
        provider_scoped_source_uuid, provider_source_cursor_stream_for_path,
        provider_source_event_import_identity, provider_source_identity, provider_sync_metadata,
        timestamps, ProviderEventImportIdentity,
    },
    native_ingestion::{
        process_pro_replay_only, NativePageAccounting, NativeProOutputPage, NativeProReplayPage,
        NativeSafeFrontier, NativeSourceIdentity,
    },
    providers::gemini::nativepath::{
        discover_gemini_transcripts, read_gemini_transcript_pages_with_profile, GeminiCheckpoint,
        GeminiEventIdentity, GeminiFileObservation, GeminiNativePage, GeminiNativePathProfile,
        GeminiPageFrontier, GeminiPageIdentity, GeminiPreviousSource, GeminiRetainedEvent,
        GeminiScanError, GeminiScanOutcome, GeminiSession, GeminiSourceChange,
        GeminiTranscriptSource, GEMINI_NATIVEPATH_PARSER_REVISION,
        GEMINI_NATIVEPATH_POLICY_REVISION,
    },
};
use crate::{
    stable_capture_uuid, CaptureError, CaptureWorkLimit, OutputSourceIdentity, ProOutputProgress,
    ProOutputSink, ProOutputSinkError, ProOutputSourceDisposition, ProviderImportFailure,
    ProviderImportSummary, ProviderImportWorkResult, Result, GEMINI_CLI_SOURCE_FORMAT,
};

use super::NativePathJsonlTreeImport;

const GEMINI_CURSOR_VERSION: u32 = 1;
const GEMINI_PUBLICATION_DOMAIN: &[u8] = b"ctx-gemini-nativepath-publication-v1\0";
const GEMINI_GROUP_MAX_PAGES: usize = 32;
const GEMINI_GROUP_MAX_SOURCES: usize = 64;
const GEMINI_GROUP_MAX_BYTES: usize = 6 * 1024 * 1024;
const GEMINI_GROUP_MAX_ESTIMATED_MUTATIONS: usize = 3_000;
const GEMINI_OUTPUT_FRONTIER_VERSION: u32 = 1;
const GEMINI_OUTPUT_PARSER_REVISION: &str = "gemini-nativepath-output-v6-p4";
const GEMINI_EVENT_INDEX_DOMAIN: &[u8] = b"ctx-gemini-nativepath-event-index-v1\0";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GeminiCursorWire {
    version: u32,
    kind: String,
    checkpoint: GeminiCheckpoint,
}

struct GeminiPendingPage {
    source: GeminiTranscriptSource,
    page: GeminiNativePage,
    next_checkpoint: GeminiCheckpoint,
    output_pages: Vec<NativeProReplayPage>,
}

struct GeminiPublicationContext<'a> {
    machine_id: &'a str,
    source_root: &'a Path,
    imported_at: DateTime<Utc>,
    history_record_id: Option<Uuid>,
}

struct ResolvedGeminiSource {
    source_id: Uuid,
    session: Option<Session>,
}

struct GeminiEventPublicationIdentity {
    identity: ProviderEventImportIdentity,
    provider_event_index: u64,
    released_provider_event_index: u64,
    exact_released_hash: String,
    preserves_released_position: bool,
}

#[derive(Clone)]
struct GeminiKnownRoute {
    path: PathBuf,
    locator_identity: String,
    canonical_source_identity: String,
    source_revision: String,
    current_cursor: SyncCursor,
    checkpoint: GeminiCheckpoint,
}

pub(crate) fn import_gemini_nativepath_tree(
    store: &mut Store,
    request: NativePathJsonlTreeImport<'_>,
) -> Result<ProviderImportSummary> {
    let configured_source_root = request
        .source_root
        .clone()
        .or(request.source_path.clone())
        .unwrap_or_else(|| request.path.to_path_buf());
    let known_routes = known_gemini_routes(store, &request.machine_id, &configured_source_root)?;
    let discovery = match discover_gemini_transcripts(request.path) {
        Ok(discovery) => Some(discovery),
        Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let sink = request.import_profile.sink().cloned();
    if request.import_profile.is_replay_only() {
        let Some(discovery) = discovery else {
            return Ok(ProviderImportSummary::default());
        };
        let sink = sink.as_deref().ok_or(CaptureError::SystemInvariant(
            "Gemini replay-only profile has no output sink",
        ))?;
        for source in &discovery.transcripts {
            replay_gemini_source_outputs_or_mark_behind(
                store,
                &request.machine_id,
                &configured_source_root,
                source,
                sink,
            );
        }
        return Ok(ProviderImportSummary::default());
    }

    let Some(discovery) = discovery else {
        return retire_missing_gemini_routes(
            store,
            &request.machine_id,
            request.imported_at,
            &known_routes,
            &BTreeSet::new(),
            ProviderSourceRouteRetirementReason::RootMissing,
        );
    };
    if !discovery.completed_inventory {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    if discovery.transcripts.is_empty() {
        if known_routes.is_empty() {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: request.path.to_path_buf(),
                reason: super::super::dialect::native_jsonl_missing_reason(CaptureProvider::Gemini),
            });
        }
        return retire_missing_gemini_routes(
            store,
            &request.machine_id,
            request.imported_at,
            &known_routes,
            &BTreeSet::new(),
            ProviderSourceRouteRetirementReason::SourceMissing,
        );
    }
    let live_paths = discovery
        .transcripts
        .iter()
        .map(|source| source.path.clone())
        .collect::<BTreeSet<_>>();

    let committed_store = Store::open_read_only(store.path())?;
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let context = GeminiPublicationContext {
        machine_id: &request.machine_id,
        source_root: &configured_source_root,
        imported_at: request.imported_at,
        history_record_id: request.history_record_id,
    };
    let mut accumulator = GeminiGroupAccumulator::new(
        store,
        &committed_store,
        &bulk_guard,
        context,
        request.capture_work_limit,
        sink.as_deref(),
    );

    let operation = (|| {
        let mut catch_up_sources = Vec::new();
        for source in discovery.transcripts {
            if accumulator.stopped {
                break;
            }
            let path_identity = provider_path_identity(&source.path)?;
            let stream = provider_source_cursor_stream_for_path(
                CaptureProvider::Gemini,
                GEMINI_CLI_SOURCE_FORMAT,
                &path_identity,
            );
            let stored = accumulator
                .store
                .get_sync_cursor(None, &request.machine_id, &stream)?;
            let core_checkpoint = stored
                .as_ref()
                .map(|cursor| decode_gemini_cursor(&cursor.cursor))
                .transpose()?
                .flatten();
            let previous = core_checkpoint
                .clone()
                .map(|checkpoint| GeminiPreviousSource {
                    checkpoint,
                    prior_route_still_live: true,
                });
            let mut output_progress = None;
            let mut single_scan_output = false;
            let output_source = sink.as_ref().map(|_| OutputSourceIdentity {
                provider: CaptureProvider::Gemini.as_str().to_owned(),
                namespace_id: configured_source_root.display().to_string(),
                source_id: path_identity.clone(),
            });
            if let (Some(sink), Some(output_source)) = (sink.as_deref(), output_source.as_ref()) {
                match sink.observe_source(output_source) {
                    Ok(progress) => {
                        let output_checkpoint =
                            resumable_output_checkpoint(progress.as_ref(), sink, &source);
                        single_scan_output = output_lane_is_aligned(
                            core_checkpoint.as_ref(),
                            progress.as_ref(),
                            output_checkpoint.as_ref(),
                            sink,
                        );
                        output_progress = progress;
                        if !single_scan_output {
                            catch_up_sources.push(source.clone());
                        }
                    }
                    Err(error) => sink.mark_behind(error),
                }
            }
            let profile = if single_scan_output {
                GeminiNativePathProfile::CoreAndTransientOutputs
            } else {
                GeminiNativePathProfile::CoreOnly
            };
            let mut reader =
                read_gemini_transcript_pages_with_profile(&source, previous.as_ref(), profile)
                    .map_err(gemini_scan_error)?;
            let mut output_state = match (sink.as_deref(), output_source, single_scan_output) {
                (Some(sink), Some(output_source), true) => {
                    match GeminiOutputState::new(
                        output_source,
                        output_progress,
                        reader.resumed_from_previous(),
                        sink.materializer_revision(),
                    ) {
                        Ok(state) => Some(state),
                        Err(error) => {
                            sink.mark_behind(ProOutputSinkError::new(
                                "gemini_nativepath_output_state",
                                error.to_string(),
                            ));
                            None
                        }
                    }
                }
                _ => None,
            };
            let mut emitted = false;
            while let Some(mut page) = reader.next_page().map_err(gemini_scan_error)? {
                emitted = true;
                let next_checkpoint = reader
                    .outcome()
                    .map(|outcome| outcome.checkpoint.clone())
                    .unwrap_or_else(|| checkpoint_from_frontier(&source, &page.next_safe_frontier));
                let output_pages = match (sink.as_deref(), output_state.as_mut()) {
                    (Some(sink), Some(state)) => match adapt_gemini_output_pages(
                        &source,
                        &mut page,
                        &next_checkpoint,
                        state,
                        sink,
                    ) {
                        Ok(pages) => pages,
                        Err(error) => {
                            sink.mark_behind(ProOutputSinkError::new(
                                "gemini_nativepath_output_adaptation",
                                error.to_string(),
                            ));
                            output_state = None;
                            Vec::new()
                        }
                    },
                    _ => Vec::new(),
                };
                accumulator.push(GeminiPendingPage {
                    source: source.clone(),
                    page,
                    next_checkpoint,
                    output_pages,
                })?;
                if accumulator.stopped {
                    break;
                }
            }
            if accumulator.stopped {
                break;
            }
            let outcome = reader.outcome().ok_or(CaptureError::SystemInvariant(
                "Gemini NativePath reader completed without an outcome",
            ))?;
            if !outcome.signals.cursor_advance_allowed {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            if !emitted {
                if outcome.signals.source_change == GeminiSourceChange::Unchanged {
                    accumulator.record_unchanged(outcome);
                } else {
                    accumulator.push(GeminiPendingPage {
                        source: source.clone(),
                        page: observation_only_page(&outcome.checkpoint),
                        next_checkpoint: outcome.checkpoint.clone(),
                        output_pages: Vec::new(),
                    })?;
                }
            }
        }
        let summary = accumulator.finish()?;
        if !accumulator.stopped {
            if let Some(sink) = sink.as_deref() {
                for source in catch_up_sources {
                    replay_gemini_source_outputs_or_mark_behind(
                        accumulator.store,
                        &request.machine_id,
                        &configured_source_root,
                        &source,
                        sink,
                    );
                }
            }
        }
        Ok(summary)
    })();
    let stopped = accumulator.stopped;
    drop(accumulator);
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    match (operation, finish) {
        (Ok(mut summary), Ok(())) => {
            if stopped {
                summary.work_remaining = true;
            } else {
                summary.merge_from(retire_missing_gemini_routes(
                    store,
                    &request.machine_id,
                    request.imported_at,
                    &known_routes,
                    &live_paths,
                    ProviderSourceRouteRetirementReason::SourceMissing,
                )?);
            }
            Ok(summary)
        }
        (_, Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
    }
}

fn checkpoint_from_frontier(
    source: &GeminiTranscriptSource,
    frontier: &GeminiPageFrontier,
) -> GeminiCheckpoint {
    GeminiCheckpoint {
        parser_revision: frontier.parser_revision,
        policy_revision: frontier.policy_revision,
        source_path: source.path.clone(),
        source_observation: source.observation.clone(),
        session: frontier.session.clone(),
        complete_prefix_end: frontier.complete_prefix_end,
        complete_prefix_sha256: frontier.complete_prefix_sha256,
        source_sha256: frontier.complete_prefix_sha256,
        next_raw_ordinal: frontier.next_raw_ordinal,
        retained_event_count: frontier.retained_event_count,
        rejected_records: frontier.rejected_records,
        append_boundary_safe: frontier.append_boundary_safe,
        terminal: false,
    }
}

fn observation_only_page(checkpoint: &GeminiCheckpoint) -> GeminiNativePage {
    let frontier = GeminiPageFrontier {
        parser_revision: checkpoint.parser_revision,
        policy_revision: checkpoint.policy_revision,
        complete_prefix_end: checkpoint.complete_prefix_end,
        complete_prefix_sha256: checkpoint.complete_prefix_sha256,
        source_device: checkpoint.source_observation.device,
        source_inode: checkpoint.source_observation.inode,
        next_raw_ordinal: checkpoint.next_raw_ordinal,
        retained_event_count: checkpoint.retained_event_count,
        rejected_records: checkpoint.rejected_records,
        append_boundary_safe: checkpoint.append_boundary_safe,
        session: checkpoint.session.clone(),
    };
    GeminiNativePage {
        identity: GeminiPageIdentity([0; 32]),
        expected_frontier: frontier.clone(),
        next_safe_frontier: frontier,
        terminal: checkpoint.terminal,
        events: Vec::new(),
        output_pages: Vec::new(),
        rejections: Vec::new(),
        physical_records: 0,
        logical_units: 1,
        retained_event_bytes: 0,
        conservative_serialized_bytes: 4 * 1024,
    }
}

fn resumable_output_checkpoint(
    progress: Option<&ProOutputProgress>,
    sink: &dyn ProOutputSink,
    source: &GeminiTranscriptSource,
) -> Option<GeminiCheckpoint> {
    let progress = progress?;
    if progress.parser_revision != GEMINI_OUTPUT_PARSER_REVISION
        || progress.materializer_revision != sink.materializer_revision()
    {
        return None;
    }
    let cursor = progress.cursor.as_ref()?;
    if cursor.version != GEMINI_OUTPUT_FRONTIER_VERSION {
        return None;
    }
    serde_json::from_slice::<GeminiCheckpoint>(&cursor.payload)
        .ok()
        .filter(|checkpoint| {
            checkpoint.source_path == source.path
                && checkpoint.parser_revision == GEMINI_NATIVEPATH_PARSER_REVISION
                && checkpoint.policy_revision == GEMINI_NATIVEPATH_POLICY_REVISION
                && progress.observed_revision
                    == gemini_source_revision(&checkpoint.source_observation)
        })
}

fn output_lane_is_aligned(
    core: Option<&GeminiCheckpoint>,
    progress: Option<&ProOutputProgress>,
    output: Option<&GeminiCheckpoint>,
    sink: &dyn ProOutputSink,
) -> bool {
    match (core, progress, output) {
        (None, None, None) => true,
        (Some(core), Some(progress), Some(output)) => {
            progress.parser_revision == GEMINI_OUTPUT_PARSER_REVISION
                && progress.materializer_revision == sink.materializer_revision()
                && core == output
        }
        _ => false,
    }
}

struct GeminiOutputState {
    source: OutputSourceIdentity,
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_sink_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
}

impl GeminiOutputState {
    fn new(
        source: OutputSourceIdentity,
        progress: Option<ProOutputProgress>,
        resumed_from_previous: bool,
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
        let expected_sink_frontier = progress
            .cursor
            .as_ref()
            .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload.clone()))
            .transpose()
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        let rewrite = !resumed_from_previous
            || progress.parser_revision != GEMINI_OUTPUT_PARSER_REVISION
            || progress.materializer_revision != materializer_revision;
        Ok(Self {
            source,
            source_epoch: if rewrite {
                progress
                    .source_epoch
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Gemini output source epoch exhausted",
                    ))?
            } else {
                progress.source_epoch
            },
            expected_source_epoch: Some(progress.source_epoch),
            expected_sink_frontier,
            disposition: if rewrite {
                ProOutputSourceDisposition::Rewrite
            } else {
                ProOutputSourceDisposition::AppendOrResume
            },
        })
    }
}

fn output_safe_frontier(checkpoint: &GeminiCheckpoint) -> Result<NativeSafeFrontier> {
    NativeSafeFrontier::new(
        GEMINI_OUTPUT_FRONTIER_VERSION,
        serde_json::to_vec(checkpoint)?,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

fn adapt_gemini_output_pages(
    source: &GeminiTranscriptSource,
    page: &mut GeminiNativePage,
    next_checkpoint: &GeminiCheckpoint,
    state: &mut GeminiOutputState,
    sink: &dyn ProOutputSink,
) -> Result<Vec<NativeProReplayPage>> {
    let output_pages = std::mem::take(&mut page.output_pages);
    let output_page_count = output_pages.len();
    let expected_checkpoint = checkpoint_from_frontier(source, &page.expected_frontier);
    let expected_frontier = output_safe_frontier(&expected_checkpoint)?;
    let next_safe_frontier = output_safe_frontier(next_checkpoint)?;
    let native_source_identity =
        NativeSourceIdentity::new(CaptureProvider::Gemini.as_str(), &state.source.source_id);
    let observed_revision = gemini_source_revision(&source.observation);
    let mut adapted = Vec::with_capacity(output_page_count);
    for (index, output_page) in output_pages.into_iter().enumerate() {
        let output = NativeProOutputPage {
            inventory_generation: sink.inventory_generation(),
            source: state.source.clone(),
            source_epoch: state.source_epoch,
            observed_revision: observed_revision.clone(),
            parser_revision: GEMINI_OUTPUT_PARSER_REVISION.to_owned(),
            materializer_revision: sink.materializer_revision().to_owned(),
            disposition: state.disposition,
            expected_prior_source_epoch: state.expected_source_epoch,
            expected_prior_frontier: state.expected_sink_frontier.clone(),
            observations: output_page.outputs,
        };
        let replay = NativeProReplayPage::new_with_source_identity(
            native_source_identity.clone(),
            expected_frontier.clone(),
            next_safe_frontier.clone(),
            page.terminal && index.saturating_add(1) == output_page_count,
            NativePageAccounting {
                logical_units: output_page.logical_units.max(1),
                conservative_serialized_bytes: output_page.conservative_serialized_bytes,
            },
            output,
        )
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        state.expected_source_epoch = Some(state.source_epoch);
        state.expected_sink_frontier = Some(next_safe_frontier.clone());
        state.disposition = ProOutputSourceDisposition::AppendOrResume;
        adapted.push(replay);
    }
    Ok(adapted)
}

fn replay_committed_gemini_outputs(
    store: &Store,
    machine_id: &str,
    pages: &mut [GeminiPendingPage],
    sink: &dyn ProOutputSink,
    failed_sources: &mut BTreeSet<PathBuf>,
) {
    for pending in pages {
        if pending.output_pages.is_empty() || failed_sources.contains(&pending.source.path) {
            continue;
        }
        let authority = committed_gemini_core_covers(
            store,
            machine_id,
            &pending.source,
            &pending.next_checkpoint,
        );
        if !matches!(authority, Ok(true)) {
            sink.mark_behind(ProOutputSinkError::new(
                "gemini_nativepath_core_authority",
                authority.err().map_or_else(
                    || "committed Core cursor is behind".to_owned(),
                    |error| error.to_string(),
                ),
            ));
            failed_sources.insert(pending.source.path.clone());
            continue;
        }
        if !matches!(
            GeminiFileObservation::from_metadata(pending.source.source_file.metadata()),
            Ok(observation)
                if observation == pending.source.observation
                    && pending.source.source_file.revalidate().is_ok()
        ) {
            sink.mark_behind(ProOutputSinkError::new(
                "gemini_nativepath_output_source_changed",
                "Gemini source changed after Core committed and before output replay",
            ));
            failed_sources.insert(pending.source.path.clone());
            continue;
        }
        for replay in pending.output_pages.drain(..) {
            if process_pro_replay_only(replay, sink).is_err() {
                failed_sources.insert(pending.source.path.clone());
                break;
            }
        }
    }
}

fn committed_gemini_core_covers(
    store: &Store,
    machine_id: &str,
    source: &GeminiTranscriptSource,
    candidate: &GeminiCheckpoint,
) -> Result<bool> {
    let locator_identity = provider_path_identity(&source.path)?;
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Gemini,
        GEMINI_CLI_SOURCE_FORMAT,
        &locator_identity,
    );
    let committed = store
        .get_sync_cursor(None, machine_id, &stream)?
        .as_ref()
        .map(|cursor| decode_gemini_cursor(&cursor.cursor))
        .transpose()?
        .flatten();
    Ok(committed
        .as_ref()
        .is_some_and(|checkpoint| gemini_checkpoint_covers(checkpoint, candidate)))
}

fn gemini_checkpoint_covers(committed: &GeminiCheckpoint, candidate: &GeminiCheckpoint) -> bool {
    committed.parser_revision == candidate.parser_revision
        && committed.policy_revision == candidate.policy_revision
        && committed.source_path == candidate.source_path
        && committed.source_observation == candidate.source_observation
        && committed.complete_prefix_end >= candidate.complete_prefix_end
        && committed.next_raw_ordinal >= candidate.next_raw_ordinal
        && committed.retained_event_count >= candidate.retained_event_count
        && committed.rejected_records >= candidate.rejected_records
        && (committed.complete_prefix_end != candidate.complete_prefix_end
            || committed.complete_prefix_sha256 == candidate.complete_prefix_sha256)
        && (!candidate.terminal || committed.terminal)
}

fn replay_gemini_source_outputs_or_mark_behind(
    store: &Store,
    machine_id: &str,
    source_root: &Path,
    source: &GeminiTranscriptSource,
    sink: &dyn ProOutputSink,
) {
    if let Err(error) = replay_gemini_source_outputs(store, machine_id, source_root, source, sink) {
        sink.mark_behind(ProOutputSinkError::new(
            "gemini_nativepath_output_replay",
            error.to_string(),
        ));
    }
}

fn replay_gemini_source_outputs(
    store: &Store,
    machine_id: &str,
    source_root: &Path,
    source: &GeminiTranscriptSource,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    let locator_identity = provider_path_identity(&source.path)?;
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Gemini,
        GEMINI_CLI_SOURCE_FORMAT,
        &locator_identity,
    );
    let core_checkpoint = store
        .get_sync_cursor(None, machine_id, &stream)?
        .as_ref()
        .map(|cursor| decode_gemini_cursor(&cursor.cursor))
        .transpose()?
        .flatten()
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Gemini output replay requires committed NativePath Core".to_owned(),
            )
        })?;
    if core_checkpoint.source_path != source.path
        || core_checkpoint.source_observation != source.observation
    {
        return Err(CaptureError::InvalidPayload(
            "Gemini output replay source no longer matches committed Core authority".to_owned(),
        ));
    }
    let output_source = OutputSourceIdentity {
        provider: CaptureProvider::Gemini.as_str().to_owned(),
        namespace_id: source_root.display().to_string(),
        source_id: locator_identity,
    };
    let progress = match sink.observe_source(&output_source) {
        Ok(progress) => progress,
        Err(error) => {
            sink.mark_behind(error);
            return Ok(());
        }
    };
    let output_checkpoint = resumable_output_checkpoint(progress.as_ref(), sink, source);
    let previous = output_checkpoint.map(|checkpoint| GeminiPreviousSource {
        checkpoint,
        prior_route_still_live: true,
    });
    let mut reader = read_gemini_transcript_pages_with_profile(
        source,
        previous.as_ref(),
        GeminiNativePathProfile::CoreAndTransientOutputs,
    )
    .map_err(gemini_scan_error)?;
    let mut state = GeminiOutputState::new(
        output_source,
        progress,
        reader.resumed_from_previous(),
        sink.materializer_revision(),
    )?;
    while let Some(mut page) = reader.next_page().map_err(gemini_scan_error)? {
        let next_checkpoint = reader
            .outcome()
            .map(|outcome| outcome.checkpoint.clone())
            .unwrap_or_else(|| checkpoint_from_frontier(source, &page.next_safe_frontier));
        if !gemini_checkpoint_covers(&core_checkpoint, &next_checkpoint) {
            return Err(CaptureError::InvalidPayload(
                "Gemini output replay advanced beyond committed Core authority".to_owned(),
            ));
        }
        let output_pages =
            adapt_gemini_output_pages(source, &mut page, &next_checkpoint, &mut state, sink)?;
        for replay in output_pages {
            if process_pro_replay_only(replay, sink).is_err() {
                return Ok(());
            }
        }
    }
    let outcome = reader.outcome().ok_or(CaptureError::SystemInvariant(
        "Gemini output replay reader completed without an outcome",
    ))?;
    if !gemini_checkpoint_covers(&core_checkpoint, &outcome.checkpoint) {
        return Err(CaptureError::InvalidPayload(
            "Gemini output replay outcome exceeded committed Core authority".to_owned(),
        ));
    }
    Ok(())
}
#[path = "gemini_publication.rs"]
mod publication;
#[cfg(test)]
use publication::released_gemini_event_index;
use publication::GeminiGroupAccumulator;

#[path = "gemini_routing.rs"]
mod routing;
use routing::*;

#[cfg(test)]
#[path = "gemini_tests.rs"]
mod tests;
