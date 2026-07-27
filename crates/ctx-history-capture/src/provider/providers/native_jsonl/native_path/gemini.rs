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
    NativePathCursorTransition, NativePathGroupAccounting, ProviderEventHashAuthority,
    ProviderSourceLocatorObservation, Store,
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
        provider_source_identity, provider_sync_metadata, timestamps,
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
const GEMINI_OUTPUT_PARSER_REVISION: &str = "gemini-nativepath-output-v5-p3";

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

pub(crate) fn import_gemini_nativepath_tree(
    store: &mut Store,
    request: NativePathJsonlTreeImport<'_>,
) -> Result<ProviderImportSummary> {
    let configured_source_root = request
        .source_root
        .clone()
        .or(request.source_path.clone())
        .unwrap_or_else(|| request.path.to_path_buf());
    let discovery = discover_gemini_transcripts(request.path)?;
    if !discovery.completed_inventory {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    if discovery.transcripts.is_empty() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: request.path.to_path_buf(),
            reason: super::super::dialect::native_jsonl_missing_reason(CaptureProvider::Gemini),
        });
    }
    let sink = request.import_profile.sink().cloned();
    if request.import_profile.is_replay_only() {
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
            GeminiFileObservation::read(&pending.source.path),
            Ok(observation) if observation == pending.source.observation
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

struct GeminiGroupAccumulator<'a> {
    store: &'a mut Store,
    committed_store: &'a Store,
    bulk_guard: &'a EventSearchBulkGuard,
    context: GeminiPublicationContext<'a>,
    work_limit: CaptureWorkLimit,
    pages: Vec<GeminiPendingPage>,
    sources: BTreeSet<PathBuf>,
    bytes: usize,
    estimated_mutations: usize,
    summary: ProviderImportSummary,
    output_sink: Option<&'a dyn ProOutputSink>,
    failed_output_sources: BTreeSet<PathBuf>,
    stopped: bool,
}

impl<'a> GeminiGroupAccumulator<'a> {
    fn new(
        store: &'a mut Store,
        committed_store: &'a Store,
        bulk_guard: &'a EventSearchBulkGuard,
        context: GeminiPublicationContext<'a>,
        work_limit: CaptureWorkLimit,
        output_sink: Option<&'a dyn ProOutputSink>,
    ) -> Self {
        Self {
            store,
            committed_store,
            bulk_guard,
            context,
            work_limit,
            pages: Vec::new(),
            sources: BTreeSet::new(),
            bytes: 0,
            estimated_mutations: 0,
            summary: ProviderImportSummary::default(),
            output_sink,
            failed_output_sources: BTreeSet::new(),
            stopped: false,
        }
    }

    fn push(&mut self, pending: GeminiPendingPage) -> Result<()> {
        let next_sources =
            self.sources.len() + usize::from(!self.sources.contains(&pending.source.path));
        let next_bytes = self
            .bytes
            .saturating_add(pending.page.conservative_serialized_bytes);
        let page_mutations = pending
            .page
            .events
            .iter()
            .map(|event| 1_usize.saturating_add(event.safe_file_touches.len()))
            .sum::<usize>()
            .saturating_add(4);
        let next_mutations = self.estimated_mutations.saturating_add(page_mutations);
        if !self.pages.is_empty()
            && (self.pages.len() >= GEMINI_GROUP_MAX_PAGES
                || next_sources > GEMINI_GROUP_MAX_SOURCES
                || next_bytes > GEMINI_GROUP_MAX_BYTES
                || next_mutations > GEMINI_GROUP_MAX_ESTIMATED_MUTATIONS)
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
        self.sources.insert(pending.source.path.clone());
        self.pages.push(pending);
        Ok(())
    }

    fn record_unchanged(&mut self, outcome: &GeminiScanOutcome) {
        let sessions = usize::from(outcome.checkpoint.session.is_some());
        let events = usize::try_from(outcome.checkpoint.retained_event_count).unwrap_or(usize::MAX);
        self.summary.skipped_sessions = self.summary.skipped_sessions.saturating_add(sessions);
        self.summary.skipped_events = self.summary.skipped_events.saturating_add(events);
        self.summary.skipped = self
            .summary
            .skipped
            .saturating_add(sessions)
            .saturating_add(events);
        for rejection in &outcome.rejections {
            self.summary.record_failure(ProviderImportFailure {
                line: usize::try_from(rejection.raw_ordinal)
                    .unwrap_or(usize::MAX)
                    .saturating_add(1),
                error: rejection.reason.clone(),
            });
        }
    }

