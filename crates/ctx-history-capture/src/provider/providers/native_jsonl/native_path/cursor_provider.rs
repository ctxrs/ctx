use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    Confidence, Event, Fidelity, FileChangeKind, FileTouched, Session, SessionStatus, SyncCursor,
};
use ctx_history_store::{
    EventSearchBulkGuard, NativePathCursorSetClassification, NativePathCursorTransition,
    NativePathGroupAccounting, ProviderEventHashAuthority, ProviderSourceLocatorObservation,
    ProviderSourceRouteRetirement, ProviderSourceRouteRetirementDisposition,
    ProviderSourceRouteRetirementReason, Store,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::provider::{
    importer::{
        provider_event_import_identity_with_exact_legacy_source, provider_file_touch_import_id,
        provider_import_session_uuid, provider_scoped_source_identity_key,
        provider_scoped_source_uuid, provider_source_cursor_stream_for_path,
        provider_sync_metadata, timestamps,
    },
    providers::cursor::{
        discover_cursor_transcripts, freeze_cursor_source, resolve_cursor_missing_sources,
        scan_cursor_source_into, CursorCheckpoint, CursorCompletedExactInventory,
        CursorFrozenSource, CursorKnownSource, CursorMissingSourceDisposition, CursorNativeEvent,
        CursorNativeSession, CursorPriorObservation, CursorPublicationPage, CursorPublicationSink,
        CursorReadOutcome, CursorRecordRejection, CursorSourceObservation, CursorTranscriptPath,
    },
};
use crate::{
    stable_capture_uuid, CaptureError, CaptureWorkLimit, ProOutputSink, ProOutputSinkError,
    ProviderImportFailure, ProviderImportSummary, ProviderImportWorkResult, Result,
    CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
};

use super::NativePathJsonlTreeImport;

const CURSOR_NATIVE_CURSOR_VERSION: u32 = 1;
const CURSOR_PUBLICATION_DOMAIN: &[u8] = b"ctx-cursor-nativepath-publication-v1\0";
const CURSOR_MISSING_TRANSCRIPTS_REASON: &str =
    "no Cursor agent transcript JSONL files found under projects/*/agent-transcripts";
const CURSOR_GROUP_MAX_PAGES: usize = 32;
const CURSOR_GROUP_MAX_SOURCES: usize = 64;
const CURSOR_GROUP_MAX_BYTES: usize = 6 * 1024 * 1024;
const CURSOR_GROUP_MAX_ESTIMATED_MUTATIONS: usize = 3_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CursorNativeCursorWire {
    version: u32,
    kind: String,
    canonical_source_identity: String,
    observation: CursorSourceObservation,
    checkpoint: CursorCheckpoint,
    retained_event_count: u64,
    rejected_records: u64,
    rejections: Vec<CursorRecordRejection>,
}

struct CursorPendingPage {
    transcript: CursorTranscriptPath,
    observation: CursorSourceObservation,
    page: CursorPublicationPage,
    retained_event_count: u64,
}

struct CursorPublicationContext<'a> {
    machine_id: &'a str,
    source_root: &'a Path,
    imported_at: DateTime<Utc>,
    history_record_id: Option<Uuid>,
}

struct ResolvedCursorSource {
    source_id: Uuid,
    session: Option<Session>,
}

struct CursorKnownRoute {
    known: CursorKnownSource,
    current_cursor: SyncCursor,
    wire: CursorNativeCursorWire,
}

