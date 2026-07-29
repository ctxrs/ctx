use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    Confidence, Event, EventType, Fidelity, FileChangeKind, FileTouched, Session, SessionStatus,
    SyncCursor,
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

use crate::complete_content::{
    attach_verified_content_locator, jsonl::EXACT_JSONL_COMPLETE_CONTENT_LOCATOR_KIND,
    verified_content_address_supported, verified_content_profile, CompleteContentBodyDigest,
    CompleteContentSourceFamily, VerifiedContentLocatorV1, VerifiedContentRole,
};
use crate::provider::{
    importer::{
        provider_event_import_identity_with_exact_legacy_source, provider_file_touch_import_id,
        provider_import_session_uuid, provider_scoped_source_identity_key,
        provider_scoped_source_uuid, provider_source_cursor_stream_for_path,
        provider_sync_metadata, timestamps,
    },
    providers::cursor::{
        cursor_complete_content_source_revision, discover_cursor_transcripts, freeze_cursor_source,
        resolve_cursor_missing_sources, scan_cursor_source_into, CursorCheckpoint,
        CursorCompletedExactInventory, CursorFrozenSource, CursorKnownSource,
        CursorMissingSourceDisposition, CursorNativeEvent, CursorNativeSession,
        CursorPriorObservation, CursorPublicationPage, CursorPublicationSink, CursorReadOutcome,
        CursorRecordRejection, CursorSourceObservation, CursorTranscriptPath,
    },
};
use crate::{
    stable_capture_uuid, CaptureError, CaptureWorkLimit, ProOutputSink, ProOutputSinkError,
    ProviderImportFailure, ProviderImportSummary, ProviderImportWorkResult, Result,
    CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT, LEGACY_CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
};

use super::NativePathJsonlTreeImport;

const CURSOR_NATIVE_CURSOR_VERSION: u32 = 1;
const CURSOR_PUBLICATION_DOMAIN: &[u8] = b"ctx-cursor-nativepath-publication-v1\0";
const CURSOR_EXACT_SOURCE_REVISION_DIGEST_DOMAIN: &[u8] =
    b"ctx-complete-content-source-revision-v1\0";
const CURSOR_EXACT_PATH_IDENTITY_DIGEST_DOMAIN: &[u8] = b"ctx-complete-content-path-identity-v1\0";
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
    let inventory = discover_cursor_transcripts(request.path);
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
            let stored = match stored {
                Some(cursor) => Some(cursor),
                None => accumulator.store.get_sync_cursor(
                    None,
                    &request.machine_id,
                    &provider_source_cursor_stream_for_path(
                        CaptureProvider::Cursor,
                        LEGACY_CURSOR_AGENT_TRANSCRIPT_SOURCE_FORMAT,
                        &frozen.observation().locator_identity,
                    ),
                )?,
            };
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
            let mut sink = CursorAccumulatorSink {
                accumulator: &mut accumulator,
                frozen: frozen.clone(),
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
                        accumulator.record_rejection_sample(
                            rejection.physical_line,
                            rejection.kind,
                            rejection.observed_bytes,
                        );
                    }
                    accumulator.record_unsampled_rejections(
                        generation.rejections.total,
                        generation.rejections.samples.len(),
                    );
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
}

impl CursorPublicationSink for CursorAccumulatorSink<'_, '_> {
    fn begin_cursor_publication(&mut self) -> Result<()> {
        Ok(())
    }

    fn stage_cursor_page(&mut self, page: CursorPublicationPage) -> Result<()> {
        if self.accumulator.stopped {
            return Ok(());
        }
        let retained_event_count = page.retained_event_count;
        self.accumulator.push(CursorPendingPage {
            transcript: self.frozen.transcript().clone(),
            observation: self.frozen.observation().clone(),
            page,
            retained_event_count,
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
                self.record_rejection_sample(
                    rejection.physical_line,
                    rejection.kind,
                    rejection.observed_bytes,
                );
            }
            self.record_unsampled_rejections(wire.rejected_records, wire.rejections.len());
        }
    }

    fn record_rejection_sample(
        &mut self,
        physical_line: u64,
        kind: crate::provider::providers::cursor::CursorRejectionKind,
        observed_bytes: u64,
    ) {
        self.summary.record_failure(ProviderImportFailure {
            line: usize::try_from(physical_line)
                .unwrap_or(usize::MAX)
                .saturating_add(1),
            error: cursor_rejection_message(kind, observed_bytes),
        });
    }

    fn record_unsampled_rejections(&mut self, total: u64, sampled: usize) {
        let unsampled = total.saturating_sub(sampled as u64);
        self.summary.failed = self
            .summary
            .failed
            .saturating_add(usize::try_from(unsampled).unwrap_or(usize::MAX));
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
#[path = "cursor_provider_publication.rs"]
mod publication;
use publication::{
    cursor_event_touch_count, cursor_rejection_message, cursor_retirement_publication_id,
    cursor_source_revision, decode_cursor_native_cursor, encode_cursor_native_cursor,
    provider_sync_cursor, publish_cursor_group,
};