    fn flush(&mut self) -> Result<()> {
        if self.pages.is_empty() {
            return Ok(());
        }
        let mut pages = std::mem::take(&mut self.pages);
        let summary = publish_gemini_group(
            self.store,
            self.committed_store,
            self.bulk_guard,
            &self.context,
            &pages,
        )?;
        self.summary.merge_from(summary);
        if let Some(sink) = self.output_sink {
            replay_committed_gemini_outputs(
                self.store,
                self.context.machine_id,
                &mut pages,
                sink,
                &mut self.failed_output_sources,
            );
        }
        self.sources.clear();
        self.bytes = 0;
        self.estimated_mutations = 0;
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

fn publish_gemini_group(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &GeminiPublicationContext<'_>,
    pages: &[GeminiPendingPage],
) -> Result<ProviderImportSummary> {
    let source_paths = pages
        .iter()
        .map(|pending| pending.source.path.clone())
        .collect::<BTreeSet<_>>();
    for path in &source_paths {
        revalidate_gemini_source(pages, path)?;
    }

    let mut transitions = Vec::with_capacity(source_paths.len());
    for path in &source_paths {
        let path_identity = provider_path_identity(path)?;
        let stream = provider_source_cursor_stream_for_path(
            CaptureProvider::Gemini,
            GEMINI_CLI_SOURCE_FORMAT,
            &path_identity,
        );
        let stored = store.get_sync_cursor(None, context.machine_id, &stream)?;
        let checkpoint = &pages
            .iter()
            .rev()
            .find(|pending| &pending.source.path == path)
            .expect("Gemini pending source exists")
            .next_checkpoint;
        transitions.push(NativePathCursorTransition::new(
            stored.as_ref().map(|cursor| cursor.cursor.clone()),
            provider_sync_cursor(
                context.machine_id,
                stream,
                encode_gemini_cursor(checkpoint)?,
                context.imported_at,
            ),
        ));
    }
    let publication_id = gemini_publication_id(pages, &transitions);
    let retained_bytes = pages.iter().fold(0_usize, |total, pending| {
        total.saturating_add(pending.page.conservative_serialized_bytes)
    });
    let accounting =
        NativePathGroupAccounting::new(pages.len(), source_paths.len(), retained_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    if matches!(
        group.classify_cursor_set(&publication_id, &transitions)?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }

    let mut summary = ProviderImportSummary::default();
    let mut resolved = BTreeMap::new();
    for path in &source_paths {
        let pending = pages
            .iter()
            .rev()
            .find(|pending| &pending.source.path == path)
            .expect("Gemini pending source exists");
        let session_fact = pending.next_checkpoint.session.as_ref();
        if session_fact.is_none() {
            // Rejection-only sources commit their path-scoped cursor without
            // inventing a canonical Core capture source.
            continue;
        }
        let raw_source_path = path.display().to_string();
        let source_root = context.source_root.display().to_string();
        let locator_identity = provider_path_identity(path)?;
        let proposed_source_identity = provider_source_identity(
            CaptureProvider::Gemini,
            GEMINI_CLI_SOURCE_FORMAT,
            Some(&source_root),
            Some(&raw_source_path),
            session_fact.map(|session| session.native_session_id.as_str()),
            &Value::Null,
        )
        .ok_or(CaptureError::SystemInvariant(
            "Gemini NativePath source has no canonical identity",
        ))?;
        let stream = provider_source_cursor_stream_for_path(
            CaptureProvider::Gemini,
            GEMINI_CLI_SOURCE_FORMAT,
            &locator_identity,
        );
        let revision = gemini_source_revision(&pending.source.observation);
        let resolution =
            group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
                provider: CaptureProvider::Gemini,
                source_format: GEMINI_CLI_SOURCE_FORMAT.to_owned(),
                machine_id: context.machine_id.to_owned(),
                locator_identity,
                cursor_stream: stream,
                proposed_source_identity,
                raw_source_path: Some(raw_source_path.clone()),
                source_revision: revision.clone(),
                observed_at_ms: context.imported_at.timestamp_millis(),
            })?;
        let source_id = match session_fact {
            Some(session) => committed_store
                .capture_source_by_canonical_identity_session(
                    CaptureProvider::Gemini,
                    GEMINI_CLI_SOURCE_FORMAT,
                    context.machine_id,
                    &resolution.canonical_source_identity,
                    &session.native_session_id,
                )?
                .map(|source| source.id)
                .unwrap_or_else(|| {
                    provider_scoped_source_uuid(
                        CaptureProvider::Gemini,
                        &session.native_session_id,
                        GEMINI_CLI_SOURCE_FORMAT,
                        Some(&raw_source_path),
                    )
                }),
            None => stable_capture_uuid(
                &format!(
                    "gemini-nativepath-source:{}:{}",
                    resolution.canonical_source_identity, raw_source_path
                ),
                "source",
            ),
        };
        group.upsert_capture_source(&gemini_capture_source(
            context,
            session_fact,
            source_id,
            &raw_source_path,
            &source_root,
            &resolution.canonical_source_identity,
            &revision,
        ))?;
        group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;

        let session = session_fact
            .map(|fact| {
                gemini_session(
                    committed_store,
                    context,
                    fact,
                    source_id,
                    &resolution.canonical_source_identity,
                )
            })
            .transpose()?;
        if let Some(session) = &session {
            let existed = committed_store.get_session(session.id).is_ok();
            if let Some(parent_id) = session.parent_session_id {
                if committed_store.get_session(parent_id).is_err() {
                    group.upsert_session(&gemini_parent_placeholder(
                        context,
                        source_id,
                        parent_id,
                        session_fact
                            .and_then(|fact| fact.parent_native_session_id.as_deref())
                            .unwrap_or("unknown-parent"),
                    ))?;
                }
            }
            group.upsert_session(session)?;
            if existed {
                summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
                summary.skipped = summary.skipped.saturating_add(1);
            } else {
                summary.imported_sessions = summary.imported_sessions.saturating_add(1);
                summary.imported = summary.imported.saturating_add(1);
            }
            if let Some(parent_id) = session.parent_session_id {
                let edge = gemini_relationship_edge(context, source_id, session, parent_id);
                let existed = committed_store.session_edge_exists(edge.id)?;
                group.upsert_projection_neutral_session_edge(&canonical_actor(session), &edge)?;
                if !existed {
                    summary.imported_edges = summary.imported_edges.saturating_add(1);
                    summary.imported = summary.imported.saturating_add(1);
                }
            }
        }
        resolved.insert(path.clone(), ResolvedGeminiSource { source_id, session });
    }

    for pending in pages {
        for rejection in &pending.page.rejections {
            summary.record_failure(ProviderImportFailure {
                line: usize::try_from(rejection.raw_ordinal)
                    .unwrap_or(usize::MAX)
                    .saturating_add(1),
                error: rejection.reason.clone(),
            });
        }
        if pending.page.events.is_empty() {
            continue;
        }
        let resolved = resolved
            .get(&pending.source.path)
            .ok_or(CaptureError::SystemInvariant(
                "Gemini publication lost its resolved source",
            ))?;
        for event in &pending.page.events {
            let session = resolved
                .session
                .as_ref()
                .ok_or(CaptureError::SystemInvariant(
                    "Gemini retained event has no canonical session",
                ))?;
            publish_gemini_event(
                &mut group,
                committed_store,
                context,
                resolved.source_id,
                session,
                event,
                &mut summary,
            )?;
        }
    }

    for path in &source_paths {
        revalidate_gemini_source(pages, path)?;
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

fn gemini_capture_source(
    context: &GeminiPublicationContext<'_>,
    session: Option<&GeminiSession>,
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
            provider: CaptureProvider::Gemini,
            machine_id: context.machine_id.to_owned(),
            process_id: None,
            cwd: session.and_then(|session| session.cwd.clone()),
            raw_source_path: Some(raw_source_path.to_owned()),
            source_format: Some(GEMINI_CLI_SOURCE_FORMAT.to_owned()),
            source_root: Some(source_root.to_owned()),
            source_identity: Some(source_identity.to_owned()),
            external_session_id: session.map(|session| session.native_session_id.clone()),
        },
        started_at: session
            .and_then(|session| session.started_at)
            .unwrap_or(context.imported_at),
        ended_at: None,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": session.map(|session| &session.native_session_id),
                "source_format": GEMINI_CLI_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "source_identity": source_identity,
                "source_revision": source_revision,
                "source_identity_key": session.map(|session| {
                    provider_scoped_source_identity_key(
                        CaptureProvider::Gemini,
                        &session.native_session_id,
                        GEMINI_CLI_SOURCE_FORMAT,
                        Some(raw_source_path),
                    )
                }),
            }),
        ),
    }
}