pub(crate) fn import_cursor_nativepath_tree(
    store: &mut Store,
    request: NativePathJsonlTreeImport<'_>,
) -> Result<ProviderImportSummary> {
    let configured_source_root = request
        .source_root
        .clone()
        .or(request.source_path.clone())
        .unwrap_or_else(|| request.path.to_path_buf());
    let discovery_input = if request.path.is_dir() && request.path.join("projects").is_dir() {
        request.path.join("projects")
    } else {
        request.path.to_path_buf()
    };
    let inventory = discover_cursor_transcripts(&discovery_input);
    if !inventory.completed {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: request.path.to_path_buf(),
            reason: "Cursor transcript inventory could not be completed",
        });
    }
    let output_sink = request.import_profile.sink().cloned();
    if request.import_profile.is_replay_only() {
        let sink = output_sink.as_deref().ok_or(CaptureError::SystemInvariant(
            "Cursor replay-only profile has no output sink",
        ))?;
        for transcript in &inventory.transcripts {
            replay_cursor_source_outputs_or_mark_behind(
                store,
                &request.machine_id,
                &configured_source_root,
                transcript,
                sink,
            );
        }
        return Ok(ProviderImportSummary::default());
    }
    let committed_store = Store::open_read_only(store.path())?;
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let context = CursorPublicationContext {
        machine_id: &request.machine_id,
        source_root: &configured_source_root,
        imported_at: request.imported_at,
        history_record_id: request.history_record_id,
    };
    let mut accumulator = CursorGroupAccumulator::new(
        store,
        &committed_store,
        &bulk_guard,
        context,
        request.capture_work_limit,
    );
    let mut observations = Vec::with_capacity(inventory.transcripts.len());

    let operation = (|| {
        for transcript in &inventory.transcripts {
            if accumulator.stopped {
                break;
            }
            let frozen = freeze_cursor_source(transcript)?;
            observations.push(frozen.observation().clone());
            let stream = provider_source_cursor_stream_for_path(
                CaptureProvider::Cursor,
                CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
                &frozen.observation().locator_identity,
            );
            let stored = accumulator
                .store
                .get_sync_cursor(None, &request.machine_id, &stream)?;
            let prior_wire = stored
                .as_ref()
                .map(|cursor| decode_cursor_native_cursor(&cursor.cursor))
                .transpose()?
                .flatten();
            let prior = prior_wire.as_ref().map(|wire| CursorPriorObservation {
                canonical_source_key: wire.canonical_source_identity.clone(),
                observation: wire.observation.clone(),
                checkpoint: wire.checkpoint.clone(),
            });
            let prior_retained = prior_wire
                .as_ref()
                .map_or(0, |wire| wire.retained_event_count);
            let mut sink = CursorAccumulatorSink {
                accumulator: &mut accumulator,
                frozen: frozen.clone(),
                prior_retained,
                retained_event_count: prior_retained,
            };
            let outcome = scan_cursor_source_into(&frozen, prior.as_ref(), &mut sink)?;
            drop(sink);
            match outcome {
                CursorReadOutcome::Unchanged(unchanged) => {
                    let events = prior_wire
                        .as_ref()
                        .map_or(0, |wire| wire.retained_event_count);
                    accumulator.record_unchanged(
                        unchanged.observation.native_session_id,
                        events,
                        prior_wire.as_ref(),
                    );
                }
                CursorReadOutcome::Generation(generation) => {
                    for rejection in &generation.rejections.samples {
                        accumulator.summary.record_failure(ProviderImportFailure {
                            line: usize::try_from(rejection.physical_line)
                                .unwrap_or(usize::MAX)
                                .saturating_add(1),
                            error: cursor_rejection_message(
                                rejection.kind,
                                rejection.observed_bytes,
                            ),
                        });
                    }
                }
            }
        }
        accumulator.finish()
    })();
    let stopped = accumulator.stopped;
    drop(accumulator);
    let operation = operation.and_then(|mut summary| {
        if stopped {
            return Ok(summary);
        }
        let exact_inventory =
            CursorCompletedExactInventory::from_discovery(&inventory, &observations);
        let retirement = retire_missing_cursor_routes(
            store,
            &bulk_guard,
            &request.machine_id,
            request.imported_at,
            exact_inventory.as_ref(),
            &inventory.input,
        )?;
        if inventory.transcripts.is_empty() && retirement.known_routes == 0 {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: request.path.to_path_buf(),
                reason: CURSOR_MISSING_TRANSCRIPTS_REASON,
            });
        }
        summary.merge_from(retirement.summary);
        Ok(summary)
    });
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    let result = match (operation, finish) {
        (Ok(mut summary), Ok(())) => {
            if stopped {
                summary.work_remaining = true;
            }
            Ok(summary)
        }
        (_, Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
    };
    if !stopped {
        if let (Ok(_), Some(sink)) = (&result, output_sink.as_deref()) {
            for transcript in &inventory.transcripts {
                replay_cursor_source_outputs_or_mark_behind(
                    store,
                    &request.machine_id,
                    &configured_source_root,
                    transcript,
                    sink,
                );
            }
        }
    }
    result
}

fn replay_cursor_source_outputs_or_mark_behind(
    store: &Store,
    machine_id: &str,
    source_root: &Path,
    transcript: &CursorTranscriptPath,
    sink: &dyn ProOutputSink,
) {
    if let Err(error) =
        replay_cursor_source_outputs(store, machine_id, source_root, transcript, sink)
    {
        sink.mark_behind(ProOutputSinkError::new(
            "cursor_nativepath_output_replay",
            error.to_string(),
        ));
    }
}

fn replay_cursor_source_outputs(
    store: &Store,
    machine_id: &str,
    source_root: &Path,
    transcript: &CursorTranscriptPath,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    let frozen = freeze_cursor_source(transcript)?;
    let observation = frozen.observation();
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Cursor,
        CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
        &observation.locator_identity,
    );
    let wire = store
        .get_sync_cursor(None, machine_id, &stream)?
        .as_ref()
        .map(|cursor| decode_cursor_native_cursor(&cursor.cursor))
        .transpose()?
        .flatten()
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Cursor output replay requires committed NativePath Core".to_owned(),
            )
        })?;
    if !cursor_core_observation_covers(&wire.observation, observation) {
        return Err(CaptureError::InvalidPayload(
            "Cursor source no longer matches committed Core authority".to_owned(),
        ));
    }

    let observed_revision = cursor_source_revision(&wire.observation);
    frozen.replay_outputs(
        source_root,
        &wire.canonical_source_identity,
        &wire.checkpoint,
        &observed_revision,
        sink,
    )
}

fn cursor_core_observation_covers(
    core: &CursorSourceObservation,
    current: &CursorSourceObservation,
) -> bool {
    core.path == current.path
        && core.locator_identity == current.locator_identity
        && core.native_session_id == current.native_session_id
        && core.length == current.length
        && core.content_sha256 == current.content_sha256
}

struct CursorRetirementResult {
    known_routes: usize,
    summary: ProviderImportSummary,
}

fn retire_missing_cursor_routes(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    machine_id: &str,
    retired_at: DateTime<Utc>,
    inventory: Option<&CursorCompletedExactInventory>,
    projects_root: &Path,
) -> Result<CursorRetirementResult> {
    let routes = known_cursor_routes(store, machine_id, projects_root)?;
    let dispositions = resolve_cursor_missing_sources(
        &routes
            .iter()
            .map(|route| route.known.clone())
            .collect::<Vec<_>>(),
        inventory,
    );
    let by_locator = routes
        .into_iter()
        .map(|route| (route.known.locator_identity.clone(), route))
        .collect::<BTreeMap<_, _>>();
    let mut summary = ProviderImportSummary::default();
    for disposition in dispositions {
        let CursorMissingSourceDisposition::RouteUnavailableCandidate {
            locator_identity, ..
        } = disposition
        else {
            continue;
        };
        let route = by_locator
            .get(&locator_identity)
            .ok_or(CaptureError::SystemInvariant(
                "Cursor missing-source resolution lost its persisted route",
            ))?;
        if retire_cursor_route(store, bulk_guard, machine_id, retired_at, route)? {
            summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
            summary.set_work_result(ProviderImportWorkResult::Changed);
        }
    }
    Ok(CursorRetirementResult {
        known_routes: by_locator.len(),
        summary,
    })
}

fn known_cursor_routes(
    store: &Store,
    machine_id: &str,
    projects_root: &Path,
) -> Result<Vec<CursorKnownRoute>> {
    let mut routes = BTreeMap::<String, CursorKnownRoute>::new();
    for source in store.list_capture_sources()? {
        if source.descriptor.provider != CaptureProvider::Cursor
            || source.descriptor.machine_id != machine_id
            || source.descriptor.source_format.as_deref()
                != Some(CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT)
        {
            continue;
        }
        let Some(raw_source_path) = source.descriptor.raw_source_path.as_deref() else {
            continue;
        };
        let path = Path::new(raw_source_path);
        if !path.starts_with(projects_root) {
            continue;
        }
        let locator_identity = crate::provider::importer::provider_path_identity(path)?;
        let stream = provider_source_cursor_stream_for_path(
            CaptureProvider::Cursor,
            CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
            &locator_identity,
        );
        let Some(current_cursor) = store.get_sync_cursor(None, machine_id, &stream)? else {
            continue;
        };
        let Some(wire) = decode_cursor_native_cursor(&current_cursor.cursor)? else {
            continue;
        };
        if wire.observation.locator_identity != locator_identity
            || wire.observation.path != path
            || source.descriptor.source_identity.as_deref()
                != Some(wire.canonical_source_identity.as_str())
        {
            continue;
        }
        let known = CursorKnownSource {
            canonical_source_key: wire.canonical_source_identity.clone(),
            locator_identity: locator_identity.clone(),
            native_session_id: wire.observation.native_session_id.clone(),
        };
        let route = CursorKnownRoute {
            known,
            current_cursor,
            wire,
        };
        if routes.insert(locator_identity, route).is_some() {
            return Err(CaptureError::SystemInvariant(
                "Cursor persisted more than one canonical route for one locator",
            ));
        }
    }
    Ok(routes.into_values().collect())
}