fn gemini_session(
    committed_store: &Store,
    context: &GeminiPublicationContext<'_>,
    fact: &GeminiSession,
    source_id: Uuid,
    source_identity: &str,
) -> Result<Session> {
    let id = provider_import_session_uuid(
        committed_store,
        CaptureProvider::Gemini,
        &fact.native_session_id,
        source_id,
        Some(source_identity),
    )?;
    let parent_session_id = fact
        .parent_native_session_id
        .as_deref()
        .map(|parent| {
            provider_import_session_uuid(
                committed_store,
                CaptureProvider::Gemini,
                parent,
                source_id,
                Some(source_identity),
            )
        })
        .transpose()?;
    Ok(Session {
        id,
        history_record_id: context.history_record_id,
        parent_session_id,
        root_session_id: parent_session_id,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Gemini,
        external_session_id: Some(fact.native_session_id.clone()),
        external_agent_id: None,
        agent_type: fact.agent_type,
        role_hint: Some(
            if fact.parent_native_session_id.is_some() || fact.agent_type == AgentType::Subagent {
                "subagent"
            } else {
                "primary"
            }
            .to_owned(),
        ),
        is_primary: fact.parent_native_session_id.is_none()
            && fact.agent_type != AgentType::Subagent,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: fact.started_at.unwrap_or(context.imported_at),
        ended_at: None,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": fact.native_session_id,
                "parent_provider_session_id": fact.parent_native_session_id,
                "native_kind": fact.native_kind,
                "source_format": GEMINI_CLI_SOURCE_FORMAT,
            }),
        ),
    })
}