fn retire_cursor_route(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    machine_id: &str,
    retired_at: DateTime<Utc>,
    route: &CursorKnownRoute,
) -> Result<bool> {
    let provider_cursor = encode_cursor_native_cursor(
        &route.wire.canonical_source_identity,
        &route.wire.observation,
        &route.wire.checkpoint,
        route.wire.retained_event_count,
        route.wire.rejected_records,
        &route.wire.rejections,
    )?;
    let stream = route.current_cursor.stream.clone();
    let transition = NativePathCursorTransition::new(
        Some(route.current_cursor.cursor.clone()),
        provider_sync_cursor(machine_id, stream.clone(), provider_cursor, retired_at),
    );
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::Cursor,
        source_format: CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT.to_owned(),
        machine_id: machine_id.to_owned(),
        locator_identity: route.known.locator_identity.clone(),
        cursor_stream: stream,
        expected_canonical_source_identity: route.wire.canonical_source_identity.clone(),
        expected_source_revision: cursor_source_revision(&route.wire.observation),
        retired_at_ms: retired_at.timestamp_millis(),
        reason: ProviderSourceRouteRetirementReason::SourceMissing,
    };
    let publication_id = cursor_retirement_publication_id(&retirement);
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

struct CursorAccumulatorSink<'a, 'store> {
    accumulator: &'a mut CursorGroupAccumulator<'store>,
    frozen: CursorFrozenSource,
    prior_retained: u64,
    retained_event_count: u64,
}

impl CursorPublicationSink for CursorAccumulatorSink<'_, '_> {
    fn begin_cursor_publication(&mut self) -> Result<()> {
        Ok(())
    }

    fn stage_cursor_page(&mut self, page: CursorPublicationPage) -> Result<()> {
        if self.accumulator.stopped {
            return Ok(());
        }
        if page.expected_checkpoint.next_byte_offset == 0
            && page.expected_checkpoint.next_semantic_ordinal == 0
        {
            self.retained_event_count = 0;
        } else if self.retained_event_count == 0 {
            self.retained_event_count = self.prior_retained;
        }
        self.retained_event_count = self
            .retained_event_count
            .saturating_add(page.events.len() as u64);
        self.accumulator.push(CursorPendingPage {
            transcript: self.frozen.transcript().clone(),
            observation: self.frozen.observation().clone(),
            page,
            retained_event_count: self.retained_event_count,
        })
    }

    fn abort_cursor_publication(&mut self) {
        self.accumulator
            .discard_unpublished_source(&self.frozen.observation().path);
    }

    fn commit_cursor_publication(&mut self) -> Result<()> {
        Ok(())
    }
}

struct CursorGroupAccumulator<'a> {
    store: &'a mut Store,
    committed_store: &'a Store,
    bulk_guard: &'a EventSearchBulkGuard,
    context: CursorPublicationContext<'a>,
    work_limit: CaptureWorkLimit,
    pages: Vec<CursorPendingPage>,
    sources: BTreeSet<PathBuf>,
    bytes: usize,
    estimated_mutations: usize,
    summary: ProviderImportSummary,
    stopped: bool,
}

impl<'a> CursorGroupAccumulator<'a> {
    fn new(
        store: &'a mut Store,
        committed_store: &'a Store,
        bulk_guard: &'a EventSearchBulkGuard,
        context: CursorPublicationContext<'a>,
        work_limit: CaptureWorkLimit,
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
            stopped: false,
        }
    }

    fn push(&mut self, pending: CursorPendingPage) -> Result<()> {
        let next_sources =
            self.sources.len() + usize::from(!self.sources.contains(&pending.observation.path));
        let next_bytes = self.bytes.saturating_add(pending.page.serialized_bytes);
        let page_mutations = pending
            .page
            .events
            .iter()
            .map(|event| 1_usize.saturating_add(cursor_event_touch_count(event)))
            .sum::<usize>()
            .saturating_add(4);
        let next_mutations = self.estimated_mutations.saturating_add(page_mutations);
        if !self.pages.is_empty()
            && (self.pages.len() >= CURSOR_GROUP_MAX_PAGES
                || next_sources > CURSOR_GROUP_MAX_SOURCES
                || next_bytes > CURSOR_GROUP_MAX_BYTES
                || next_mutations > CURSOR_GROUP_MAX_ESTIMATED_MUTATIONS)
        {
            self.flush()?;
            if self.stopped {
                return Ok(());
            }
        }
        self.bytes = self.bytes.saturating_add(pending.page.serialized_bytes);
        self.estimated_mutations = self.estimated_mutations.saturating_add(page_mutations);
        self.sources.insert(pending.observation.path.clone());
        self.pages.push(pending);
        Ok(())
    }

    fn discard_unpublished_source(&mut self, path: &Path) {
        self.pages
            .retain(|pending| pending.observation.path != path);
        self.recalculate_bounds();
    }

    fn recalculate_bounds(&mut self) {
        self.sources = self
            .pages
            .iter()
            .map(|pending| pending.observation.path.clone())
            .collect();
        self.bytes = self.pages.iter().fold(0, |total, pending| {
            total.saturating_add(pending.page.serialized_bytes)
        });
        self.estimated_mutations = self.pages.iter().fold(0, |total, pending| {
            total.saturating_add(
                pending
                    .page
                    .events
                    .iter()
                    .map(|event| 1_usize.saturating_add(cursor_event_touch_count(event)))
                    .sum::<usize>()
                    .saturating_add(4),
            )
        });
    }

    fn record_unchanged(
        &mut self,
        _provider_session_id: String,
        events: u64,
        wire: Option<&CursorNativeCursorWire>,
    ) {
        self.summary.skipped_sessions = self.summary.skipped_sessions.saturating_add(1);
        self.summary.skipped_events = self
            .summary
            .skipped_events
            .saturating_add(usize::try_from(events).unwrap_or(usize::MAX));
        self.summary.skipped = self
            .summary
            .skipped
            .saturating_add(1)
            .saturating_add(usize::try_from(events).unwrap_or(usize::MAX));
        if let Some(wire) = wire {
            for rejection in &wire.rejections {
                self.summary.record_failure(ProviderImportFailure {
                    line: usize::try_from(rejection.physical_line)
                        .unwrap_or(usize::MAX)
                        .saturating_add(1),
                    error: cursor_rejection_message(rejection.kind, rejection.observed_bytes),
                });
            }
        }
    }

    fn flush(&mut self) -> Result<()> {
        if self.pages.is_empty() {
            return Ok(());
        }
        let pages = std::mem::take(&mut self.pages);
        let summary = publish_cursor_group(
            self.store,
            self.committed_store,
            self.bulk_guard,
            &self.context,
            &pages,
        )?;
        self.summary.merge_from(summary);
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

fn publish_cursor_group(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &CursorPublicationContext<'_>,
    pages: &[CursorPendingPage],
) -> Result<ProviderImportSummary> {
    let source_paths = pages
        .iter()
        .map(|pending| pending.observation.path.clone())
        .collect::<BTreeSet<_>>();
    for path in &source_paths {
        revalidate_cursor_source(pages, path)?;
    }

    let mut transitions = Vec::with_capacity(source_paths.len());
    for path in &source_paths {
        let pending = pages
            .iter()
            .rev()
            .find(|pending| &pending.observation.path == path)
            .expect("Cursor pending source exists");
        let stream = provider_source_cursor_stream_for_path(
            CaptureProvider::Cursor,
            CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
            &pending.observation.locator_identity,
        );
        let stored = store.get_sync_cursor(None, context.machine_id, &stream)?;
        let provider_cursor = encode_cursor_native_cursor(
            &pending.observation.proposed_source_identity,
            &pending.observation,
            &pending.page.next_checkpoint,
            pending.retained_event_count,
            pending.page.rejected_records,
            &pending.page.rejections,
        )?;
        transitions.push(NativePathCursorTransition::new(
            stored.as_ref().map(|cursor| cursor.cursor.clone()),
            provider_sync_cursor(
                context.machine_id,
                stream,
                provider_cursor,
                context.imported_at,
            ),
        ));
    }
    let publication_id = cursor_publication_id(pages, &transitions);
    let retained_bytes = pages.iter().fold(0_usize, |total, pending| {
        total.saturating_add(pending.page.serialized_bytes)
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
            .find(|pending| &pending.observation.path == path)
            .expect("Cursor pending source exists");
        let raw_source_path = path.display().to_string();
        let source_root = context.source_root.display().to_string();
        let stream = provider_source_cursor_stream_for_path(
            CaptureProvider::Cursor,
            CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
            &pending.observation.locator_identity,
        );
        let source_revision = cursor_source_revision(&pending.observation);
        let resolution =
            group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
                provider: CaptureProvider::Cursor,
                source_format: CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT.to_owned(),
                machine_id: context.machine_id.to_owned(),
                locator_identity: pending.observation.locator_identity.clone(),
                cursor_stream: stream,
                proposed_source_identity: pending.observation.proposed_source_identity.clone(),
                raw_source_path: Some(raw_source_path.clone()),
                source_revision: source_revision.clone(),
                observed_at_ms: context.imported_at.timestamp_millis(),
            })?;
        let native_session_id = &pending.observation.native_session_id;
        let source_id = committed_store
            .capture_source_by_canonical_identity_session(
                CaptureProvider::Cursor,
                CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
                context.machine_id,
                &resolution.canonical_source_identity,
                native_session_id,
            )?
            .map(|source| source.id)
            .unwrap_or_else(|| {
                provider_scoped_source_uuid(
                    CaptureProvider::Cursor,
                    native_session_id,
                    CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
                    Some(&raw_source_path),
                )
            });
        let session_fact = cursor_session_fact(pending);
        group.upsert_capture_source(&cursor_capture_source(
            context,
            session_fact.as_ref(),
            source_id,
            &raw_source_path,
            &source_root,
            &resolution.canonical_source_identity,
            &source_revision,
        ))?;
        group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;
        let session = session_fact
            .as_ref()
            .map(|fact| {
                cursor_session(
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
            group.upsert_session(session)?;
            if existed {
                summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
                summary.skipped = summary.skipped.saturating_add(1);
            } else {
                summary.imported_sessions = summary.imported_sessions.saturating_add(1);
                summary.imported = summary.imported.saturating_add(1);
            }
        }
        resolved.insert(path.clone(), ResolvedCursorSource { source_id, session });
    }

    for pending in pages {
        let source =
            resolved
                .get(&pending.observation.path)
                .ok_or(CaptureError::SystemInvariant(
                    "Cursor publication lost its resolved source",
                ))?;
        for event in &pending.page.events {
            let session = source
                .session
                .as_ref()
                .ok_or(CaptureError::SystemInvariant(
                    "Cursor retained event has no canonical session",
                ))?;
            publish_cursor_event(
                &mut group,
                committed_store,
                context,
                source.source_id,
                session,
                event,
                &mut summary,
            )?;
        }
    }

    for path in &source_paths {
        revalidate_cursor_source(pages, path)?;
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

fn cursor_session_fact(pending: &CursorPendingPage) -> Option<CursorNativeSession> {
    let checkpoint = &pending.page.next_checkpoint.session;
    let has_session = !pending.page.events.is_empty()
        || checkpoint.started_at.is_some()
        || checkpoint.title.is_some();
    has_session.then(|| CursorNativeSession {
        native_session_id: pending.observation.native_session_id.clone(),
        project: pending.transcript.project().to_path_buf(),
        started_at: checkpoint.started_at,
        ended_at: checkpoint.ended_at,
        title: checkpoint.title.clone(),
    })
}

fn cursor_capture_source(
    context: &CursorPublicationContext<'_>,
    session: Option<&CursorNativeSession>,
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
            provider: CaptureProvider::Cursor,
            machine_id: context.machine_id.to_owned(),
            process_id: None,
            cwd: None,
            raw_source_path: Some(raw_source_path.to_owned()),
            source_format: Some(CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT.to_owned()),
            source_root: Some(source_root.to_owned()),
            source_identity: Some(source_identity.to_owned()),
            external_session_id: session.map(|session| session.native_session_id.clone()),
        },
        started_at: session
            .and_then(|session| session.started_at)
            .unwrap_or(context.imported_at),
        ended_at: session.and_then(|session| session.ended_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": session.map(|session| &session.native_session_id),
                "source_format": CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "source_identity": source_identity,
                "source_revision": source_revision,
                "source_identity_key": session.map(|session| {
                    provider_scoped_source_identity_key(
                        CaptureProvider::Cursor,
                        &session.native_session_id,
                        CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
                        Some(raw_source_path),
                    )
                }),
            }),
        ),
    }
}