fn gemini_parent_placeholder(
    context: &GeminiPublicationContext<'_>,
    source_id: Uuid,
    id: Uuid,
    external_session_id: &str,
) -> Session {
    Session {
        id,
        history_record_id: context.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Gemini,
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
                "source_format": GEMINI_CLI_SOURCE_FORMAT,
                "relationship_placeholder": true,
            }),
        ),
    }
}

fn gemini_relationship_edge(
    context: &GeminiPublicationContext<'_>,
    source_id: Uuid,
    session: &Session,
    parent_id: Uuid,
) -> SessionEdge {
    SessionEdge {
        id: stable_capture_uuid(
            &format!(
                "gemini-nativepath:{}:parent_child",
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
                "source_format": GEMINI_CLI_SOURCE_FORMAT,
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

#[allow(clippy::too_many_arguments)]
fn publish_gemini_event(
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    committed_store: &Store,
    context: &GeminiPublicationContext<'_>,
    source_id: Uuid,
    session: &Session,
    event: &GeminiRetainedEvent,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let provider_event_index = gemini_event_index(event)?;
    let event_hash = hex_digest(event.body_sha256);
    let identity = provider_event_import_identity_with_exact_legacy_source(
        committed_store,
        CaptureProvider::Gemini,
        session.external_session_id.as_deref().unwrap_or_default(),
        source_id,
        provider_event_index,
        provider_event_index,
        &event_hash,
        None,
        Some(event.native_order.raw_ordinal),
        session.id
            == crate::provider::importer::provider_session_uuid(
                CaptureProvider::Gemini,
                session.external_session_id.as_deref().unwrap_or_default(),
            ),
    )?;
    let dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, &event_hash)
            .unwrap_or(identity.dedupe_key);
    let occurred_at = event.occurred_at.unwrap_or(session.started_at);
    let normalized = Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id: context.history_record_id,
        session_id: Some(session.id),
        run_id: None,
        event_type: event.event_type,
        role: Some(event.role),
        occurred_at,
        capture_source_id: Some(source_id),
        payload: json!({
            "provider": CaptureProvider::Gemini.as_str(),
            "provider_session_id": session.external_session_id,
            "provider_event_index": provider_event_index,
            "provider_event_hash": event_hash,
            "native_identity": event.identity,
            "body": event.body,
            "preview": event.preview,
            "searchable_text": event.searchable_text,
            "artifacts": [],
        }),
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": session.external_session_id,
                "provider_event_index": provider_event_index,
                "provider_event_hash": event_hash,
                "provider_event_hash_authority": "provider_supplied",
                "source_format": GEMINI_CLI_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "cursor": match &event.identity {
                    GeminiEventIdentity::NativeRecordId(identity) => identity,
                },
                "fixture_line": event.native_order.raw_ordinal.saturating_add(1),
                "source_record_ordinal": event.native_order.raw_ordinal,
                "source_record_subrecord_index": event.native_order.sub_ordinal,
                "native_identity": event.identity,
            }),
        ),
    };
    if group.reconcile_provider_event(&normalized, ProviderEventHashAuthority::ProviderSupplied)? {
        summary.imported_events = summary.imported_events.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    } else {
        summary.skipped_events = summary.skipped_events.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    }
    summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);

    for (touch_ordinal, path) in event.safe_file_touches.iter().enumerate() {
        let packed_touch = event
            .native_order
            .raw_ordinal
            .checked_mul(u64::from(u16::MAX) + 1)
            .and_then(|base| base.checked_add(touch_ordinal as u64))
            .ok_or(CaptureError::SystemInvariant(
                "Gemini file-touch identity overflowed",
            ))?;
        let id = provider_file_touch_import_id(
            committed_store,
            CaptureProvider::Gemini,
            session.external_session_id.as_deref().unwrap_or_default(),
            source_id,
            Some(provider_event_index),
            packed_touch,
            session.id
                == crate::provider::importer::provider_session_uuid(
                    CaptureProvider::Gemini,
                    session.external_session_id.as_deref().unwrap_or_default(),
                ),
        )?;
        group.upsert_file_touched(&FileTouched {
            id,
            history_record_id: context.history_record_id,
            run_id: None,
            event_id: Some(normalized.id),
            vcs_workspace_id: None,
            path: path.clone(),
            change_kind: Some(FileChangeKind::Unknown),
            old_path: None,
            line_count_delta: None,
            confidence: Confidence::Explicit,
            timestamps: timestamps(occurred_at),
            source_id: Some(source_id),
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({
                    "provider": CaptureProvider::Gemini.as_str(),
                    "provider_session_id": session.external_session_id,
                    "provider_event_index": provider_event_index,
                    "source_format": GEMINI_CLI_SOURCE_FORMAT,
                }),
            ),
        })?;
    }
    Ok(())
}

fn gemini_event_index(event: &GeminiRetainedEvent) -> Result<u64> {
    if event.native_order.sub_ordinal == 0 {
        return Ok(event.native_order.raw_ordinal);
    }
    event
        .native_order
        .raw_ordinal
        .checked_mul(u64::from(u16::MAX) + 1)
        .and_then(|index| index.checked_add(u64::from(event.native_order.sub_ordinal)))
        .map(|index| index | (1_u64 << 63))
        .ok_or(CaptureError::SystemInvariant(
            "Gemini provider event identity index overflowed",
        ))
}

fn revalidate_gemini_source(pages: &[GeminiPendingPage], path: &Path) -> Result<()> {
    let expected = &pages
        .iter()
        .find(|pending| pending.source.path == path)
        .expect("Gemini pending source exists")
        .source
        .observation;
    if &GeminiFileObservation::read(path)? != expected {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    Ok(())
}

fn gemini_source_revision(observation: &GeminiFileObservation) -> String {
    let (side, seconds, nanos) = match observation.modified.duration_since(UNIX_EPOCH) {
        Ok(duration) => ('+', duration.as_secs(), duration.subsec_nanos()),
        Err(error) => {
            let duration = error.duration();
            ('-', duration.as_secs(), duration.subsec_nanos())
        }
    };
    format!(
        "gemini-nativepath-metadata-v1:length={};modified={side}{seconds}.{nanos:09};readonly={};device={};inode={}",
        observation.length,
        observation.readonly,
        observation
            .device
            .map_or_else(|| "none".to_owned(), |value| value.to_string()),
        observation
            .inode
            .map_or_else(|| "none".to_owned(), |value| value.to_string()),
    )
}

fn encode_gemini_cursor(checkpoint: &GeminiCheckpoint) -> Result<String> {
    Ok(serde_json::to_string(&GeminiCursorWire {
        version: GEMINI_CURSOR_VERSION,
        kind: "gemini-nativepath".to_owned(),
        checkpoint: checkpoint.clone(),
    })?)
}

fn decode_gemini_cursor(encoded_store_cursor: &str) -> Result<Option<GeminiCheckpoint>> {
    let encoded = ctx_history_store::decode_native_path_committed_cursor(encoded_store_cursor)
        .map(|cursor| cursor.provider_cursor().to_owned())
        .unwrap_or_else(|_| encoded_store_cursor.to_owned());
    let Ok(wire) = serde_json::from_str::<GeminiCursorWire>(&encoded) else {
        // Non-NativePath cursors reset into one authoritative NativePath scan.
        // The resulting commit emits only the current cursor format.
        return Ok(None);
    };
    Ok((wire.version == GEMINI_CURSOR_VERSION
        && wire.kind == "gemini-nativepath"
        && wire.checkpoint.parser_revision == GEMINI_NATIVEPATH_PARSER_REVISION
        && wire.checkpoint.policy_revision == GEMINI_NATIVEPATH_POLICY_REVISION)
        .then_some(wire.checkpoint))
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
                CaptureProvider::Gemini.as_str(),
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

fn gemini_publication_id(
    pages: &[GeminiPendingPage],
    transitions: &[NativePathCursorTransition],
) -> String {
    let mut digest = Sha256::new();
    digest.update(GEMINI_PUBLICATION_DOMAIN);
    digest.update((pages.len() as u64).to_be_bytes());
    for pending in pages {
        digest.update(pending.page.identity.as_bytes());
    }
    for transition in transitions {
        digest.update(transition.key().stream().as_bytes());
        if let Some(expected) = transition.expected_cursor() {
            digest.update((expected.len() as u64).to_be_bytes());
            digest.update(expected.as_bytes());
        }
        digest.update((transition.next().cursor.len() as u64).to_be_bytes());
        digest.update(transition.next().cursor.as_bytes());
    }
    format!("gemini-nativepath-v1:{:x}", digest.finalize())
}

fn gemini_scan_error(error: GeminiScanError) -> CaptureError {
    match error {
        GeminiScanError::Capture(error) => error,
        error => CaptureError::InvalidPayload(error.to_string()),
    }
}

fn hex_digest(digest: [u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc, Mutex,
        },
    };

    use ctx_history_core::EventType;
    use serde_json::{json, Value};
    use tempfile::tempdir;

    use super::*;
    use crate::{
        import_gemini_cli_history, GeminiCliImportOptions, OutputOutcome,
        ProOutputMaterializationPage, ProOutputObservation, ProOutputPageResult,
    };

    const MACHINE: &str = "gemini-production-route-proof";
    const SUCCESS_BODY: &str = "GEMINI_PRODUCTION_SUCCESS_BODY";

    #[test]
    fn gemini_production_nativepath_core_first_failure_isolated_and_replay_catches_up_idempotently()
    {
        let temp = tempdir().unwrap();
        let root = temp.path().join(".gemini");
        let transcript = root.join("tmp/project/chats/route-proof.jsonl");
        let records = [
            json!({
                "sessionId": "gemini-route-proof",
                "startTime": "2026-07-25T12:00:00.000Z",
                "lastUpdated": "2026-07-25T12:00:00.000Z",
                "kind": "main",
                "directories": ["/workspace/gemini-route-proof"]
            }),
            json!({
                "id": "message-1",
                "timestamp": "2026-07-25T12:00:01.000Z",
                "type": "user",
                "content": "core-visible message"
            }),
            json!({
                "id": "result-1",
                "timestamp": "2026-07-25T12:00:02.000Z",
                "type": "gemini",
                "toolCalls": [{
                    "id": "call-1",
                    "name": "run_shell_command",
                    "result": {
                        "content": SUCCESS_BODY,
                        "success": true,
                        "exitCode": 0,
                        "durationMs": 17
                    }
                }]
            }),
        ];
        let expected_byte_start = jsonl(&records[..2]).len() as u64;
        let transcript_bytes = jsonl(&records);
        let expected_byte_end = transcript_bytes.len() as u64;
        fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        fs::write(&transcript, transcript_bytes).unwrap();

        let store_path = temp.path().join("history.sqlite");
        let mut store = Store::open(&store_path).unwrap();
        let failing = Arc::new(RecordingSink::new(store_path.clone(), true));
        let first = import(
            &root,
            &mut store,
            crate::ImportProfile::CoreAndPro(failing.clone()),
        );

        assert_eq!(first.work_result(), ProviderImportWorkResult::Changed);
        assert!(failing.saw_core_before_page.load(Ordering::SeqCst));
        assert_eq!(failing.behind.load(Ordering::SeqCst), 1);
        assert_eq!(failing.pages.load(Ordering::SeqCst), 0);
        let session = store
            .list_sessions()
            .unwrap()
            .into_iter()
            .find(|session| session.provider == CaptureProvider::Gemini)
            .unwrap();
        let core_events = store.events_for_session(session.id).unwrap();
        assert!(core_events
            .iter()
            .any(|event| event.event_type == EventType::Message));
        assert!(!serde_json::to_string(&core_events)
            .unwrap()
            .contains(SUCCESS_BODY));

        let replay = Arc::new(RecordingSink::new(store_path, false));
        let catch_up = import(
            &root,
            &mut store,
            crate::ImportProfile::ProReplayOnly(replay.clone()),
        );
        assert_eq!(catch_up.work_result(), ProviderImportWorkResult::NoOp);
        assert!(replay.saw_core_before_page.load(Ordering::SeqCst));
        assert_eq!(replay.behind.load(Ordering::SeqCst), 0);
        assert!(replay.pages.load(Ordering::SeqCst) > 0);

        let observations = replay.observations.lock().unwrap();
        assert_eq!(observations.len(), 1);
        let observation = &observations[0];
        assert_eq!(observation.content, SUCCESS_BODY.as_bytes());
        assert_eq!(observation.call_id.as_deref(), Some("call-1"));
        assert_eq!(
            observation.coordinate.native_record_id.as_deref(),
            Some("result-1")
        );
        assert_eq!(observation.coordinate.source_record_ordinal, Some(2));
        assert_eq!(
            observation.coordinate.source_record_subrecord_index,
            Some(0)
        );
        assert_eq!(observation.coordinate.byte_start, Some(expected_byte_start));
        assert_eq!(
            observation.coordinate.byte_end_exclusive,
            Some(expected_byte_end)
        );
        assert_eq!(observation.outcome.outcome, OutputOutcome::Success);
        assert_eq!(observation.outcome.exit_code, Some(0));
        assert_eq!(observation.outcome.duration_ms, Some(17));
        assert_eq!(observation.locator.version, 1);
        assert_eq!(observation.locator.kind, "gemini/nativepath/jsonl-result");
        let locator: Value = serde_json::from_slice(&observation.locator.payload).unwrap();
        let canonical_transcript = fs::canonicalize(&transcript).unwrap();
        assert_eq!(
            locator.get("path").and_then(Value::as_str),
            Some(canonical_transcript.to_str().unwrap())
        );
        assert_eq!(
            locator.get("byte_start").and_then(Value::as_u64),
            Some(expected_byte_start)
        );
        assert_eq!(
            locator.get("byte_end_exclusive").and_then(Value::as_u64),
            Some(expected_byte_end)
        );
        drop(observations);

        let sources = replay.sources.lock().unwrap();
        assert_eq!(sources.len(), replay.pages.load(Ordering::SeqCst));
        assert!(sources.iter().all(|source| {
            source.provider == CaptureProvider::Gemini.as_str()
                && source.namespace_id == root.display().to_string()
                && source.source_id == provider_path_identity(&canonical_transcript).unwrap()
        }));
        drop(sources);

        let pages_after_catch_up = replay.pages.load(Ordering::SeqCst);
        let idempotent = import(
            &root,
            &mut store,
            crate::ImportProfile::ProReplayOnly(replay.clone()),
        );
        assert_eq!(idempotent.work_result(), ProviderImportWorkResult::NoOp);
        assert_eq!(replay.pages.load(Ordering::SeqCst), pages_after_catch_up);
        assert_eq!(replay.observations.lock().unwrap().len(), 1);
    }

    struct RecordingSink {
        store_path: PathBuf,
        fail_first: AtomicBool,
        progress: Mutex<Option<ProOutputProgress>>,
        pages: AtomicUsize,
        behind: AtomicUsize,
        saw_core_before_page: AtomicBool,
        sources: Mutex<Vec<OutputSourceIdentity>>,
        observations: Mutex<Vec<ProOutputObservation>>,
    }

    impl RecordingSink {
        fn new(store_path: PathBuf, fail_first: bool) -> Self {
            Self {
                store_path,
                fail_first: AtomicBool::new(fail_first),
                progress: Mutex::new(None),
                pages: AtomicUsize::new(0),
                behind: AtomicUsize::new(0),
                saw_core_before_page: AtomicBool::new(false),
                sources: Mutex::new(Vec::new()),
                observations: Mutex::new(Vec::new()),
            }
        }
    }

    impl ProOutputSink for RecordingSink {
        fn inventory_generation(&self) -> u64 {
            1
        }

        fn materializer_revision(&self) -> &str {
            "gemini-nativepath-test-materializer-v1"
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
            if core
                .list_sessions()
                .map_err(|error| ProOutputSinkError::new("test_sessions", error.to_string()))?
                .iter()
                .any(|session| session.provider == CaptureProvider::Gemini)
            {
                self.saw_core_before_page.store(true, Ordering::SeqCst);
            }
            if self.fail_first.swap(false, Ordering::SeqCst) {
                return Err(ProOutputSinkError::new(
                    "intentional_test_failure",
                    "intentional Gemini output failure",
                ));
            }
            let committed_cursor = page.next_safe_cursor.clone();
            let accepted_outputs = u32::try_from(page.observations.len()).unwrap();
            *self.progress.lock().unwrap() = Some(ProOutputProgress {
                source_epoch: page.source_epoch,
                observed_revision: page.observed_revision.clone(),
                cursor: Some(committed_cursor.clone()),
                parser_revision: page.parser_revision.clone(),
                materializer_revision: page.materializer_revision.clone(),
                terminal: page.terminal,
            });
            self.sources.lock().unwrap().push(page.source);
            self.observations.lock().unwrap().extend(page.observations);
            self.pages.fetch_add(1, Ordering::SeqCst);
            Ok(ProOutputPageResult {
                source_epoch: page.source_epoch,
                committed_cursor,
                accepted_outputs,
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
        import_profile: crate::ImportProfile,
    ) -> ProviderImportSummary {
        import_gemini_cli_history(
            root,
            store,
            GeminiCliImportOptions {
                machine_id: MACHINE.to_owned(),
                source_path: Some(root.to_path_buf()),
                imported_at: "2026-07-25T12:00:00Z".parse().unwrap(),
                import_profile,
                ..GeminiCliImportOptions::default()
            },
        )
        .unwrap()
    }

    fn jsonl(values: &[Value]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for value in values {
            serde_json::to_writer(&mut bytes, value).unwrap();
            bytes.push(b'\n');
        }
        bytes
    }
}