fn cursor_session(
    committed_store: &Store,
    context: &CursorPublicationContext<'_>,
    fact: &CursorNativeSession,
    source_id: Uuid,
    source_identity: &str,
) -> Result<Session> {
    let id = provider_import_session_uuid(
        committed_store,
        CaptureProvider::Cursor,
        &fact.native_session_id,
        source_id,
        Some(source_identity),
    )?;
    Ok(Session {
        id,
        history_record_id: context.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Cursor,
        external_session_id: Some(fact.native_session_id.clone()),
        external_agent_id: None,
        agent_type: AgentType::Primary,
        role_hint: Some("primary".to_owned()),
        is_primary: true,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: fact.started_at.unwrap_or(context.imported_at),
        ended_at: fact.ended_at,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": fact.native_session_id,
                "project": fact.project,
                "title": fact.title,
                "source_format": CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
            }),
        ),
    })
}

#[allow(clippy::too_many_arguments)]
fn publish_cursor_event(
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    committed_store: &Store,
    context: &CursorPublicationContext<'_>,
    source_id: Uuid,
    session: &Session,
    event: &CursorNativeEvent,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let provider_event_index = cursor_event_index(event)?;
    let identity = provider_event_import_identity_with_exact_legacy_source(
        committed_store,
        CaptureProvider::Cursor,
        session.external_session_id.as_deref().unwrap_or_default(),
        source_id,
        provider_event_index,
        provider_event_index,
        &event.provider_event_hash,
        None,
        Some(event.native_order.semantic_ordinal),
        session.id
            == crate::provider::importer::provider_session_uuid(
                CaptureProvider::Cursor,
                session.external_session_id.as_deref().unwrap_or_default(),
            ),
    )?;
    let dedupe_key = Store::provider_event_dedupe_key_with_payload_hash(
        &identity.dedupe_key,
        &event.provider_event_hash,
    )
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
            "provider": CaptureProvider::Cursor.as_str(),
            "provider_session_id": session.external_session_id,
            "provider_event_index": provider_event_index,
            "provider_event_hash": event.provider_event_hash,
            "native_identity": event.identity.provider_identity(),
            "body": event.body,
            "artifacts": [],
        }),
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": session.external_session_id,
                "provider_event_index": provider_event_index,
                "provider_event_hash": event.provider_event_hash,
                "provider_event_hash_authority": "provider_supplied",
                "source_format": CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "cursor": event.identity.provider_identity(),
                "fixture_line": event.native_order.semantic_ordinal.saturating_add(1),
                "source_record_ordinal": event.native_order.semantic_ordinal,
                "source_record_subrecord_index": event.native_order.part_ordinal,
                "native_identity": event.identity.provider_identity(),
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

    if let crate::provider::providers::cursor::CursorEventBody::ToolCall { input_paths, .. } =
        &event.body
    {
        for (touch_ordinal, path) in input_paths.iter().enumerate() {
            let packed_touch = event
                .native_order
                .semantic_ordinal
                .checked_mul(u64::from(u16::MAX) + 1)
                .and_then(|base| base.checked_add(touch_ordinal as u64))
                .ok_or(CaptureError::SystemInvariant(
                    "Cursor file-touch identity overflowed",
                ))?;
            let id = provider_file_touch_import_id(
                committed_store,
                CaptureProvider::Cursor,
                session.external_session_id.as_deref().unwrap_or_default(),
                source_id,
                Some(provider_event_index),
                packed_touch,
                session.id
                    == crate::provider::importer::provider_session_uuid(
                        CaptureProvider::Cursor,
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
                        "provider": CaptureProvider::Cursor.as_str(),
                        "provider_session_id": session.external_session_id,
                        "provider_event_index": provider_event_index,
                        "source_format": CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
                    }),
                ),
            })?;
        }
    }
    Ok(())
}

fn cursor_event_index(event: &CursorNativeEvent) -> Result<u64> {
    if event.native_order.part_ordinal == 0 {
        return Ok(event.native_order.semantic_ordinal);
    }
    event
        .native_order
        .semantic_ordinal
        .checked_mul(u64::from(u16::MAX) + 1)
        .and_then(|index| index.checked_add(u64::from(event.native_order.part_ordinal)))
        .map(|index| index | (1_u64 << 63))
        .ok_or(CaptureError::SystemInvariant(
            "Cursor provider event identity index overflowed",
        ))
}

fn cursor_event_touch_count(event: &CursorNativeEvent) -> usize {
    match &event.body {
        crate::provider::providers::cursor::CursorEventBody::ToolCall { input_paths, .. } => {
            input_paths.len()
        }
        _ => 0,
    }
}

fn revalidate_cursor_source(pages: &[CursorPendingPage], path: &Path) -> Result<()> {
    let pending = pages
        .iter()
        .find(|pending| pending.observation.path == path)
        .expect("Cursor pending source exists");
    let frozen = freeze_cursor_source(&pending.transcript)?;
    if frozen.observation() != &pending.observation {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    frozen.revalidate()
}

fn cursor_source_revision(observation: &CursorSourceObservation) -> String {
    format!(
        "cursor-nativepath-strong-v1:length={};sha256={};modified={}:{}.{:09};readonly={};device={};inode={}",
        observation.length,
        hex_digest(observation.content_sha256),
        if observation.modified.before_epoch { '-' } else { '+' },
        observation.modified.seconds,
        observation.modified.nanos,
        observation.readonly,
        observation
            .file_identity
            .map_or_else(|| "none".to_owned(), |identity| identity.device.to_string()),
        observation
            .file_identity
            .map_or_else(|| "none".to_owned(), |identity| identity.inode.to_string()),
    )
}

fn encode_cursor_native_cursor(
    canonical_source_identity: &str,
    observation: &CursorSourceObservation,
    checkpoint: &CursorCheckpoint,
    retained_event_count: u64,
    rejected_records: u64,
    rejections: &[CursorRecordRejection],
) -> Result<String> {
    Ok(serde_json::to_string(&CursorNativeCursorWire {
        version: CURSOR_NATIVE_CURSOR_VERSION,
        kind: "cursor-nativepath".to_owned(),
        canonical_source_identity: canonical_source_identity.to_owned(),
        observation: observation.clone(),
        checkpoint: checkpoint.clone(),
        retained_event_count,
        rejected_records,
        rejections: rejections.to_vec(),
    })?)
}

fn decode_cursor_native_cursor(
    encoded_store_cursor: &str,
) -> Result<Option<CursorNativeCursorWire>> {
    let encoded = ctx_history_store::decode_native_path_committed_cursor(encoded_store_cursor)
        .map(|cursor| cursor.provider_cursor().to_owned())
        .unwrap_or_else(|_| encoded_store_cursor.to_owned());
    let Ok(wire) = serde_json::from_str::<CursorNativeCursorWire>(&encoded) else {
        return Ok(None);
    };
    Ok((wire.version == CURSOR_NATIVE_CURSOR_VERSION
        && wire.kind == "cursor-nativepath"
        && wire.checkpoint.schema_version == CursorCheckpoint::SCHEMA_VERSION
        && wire.checkpoint.parser_revision == CursorCheckpoint::PARSER_REVISION)
        .then_some(wire))
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
                CaptureProvider::Cursor.as_str(),
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

fn cursor_publication_id(
    pages: &[CursorPendingPage],
    transitions: &[NativePathCursorTransition],
) -> String {
    let mut digest = Sha256::new();
    digest.update(CURSOR_PUBLICATION_DOMAIN);
    digest.update((pages.len() as u64).to_be_bytes());
    for pending in pages {
        digest.update(pending.observation.content_sha256);
        digest.update(pending.page.expected_checkpoint.prefix.sha256);
        digest.update(pending.page.next_checkpoint.prefix.sha256);
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
    format!("cursor-nativepath-v1:{:x}", digest.finalize())
}

fn cursor_retirement_publication_id(retirement: &ProviderSourceRouteRetirement) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-cursor-nativepath-route-retirement-v1\0");
    digest.update(retirement.provider.as_str().as_bytes());
    digest.update(retirement.source_format.as_bytes());
    digest.update(retirement.machine_id.as_bytes());
    digest.update(retirement.locator_identity.as_bytes());
    digest.update(retirement.cursor_stream.as_bytes());
    digest.update(retirement.expected_canonical_source_identity.as_bytes());
    digest.update(retirement.expected_source_revision.as_bytes());
    digest.update(format!("{:?}", retirement.reason).as_bytes());
    format!("cursor-nativepath-retirement-v1:{:x}", digest.finalize())
}

fn hex_digest(digest: [u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn cursor_rejection_message(
    kind: crate::provider::providers::cursor::CursorRejectionKind,
    observed_bytes: u64,
) -> String {
    let reason = match kind {
        crate::provider::providers::cursor::CursorRejectionKind::MalformedJson => "malformed JSONL",
        crate::provider::providers::cursor::CursorRejectionKind::Oversized => "oversized JSONL",
        crate::provider::providers::cursor::CursorRejectionKind::UnsupportedShape => {
            "unsupported JSONL shape"
        }
    };
    format!("Cursor {reason} record ({observed_bytes} bytes)")
}
