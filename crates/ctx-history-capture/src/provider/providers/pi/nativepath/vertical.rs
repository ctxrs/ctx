use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    compute_payload_hash, AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor,
    CaptureSourceKind, Event, Fidelity, FileTouched, ProviderSourceTrust, Run, RunStatus, RunType,
    Session, SessionStatus, SyncCursor,
};
use ctx_history_store::{
    decode_native_path_committed_cursor, EventSearchBulkGuard, NativePathCursorSetClassification,
    NativePathCursorTransition, NativePathGroupAccounting, ProviderEventHashAuthority,
    ProviderSourceLocatorObservation, ProviderSourceRouteRetirement,
    ProviderSourceRouteRetirementDisposition, ProviderSourceRouteRetirementReason, Store,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    provider::{
        importer::{
            compact_provider_result_payload,
            provider_event_import_identity_with_exact_legacy_source, provider_file_touch_import_id,
            provider_import_session_uuid, provider_path_identity,
            provider_scoped_source_identity_key, provider_scoped_source_uuid,
            provider_source_cursor_stream_for_path, provider_source_identity,
            provider_sync_metadata, timestamps, CertifiedProviderCursor,
        },
        native_ingestion::{process_pro_replay_only, NativeSafeFrontier},
    },
    stable_capture_uuid, CaptureError, CaptureWorkLimit, ImportProfile, PiSessionImportOptions,
    ProOutputSinkError, ProviderAdapterContext, ProviderImportFailure, ProviderImportSummary,
    ProviderImportWorkResult, Result,
};

use super::{
    discover_pi_sessions, open_pi_native_session, revalidate_pi_source_revision,
    PiNativeCheckpoint, PiNativeCorePage, PiNativeCoreUnit, PiNativeEventRow, PiNativeFileTouchRow,
    PiNativeOpenOutcome, PiNativeOwnedPage, PiNativeProfile, PiNativeResume, PiNativeScanOptions,
    PiNativeSessionRow, PiSourceLifecycle,
};
use crate::provider::providers::pi::PI_SOURCE_FORMAT;

const PI_STORE_CURSOR_VERSION: u32 = 1;
const PI_ROOT_MANIFEST_VERSION: u32 = 1;
const PI_ROOT_CURSOR_FORMAT: &str = "pi_session_jsonl_nativepath_root_v1";
const PI_PUBLICATION_PREFIX: &str = "pi-nativepath-v1:";
const PI_ROOT_PUBLICATION_PREFIX: &str = "pi-nativepath-root-v1:";
const PI_RETIREMENT_PUBLICATION_PREFIX: &str = "pi-nativepath-retire-v1:";
const PI_OUTPUT_PARSER_REVISION: &str = "pi-nativepath:1:1";
const PI_RELEASED_CAPTURE_REVISION: u32 = 2;
const PI_RELEASED_POLICY_REVISION: u32 = 6;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PiStoreCursorWire {
    version: u32,
    checkpoint: PiNativeCheckpoint,
    source_revision: String,
    canonical_source_identity: Option<String>,
    source_id: Option<Uuid>,
    session_id: Option<Uuid>,
    provider_session_id: Option<String>,
    rejected_records: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleasedPiParserCheckpoint {
    header: Option<ReleasedPiSessionHeaderCheckpoint>,
    next_ordinal: u64,
    accepted_captures: u64,
    accepted_events: u64,
    accepted_file_touches: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleasedPiSessionHeaderCheckpoint {
    id: String,
    version: Option<u64>,
    timestamp: DateTime<Utc>,
    cwd: Option<String>,
    parent_session: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PiRootManifest {
    version: u32,
    configured_root: PathBuf,
    source_root: String,
    entries: Vec<PiRootEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PiRootEntry {
    path: PathBuf,
    locator_identity: String,
    cursor_stream: String,
    canonical_source_identity: Option<String>,
    source_revision: String,
}

struct PiCoreState {
    expected_store_cursor: Option<SyncCursor>,
    prior: Option<PiStoreCursorWire>,
}

struct ResolvedPiSession {
    source_id: Uuid,
    session_id: Uuid,
    provider_session_id: String,
    canonical_source_identity: String,
    session: Session,
}

pub(crate) fn import_pi_nativepath_history(
    path: &Path,
    store: &mut Store,
    options: PiSessionImportOptions,
) -> Result<ProviderImportSummary> {
    let configured_root = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let source_root = configured_root.display().to_string();
    let committed_store = Store::open_read_only(store.path())?;
    let prior_manifest =
        load_root_manifest(&committed_store, &options.machine_id, &configured_root)?;
    let (paths, root_missing) = discover_paths(path)?;
    if paths.is_empty() && prior_manifest.is_none() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Pi session root contains no session JSONL files",
        });
    }

    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let mut summary = ProviderImportSummary::default();
        let mut completed_paths = BTreeSet::new();
        let mut changed_core_pages = 0_usize;
        for source_path in &paths {
            let result = import_one_source(
                source_path,
                store,
                &committed_store,
                &bulk_guard,
                &configured_root,
                &source_root,
                &options,
            )?;
            changed_core_pages = changed_core_pages.saturating_add(result.changed_core_pages);
            summary.merge(result.summary);
            completed_paths.insert(source_path.clone());
            if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
                && changed_core_pages != 0
            {
                summary.work_remaining = true;
                return Ok(summary);
            }
        }

        if let Some(prior) = &prior_manifest {
            let current = paths.iter().collect::<BTreeSet<_>>();
            for entry in &prior.entries {
                if current.contains(&entry.path) {
                    continue;
                }
                summary.merge(retire_source_route(
                    store,
                    &bulk_guard,
                    &options,
                    entry,
                    if root_missing {
                        ProviderSourceRouteRetirementReason::RootMissing
                    } else {
                        ProviderSourceRouteRetirementReason::SourceMissing
                    },
                )?);
            }
        }

        let entries = completed_paths
            .iter()
            .map(|source_path| {
                root_entry_from_store(&committed_store, store, &options.machine_id, source_path)
            })
            .collect::<Result<Vec<_>>>()?;
        summary.merge(publish_root_manifest(
            store,
            &bulk_guard,
            &options,
            &configured_root,
            &source_root,
            entries,
        )?);
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

struct SourceImportResult {
    summary: ProviderImportSummary,
    changed_core_pages: usize,
}

#[allow(clippy::too_many_arguments)]
fn import_one_source(
    path: &Path,
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    configured_root: &Path,
    source_root: &str,
    options: &PiSessionImportOptions,
) -> Result<SourceImportResult> {
    let cursor_stream = source_cursor_stream(path)?;
    let core_state = load_core_state(store, &options.machine_id, &cursor_stream)?;
    let sink = options.import_profile.sink().map(|sink| sink.as_ref());
    let output_source = output_source_identity(path, &cursor_stream)?;
    let mut output_progress = None;
    let mut output_observe_failed = false;
    if let Some(sink) = sink {
        match sink.observe_source(&output_source) {
            Ok(progress) => output_progress = progress,
            Err(error) => {
                sink.mark_behind(error);
                output_observe_failed = true;
            }
        }
    }
    if options.import_profile.is_replay_only() && core_state.prior.is_none() {
        return Err(CaptureError::InvalidPayload(
            "Pi output replay requires committed NativePath Core".to_owned(),
        ));
    }

    let progress_frontier = output_progress
        .as_ref()
        .and_then(|progress| progress.cursor.as_ref())
        .and_then(
            |cursor| match NativeSafeFrontier::new(cursor.version, cursor.payload.clone()) {
                Ok(frontier) => Some(frontier),
                Err(error) => {
                    if let Some(sink) = sink {
                        sink.mark_behind(ProOutputSinkError::new(
                            "pi_nativepath_output_cursor",
                            error.to_string(),
                        ));
                    }
                    output_observe_failed = true;
                    None
                }
            },
        );
    let progress_checkpoint = progress_frontier
        .as_ref()
        .and_then(|frontier| PiNativeCheckpoint::decode_frontier(frontier).ok());
    let can_resume_output = output_progress.as_ref().is_some_and(|progress| {
        progress.parser_revision == PI_OUTPUT_PARSER_REVISION
            && sink
                .is_some_and(|sink| progress.materializer_revision == sink.materializer_revision())
            && progress_checkpoint.is_some()
    });
    let prior_epoch = output_progress
        .as_ref()
        .map(|progress| progress.source_epoch);
    let rewrite_epoch = prior_epoch
        .and_then(|epoch| {
            epoch.checked_add(1).or_else(|| {
                if let Some(sink) = sink {
                    sink.mark_behind(ProOutputSinkError::new(
                        "pi_nativepath_output_epoch",
                        "Pi output source epoch is exhausted",
                    ));
                }
                output_observe_failed = true;
                None
            })
        })
        .unwrap_or(0);
    let native_profile = match &options.import_profile {
        ImportProfile::CoreOnly => PiNativeProfile::CoreOnly,
        ImportProfile::CoreAndPro(_) if output_observe_failed => PiNativeProfile::CoreOnly,
        ImportProfile::CoreAndPro(_) => PiNativeProfile::CoreAndPro,
        ImportProfile::ProReplayOnly(_) if output_observe_failed => PiNativeProfile::CoreOnly,
        ImportProfile::ProReplayOnly(_) => PiNativeProfile::CoreAndPro,
    };
    let context = ProviderAdapterContext {
        machine_id: options.machine_id.clone(),
        source_path: Some(path.to_path_buf()),
        source_root: Some(configured_root.to_path_buf()),
        imported_at: options.imported_at,
    };
    let mut scan_options = PiNativeScanOptions::new(context, native_profile);
    scan_options.resume = PiNativeResume {
        core: core_state
            .prior
            .as_ref()
            .map(|prior| prior.checkpoint.clone()),
        output: can_resume_output
            .then(|| progress_checkpoint.clone())
            .flatten(),
    };
    if let Some(sink) = sink {
        scan_options.inventory_generation = sink.inventory_generation();
        scan_options.output_materializer_revision = sink.materializer_revision().to_owned();
    }
    scan_options.output_source_epoch = prior_epoch.unwrap_or(0);
    scan_options.rewrite_output_source_epoch = rewrite_epoch;
    scan_options.expected_prior_output_source_epoch = prior_epoch;
    scan_options.expected_prior_output_frontier = progress_frontier;
    scan_options.force_output_rewrite = output_progress.is_some() && !can_resume_output;

    let PiNativeOpenOutcome::Ready(mut scanner) =
        open_pi_native_session(path, scan_options).map_err(map_native_error)?
    else {
        return Err(CaptureError::SourceChangedDuringCapture);
    };
    if options.import_profile.is_replay_only()
        && scanner.core_lifecycle() != Some(PiSourceLifecycle::NoOp)
    {
        return Err(CaptureError::InvalidPayload(
            "Pi output replay source no longer matches committed Core".to_owned(),
        ));
    }

    let mut summary = ProviderImportSummary::default();
    let mut changed_core_pages = 0_usize;
    let mut current_core = core_state
        .prior
        .as_ref()
        .map(|prior| prior.checkpoint.clone());
    let mut output_failed = output_observe_failed;
    while let Some(page) = scanner.next_page().map_err(map_native_error)? {
        match page {
            PiNativeOwnedPage::Core(page) => {
                if options.import_profile.is_replay_only() {
                    if !page.core.units.is_empty() {
                        return Err(CaptureError::SystemInvariant(
                            "Pi replay-only scan attempted a Core mutation",
                        ));
                    }
                    continue;
                }
                let page_summary = publish_core_page(
                    store,
                    committed_store,
                    bulk_guard,
                    source_root,
                    options,
                    path,
                    &cursor_stream,
                    scanner.source_revision(),
                    &core_state,
                    page,
                )?;
                if page_summary.work_result() == ProviderImportWorkResult::Changed {
                    changed_core_pages = changed_core_pages.saturating_add(1);
                }
                current_core = load_core_state(store, &options.machine_id, &cursor_stream)?
                    .prior
                    .map(|prior| prior.checkpoint);
                summary.merge(page_summary);
                if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
                    && changed_core_pages != 0
                {
                    summary.work_remaining = true;
                    return Ok(SourceImportResult {
                        summary,
                        changed_core_pages,
                    });
                }
            }
            PiNativeOwnedPage::Output(page) => {
                let next = PiNativeCheckpoint::decode_frontier(&page.next_safe_frontier)
                    .map_err(map_native_error)?;
                if output_failed {
                    continue;
                }
                let sink = sink.ok_or(CaptureError::SystemInvariant(
                    "Pi output page has no selected sink",
                ))?;
                if current_core
                    .as_ref()
                    .is_none_or(|authority| !checkpoint_covers(authority, &next))
                {
                    sink.mark_behind(ProOutputSinkError::new(
                        "pi_nativepath_core_authority_pending",
                        "Pi output replay is waiting for the canonical Core cursor",
                    ));
                    output_failed = true;
                    continue;
                }
                if let Err(failure) = process_pro_replay_only(*page, sink) {
                    sink.mark_behind(ProOutputSinkError::new(
                        "pi_nativepath_output_replay",
                        format!("{:?}", failure.output_error),
                    ));
                    output_failed = true;
                }
            }
        }
    }
    let outcome = scanner.outcome().ok_or(CaptureError::SystemInvariant(
        "Pi NativePath scanner did not finish",
    ))?;
    if !outcome.complete {
        summary.work_remaining = true;
    }
    Ok(SourceImportResult {
        summary,
        changed_core_pages,
    })
}

#[allow(clippy::too_many_arguments)]
fn publish_core_page(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    source_root: &str,
    options: &PiSessionImportOptions,
    path: &Path,
    cursor_stream: &str,
    source_revision: &str,
    _initial_state: &PiCoreState,
    page: crate::provider::native_ingestion::NativeIngestionPage<PiNativeCorePage>,
) -> Result<ProviderImportSummary> {
    let expected_checkpoint =
        PiNativeCheckpoint::decode_frontier(&page.expected_frontier).map_err(map_native_error)?;
    let next_checkpoint =
        PiNativeCheckpoint::decode_frontier(&page.next_safe_frontier).map_err(map_native_error)?;
    let current_state = load_core_state(store, &options.machine_id, cursor_stream)?;
    let resets_generation = expected_checkpoint.complete_offset == 0
        && current_state
            .prior
            .as_ref()
            .is_some_and(|prior| prior.checkpoint != expected_checkpoint);
    if current_state.prior.as_ref().map(|prior| &prior.checkpoint) != Some(&expected_checkpoint)
        && !(current_state.prior.is_none() && expected_checkpoint.complete_offset == 0)
        && !resets_generation
    {
        return Err(CaptureError::InvalidPayload(
            "Pi NativePath Core cursor conflict".to_owned(),
        ));
    }
    let mut next_wire = current_state
        .prior
        .clone()
        .filter(|_| !resets_generation)
        .unwrap_or(PiStoreCursorWire {
            version: PI_STORE_CURSOR_VERSION,
            checkpoint: expected_checkpoint,
            source_revision: source_revision.to_owned(),
            canonical_source_identity: None,
            source_id: None,
            session_id: None,
            provider_session_id: None,
            rejected_records: 0,
        });
    next_wire.checkpoint = next_checkpoint;
    next_wire.source_revision = source_revision.to_owned();
    next_wire.rejected_records = next_wire.rejected_records.saturating_add(
        u64::try_from(
            page.core
                .units
                .iter()
                .filter(|unit| matches!(unit, PiNativeCoreUnit::Rejection(_)))
                .count(),
        )
        .unwrap_or(u64::MAX),
    );
    prime_cursor_identity(
        committed_store,
        source_root,
        options,
        path,
        &page.core,
        &mut next_wire,
    )?;
    let encoded = serde_json::to_string(&next_wire)?;
    let next_cursor = SyncCursor {
        id: Uuid::new_v4(),
        team_id: None,
        device_id: options.machine_id.clone(),
        stream: cursor_stream.to_owned(),
        cursor: encoded,
        last_synced_at: Some(options.imported_at),
        timestamps: timestamps(options.imported_at),
    };
    let transition = NativePathCursorTransition::new(
        current_state
            .expected_store_cursor
            .as_ref()
            .map(|cursor| cursor.cursor.clone()),
        next_cursor,
    );
    let publication_id = publication_id(path, &page, &transition);
    let accounting =
        NativePathGroupAccounting::new(1, 1, page.accounting.conservative_serialized_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }

    let raw_source_path = path.display().to_string();
    let locator_identity = provider_path_identity(path)?;
    let mut resolved = current_state
        .prior
        .as_ref()
        .map(|prior| hydrate_prior_session(committed_store, prior))
        .transpose()?
        .flatten();
    let mut events = BTreeMap::new();
    let mut summary = ProviderImportSummary::default();
    for unit in &page.core.units {
        match unit {
            PiNativeCoreUnit::Session(row) => {
                let session = resolve_session(
                    committed_store,
                    &mut group,
                    source_root,
                    options,
                    &raw_source_path,
                    &locator_identity,
                    cursor_stream,
                    source_revision,
                    row,
                )?;
                if next_wire.canonical_source_identity.as_deref()
                    != Some(session.canonical_source_identity.as_str())
                    || next_wire.source_id != Some(session.source_id)
                    || next_wire.session_id != Some(session.session_id)
                    || next_wire.provider_session_id.as_deref()
                        != Some(session.provider_session_id.as_str())
                {
                    return Err(CaptureError::SystemInvariant(
                        "Pi NativePath resolved identity changed after cursor certification",
                    ));
                }
                summary.imported_sessions = summary.imported_sessions.saturating_add(1);
                summary.imported = summary.imported.saturating_add(1);
                resolved = Some(session);
            }
            PiNativeCoreUnit::Event(row) => {
                let session = resolved.as_ref().ok_or(CaptureError::SystemInvariant(
                    "Pi NativePath event page has no resolved session",
                ))?;
                let event = publish_event(
                    committed_store,
                    &mut group,
                    options,
                    session,
                    row,
                    &mut summary,
                )?;
                events.insert(row.provider_event_index, event);
            }
            PiNativeCoreUnit::FileTouch(row) => {
                let session = resolved.as_ref().ok_or(CaptureError::SystemInvariant(
                    "Pi NativePath file-touch page has no resolved session",
                ))?;
                publish_file_touch(
                    committed_store,
                    &mut group,
                    options,
                    session,
                    row,
                    events.get(&row.provider_event_index.unwrap_or(u64::MAX)),
                    &mut summary,
                )?;
            }
            PiNativeCoreUnit::Rejection(rejection) => {
                summary.record_failure(ProviderImportFailure {
                    line: usize::try_from(rejection.line_number).unwrap_or(usize::MAX),
                    error: rejection.diagnostic.clone(),
                });
            }
        }
    }
    if !revalidate_pi_source_revision(path, source_revision).map_err(map_native_error)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

fn prime_cursor_identity(
    committed_store: &Store,
    source_root: &str,
    options: &PiSessionImportOptions,
    path: &Path,
    core: &PiNativeCorePage,
    cursor: &mut PiStoreCursorWire,
) -> Result<()> {
    let provider_session_id = core.units.iter().find_map(|unit| match unit {
        PiNativeCoreUnit::Session(row) => Some(row.provider_session_id.as_str()),
        PiNativeCoreUnit::Event(row) => Some(row.provider_session_id.as_str()),
        PiNativeCoreUnit::FileTouch(row) => Some(row.provider_session_id.as_str()),
        PiNativeCoreUnit::Rejection(_) => None,
    });
    let Some(provider_session_id) = provider_session_id else {
        return Ok(());
    };
    if cursor
        .provider_session_id
        .as_deref()
        .is_some_and(|prior| prior != provider_session_id)
    {
        cursor.source_id = None;
        cursor.session_id = None;
    }
    let raw_source_path = path.display().to_string();
    let canonical_source_identity = provider_source_identity(
        CaptureProvider::Pi,
        PI_SOURCE_FORMAT,
        Some(source_root),
        Some(&raw_source_path),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Pi NativePath source has no canonical identity",
    ))?;
    let source_id = cursor
        .source_id
        .or(committed_store
            .capture_source_by_canonical_identity_session(
                CaptureProvider::Pi,
                PI_SOURCE_FORMAT,
                &options.machine_id,
                &canonical_source_identity,
                provider_session_id,
            )?
            .map(|source| source.id))
        .unwrap_or_else(|| {
            provider_scoped_source_uuid(
                CaptureProvider::Pi,
                provider_session_id,
                PI_SOURCE_FORMAT,
                Some(&raw_source_path),
            )
        });
    let session_id = provider_import_session_uuid(
        committed_store,
        CaptureProvider::Pi,
        provider_session_id,
        source_id,
        Some(&canonical_source_identity),
    )?;
    cursor.canonical_source_identity = Some(canonical_source_identity);
    cursor.source_id = Some(source_id);
    cursor.session_id = Some(session_id);
    cursor.provider_session_id = Some(provider_session_id.to_owned());
    Ok(())
}

fn hydrate_prior_session(
    committed_store: &Store,
    cursor: &PiStoreCursorWire,
) -> Result<Option<ResolvedPiSession>> {
    let (Some(source_id), Some(session_id), Some(provider_session_id), Some(canonical)) = (
        cursor.source_id,
        cursor.session_id,
        cursor.provider_session_id.as_ref(),
        cursor.canonical_source_identity.as_ref(),
    ) else {
        return Ok(None);
    };
    let session = committed_store.get_session(session_id)?;
    Ok(Some(ResolvedPiSession {
        source_id,
        session_id,
        provider_session_id: provider_session_id.clone(),
        canonical_source_identity: canonical.clone(),
        session,
    }))
}

#[allow(clippy::too_many_arguments)]
fn resolve_session(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    source_root: &str,
    options: &PiSessionImportOptions,
    raw_source_path: &str,
    locator_identity: &str,
    cursor_stream: &str,
    source_revision: &str,
    row: &PiNativeSessionRow,
) -> Result<ResolvedPiSession> {
    let proposed_source_identity = provider_source_identity(
        CaptureProvider::Pi,
        PI_SOURCE_FORMAT,
        Some(source_root),
        Some(raw_source_path),
        Some(&row.source_idempotency_key),
        &row.source_metadata,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Pi NativePath source has no canonical identity",
    ))?;
    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::Pi,
            source_format: PI_SOURCE_FORMAT.to_owned(),
            machine_id: options.machine_id.clone(),
            locator_identity: locator_identity.to_owned(),
            cursor_stream: cursor_stream.to_owned(),
            proposed_source_identity,
            raw_source_path: Some(raw_source_path.to_owned()),
            source_revision: source_revision.to_owned(),
            observed_at_ms: options.imported_at.timestamp_millis(),
        })?;
    let source_id = if resolution.relocated {
        committed_store
            .capture_source_by_canonical_identity_session(
                CaptureProvider::Pi,
                PI_SOURCE_FORMAT,
                &options.machine_id,
                &resolution.canonical_source_identity,
                &row.provider_session_id,
            )?
            .map(|source| source.id)
            .unwrap_or_else(|| {
                provider_scoped_source_uuid(
                    CaptureProvider::Pi,
                    &row.provider_session_id,
                    PI_SOURCE_FORMAT,
                    Some(raw_source_path),
                )
            })
    } else {
        provider_scoped_source_uuid(
            CaptureProvider::Pi,
            &row.provider_session_id,
            PI_SOURCE_FORMAT,
            Some(raw_source_path),
        )
    };
    let source = CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Pi,
            machine_id: options.machine_id.clone(),
            process_id: None,
            cwd: row.cwd.clone(),
            raw_source_path: Some(raw_source_path.to_owned()),
            source_format: Some(PI_SOURCE_FORMAT.to_owned()),
            source_root: Some(source_root.to_owned()),
            source_identity: Some(resolution.canonical_source_identity.clone()),
            external_session_id: Some(row.provider_session_id.clone()),
        },
        started_at: row.started_at,
        ended_at: None,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": row.provider_session_id,
                "source_format": PI_SOURCE_FORMAT,
                "source_trust": ProviderSourceTrust::ProviderExport,
                "imported_at": options.imported_at,
                "source_idempotency_key": row.source_idempotency_key,
                "source_identity": resolution.canonical_source_identity,
                "source_root": source_root,
                "source_identity_key": provider_scoped_source_identity_key(
                    CaptureProvider::Pi,
                    &row.provider_session_id,
                    PI_SOURCE_FORMAT,
                    Some(raw_source_path),
                ),
                "source_metadata": row.source_metadata,
                "session_metadata": row.session_metadata,
                "source_revision": source_revision,
                "nativepath_publication": 1,
            }),
        ),
    };
    group.upsert_capture_source(&source)?;
    group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;
    let session_id = provider_import_session_uuid(
        committed_store,
        CaptureProvider::Pi,
        &row.provider_session_id,
        source_id,
        Some(&resolution.canonical_source_identity),
    )?;
    let session = Session {
        id: session_id,
        history_record_id: options.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Pi,
        external_session_id: Some(row.provider_session_id.clone()),
        external_agent_id: None,
        agent_type: AgentType::Primary,
        role_hint: Some("primary".to_owned()),
        is_primary: true,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: row.started_at,
        ended_at: None,
        timestamps: timestamps(options.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": row.provider_session_id,
                "source_format": PI_SOURCE_FORMAT,
                "source_trust": ProviderSourceTrust::ProviderExport,
                "imported_at": options.imported_at,
                "session_idempotency_key": row.session_idempotency_key,
                "metadata": row.session_metadata,
            }),
        ),
    };
    group.upsert_session(&session)?;
    Ok(ResolvedPiSession {
        source_id,
        session_id,
        provider_session_id: row.provider_session_id.clone(),
        canonical_source_identity: resolution.canonical_source_identity,
        session,
    })
}

fn publish_event(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    options: &PiSessionImportOptions,
    resolved: &ResolvedPiSession,
    row: &PiNativeEventRow,
    summary: &mut ProviderImportSummary,
) -> Result<Event> {
    let event_hash = compute_payload_hash(&row.payload)?;
    let event_hash_authority = ProviderEventHashAuthority::NormalizedPayloadFallback;
    let identity = provider_event_import_identity_with_exact_legacy_source(
        committed_store,
        CaptureProvider::Pi,
        &row.provider_session_id,
        resolved.source_id,
        row.provider_event_identity_index,
        row.provider_event_index,
        &event_hash,
        None,
        Some(row.provider_event_index),
        resolved.session_id
            == crate::provider::importer::provider_session_uuid(
                CaptureProvider::Pi,
                &row.provider_session_id,
            ),
    )?;
    let line_number = usize::try_from(row.locator.line_number).unwrap_or(usize::MAX);
    let run = pi_command_run(
        row,
        &event_hash,
        identity.run_source_id,
        options.history_record_id,
        resolved.session_id,
        resolved.source_id,
    )?;
    if let Some(run) = &run {
        group.upsert_run(run)?;
    }
    let dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, &event_hash)
            .unwrap_or(identity.dedupe_key);
    let event = Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id: options.history_record_id,
        session_id: Some(resolved.session_id),
        run_id: run.as_ref().map(|run| run.id),
        event_type: row.event_type,
        role: row.role,
        occurred_at: row.occurred_at,
        capture_source_id: Some(resolved.source_id),
        payload: json!({
            "provider": CaptureProvider::Pi.as_str(),
            "provider_session_id": row.provider_session_id,
            "provider_event_index": row.provider_event_index,
            "provider_event_hash": event_hash,
            "cursor": row.cursor,
            "artifacts": [],
            "body": compact_provider_result_payload(row.event_type, &row.payload),
        }),
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": row.provider_session_id,
                "provider_event_index": row.provider_event_index,
                "provider_event_hash": event_hash,
                "provider_event_hash_authority": event_hash_authority.as_str(),
                "cursor": row.cursor,
                "source_format": PI_SOURCE_FORMAT,
                "source_trust": ProviderSourceTrust::ProviderExport,
                "fixture_line": line_number,
                "imported_at": options.imported_at,
                "event_idempotency_key": row.idempotency_key,
                "source_record_ordinal": row.locator.source_record_ordinal,
                "source_record_subrecord_index": 0_u32,
                "metadata": row.metadata,
            }),
        ),
    };
    if group.reconcile_provider_event(&event, event_hash_authority)? {
        summary.imported_events = summary.imported_events.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    } else {
        summary.skipped_events = summary.skipped_events.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    }
    summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
    Ok(event)
}

fn pi_command_run(
    row: &PiNativeEventRow,
    event_hash: &str,
    run_source_id: Option<Uuid>,
    history_record_id: Option<Uuid>,
    session_id: Uuid,
    source_id: Uuid,
) -> Result<Option<Run>> {
    if row.event_type != ctx_history_core::EventType::CommandOutput {
        return Ok(None);
    }
    let duration_ms = match row.payload.get("duration_ms") {
        None | Some(Value::Null) => None,
        Some(value) => Some(value.as_i64().ok_or_else(|| {
            CaptureError::InvalidPayload("duration_ms must be an integer".to_owned())
        })?),
    };
    let started_at = match duration_ms {
        Some(duration_ms) if duration_ms < 0 => {
            return Err(CaptureError::InvalidPayload(format!(
                "duration_ms must be nonnegative, got {duration_ms}"
            )));
        }
        Some(duration_ms) => {
            let duration = chrono::Duration::try_milliseconds(duration_ms).ok_or_else(|| {
                CaptureError::InvalidPayload(format!(
                    "duration_ms is not representable as milliseconds: {duration_ms}"
                ))
            })?;
            row.occurred_at
                .checked_sub_signed(duration)
                .ok_or_else(|| {
                    CaptureError::InvalidPayload(format!(
                        "duration_ms moves command start before representable time: {duration_ms}"
                    ))
                })?
        }
        None => row.occurred_at,
    };
    let call_id = row.payload.get("call_id").and_then(Value::as_str);
    let run_key = call_id.unwrap_or(event_hash);
    let run_id = run_source_id.map_or_else(
        || {
            stable_capture_uuid(
                &format!(
                    "provider:{}:{}:run:{run_key}",
                    CaptureProvider::Pi.as_str(),
                    row.provider_session_id
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
    let command_preview = row
        .payload
        .get("command")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned);
    let cwd = row
        .payload
        .get("workdir")
        .or_else(|| row.payload.get("cwd"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned);
    Ok(Some(Run {
        id: run_id,
        history_record_id,
        session_id: Some(session_id),
        run_type: RunType::Command,
        status: pi_command_run_status(&row.payload),
        started_at,
        ended_at: Some(row.occurred_at),
        exit_code: row
            .payload
            .get("exit_code")
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok()),
        cwd,
        command_preview,
        input_blob_id: None,
        output_blob_id: None,
        timestamps: timestamps(row.occurred_at),
        source_id: Some(source_id),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": row.provider_session_id,
                "provider_event_index": row.provider_event_index,
                "provider_event_hash": event_hash,
                "call_id": call_id,
                "source": "provider_command_output",
            }),
        ),
    }))
}

fn pi_command_run_status(payload: &Value) -> RunStatus {
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

fn publish_file_touch(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    options: &PiSessionImportOptions,
    resolved: &ResolvedPiSession,
    row: &PiNativeFileTouchRow,
    event: Option<&Event>,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let id = provider_file_touch_import_id(
        committed_store,
        CaptureProvider::Pi,
        &row.provider_session_id,
        resolved.source_id,
        row.provider_event_index,
        row.provider_touch_index,
        resolved.session_id
            == crate::provider::importer::provider_session_uuid(
                CaptureProvider::Pi,
                &row.provider_session_id,
            ),
    )?;
    group.upsert_file_touched(&FileTouched {
        id,
        history_record_id: options.history_record_id,
        run_id: None,
        event_id: event.map(|event| event.id),
        vcs_workspace_id: None,
        path: row.path.clone(),
        change_kind: row.change_kind,
        old_path: row.old_path.clone(),
        line_count_delta: row.line_count_delta,
        confidence: row.confidence,
        timestamps: timestamps(row.occurred_at),
        source_id: Some(resolved.source_id),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider": CaptureProvider::Pi.as_str(),
                "provider_session_id": row.provider_session_id,
                "provider_touch_index": row.provider_touch_index,
                "provider_event_index": row.provider_event_index,
                "source_format": row.source_format,
                "raw_source_path": row.raw_source_path,
                "source_root": row.source_root,
                "metadata": row.metadata,
                "session_id": resolved.session.id,
            }),
        ),
    })?;
    summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
    Ok(())
}

fn load_core_state(store: &Store, machine_id: &str, stream: &str) -> Result<PiCoreState> {
    let stored = store.get_sync_cursor(None, machine_id, stream)?;
    let prior = stored
        .as_ref()
        .map(|cursor| decode_core_cursor(&cursor.cursor))
        .transpose()?
        .flatten();
    Ok(PiCoreState {
        expected_store_cursor: stored,
        prior,
    })
}

fn decode_core_cursor(encoded: &str) -> Result<Option<PiStoreCursorWire>> {
    let provider_cursor = match decode_native_path_committed_cursor(encoded) {
        Ok(committed) => committed.provider_cursor().to_owned(),
        Err(error) => {
            let resembles_native_envelope = serde_json::from_str::<Value>(encoded)
                .ok()
                .and_then(|value| value.as_object().cloned())
                .is_some_and(|object| {
                    object.contains_key("publication_id") || object.contains_key("provider_cursor")
                });
            if resembles_native_envelope {
                return Err(CaptureError::Store(error));
            }
            encoded.to_owned()
        }
    };
    if let Ok(wire) = serde_json::from_str::<PiStoreCursorWire>(&provider_cursor) {
        if wire.version != PI_STORE_CURSOR_VERSION {
            return Err(CaptureError::InvalidPayload(
                "unsupported Pi NativePath Store cursor".to_owned(),
            ));
        }
        return Ok(Some(wire));
    }
    validate_released_cursor(&provider_cursor)?;
    Ok(None)
}

fn validate_released_cursor(encoded: &str) -> Result<()> {
    let cursor = CertifiedProviderCursor::decode_if_certified(encoded)?.ok_or_else(|| {
        CaptureError::InvalidPayload(
            "Pi cursor is neither NativePath nor a released certified cursor".to_owned(),
        )
    })?;
    if cursor.parser_revision() != PI_RELEASED_CAPTURE_REVISION
        || cursor.policy_revision() != PI_RELEASED_POLICY_REVISION
    {
        return Err(CaptureError::InvalidPayload(
            "Pi released cursor has unsupported revisions".to_owned(),
        ));
    }
    crate::released_jsonl_cursor::released_jsonl_position_offset(cursor.native_position())
        .map_err(|_| {
            CaptureError::InvalidPayload("Pi released cursor position is malformed".to_owned())
        })?;
    let checkpoint: ReleasedPiParserCheckpoint = cursor.parser_checkpoint().deserialize()?;
    validate_released_checkpoint(&checkpoint)
}

fn validate_released_checkpoint(checkpoint: &ReleasedPiParserCheckpoint) -> Result<()> {
    if checkpoint.accepted_captures > checkpoint.next_ordinal
        || checkpoint.accepted_events > checkpoint.accepted_captures
    {
        return Err(CaptureError::InvalidPayload(
            "Pi released cursor checkpoint counters are inconsistent".to_owned(),
        ));
    }
    if let Some(header) = &checkpoint.header {
        if header.id.trim().is_empty() {
            return Err(CaptureError::InvalidPayload(
                "Pi released cursor session identity is empty".to_owned(),
            ));
        }
        let _ = (
            header.version,
            header.timestamp,
            &header.cwd,
            &header.parent_session,
        );
    }
    let _ = checkpoint.accepted_file_touches;
    Ok(())
}

#[cfg(test)]
pub(super) fn released_cursor_for_test() -> String {
    CertifiedProviderCursor::new(
        "released-pi-source-revision",
        PI_RELEASED_CAPTURE_REVISION,
        PI_RELEASED_POLICY_REVISION,
        crate::provider::importer::released_jsonl_initial_position_for_test(),
        crate::provider::importer::BoundedParserCheckpoint::from_serializable(
            &ReleasedPiParserCheckpoint {
                header: None,
                next_ordinal: 0,
                accepted_captures: 0,
                accepted_events: 0,
                accepted_file_touches: 0,
            },
        )
        .expect("released Pi checkpoint"),
    )
    .expect("released Pi cursor")
    .encode()
    .expect("encode released Pi cursor")
}

fn checkpoint_covers(committed: &PiNativeCheckpoint, candidate: &PiNativeCheckpoint) -> bool {
    committed.revisions_match()
        && candidate.revisions_match()
        && committed.route_sha256 == candidate.route_sha256
        && committed.physical_file_id == candidate.physical_file_id
        && committed.complete_offset >= candidate.complete_offset
        && (committed.complete_offset != candidate.complete_offset
            || committed.committed_prefix_sha256 == candidate.committed_prefix_sha256)
}

fn discover_paths(path: &Path) -> Result<(Vec<PathBuf>, bool)> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok((vec![path.to_path_buf()], false)),
        Ok(_) => discover_pi_sessions(path)
            .map(|discovery| (discovery.sessions, false))
            .map_err(map_native_error),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok((Vec::new(), true)),
        Err(error) => Err(CaptureError::Io(error)),
    }
}

pub(super) fn source_cursor_stream(path: &Path) -> Result<String> {
    let identity = provider_path_identity(path)?;
    Ok(provider_source_cursor_stream_for_path(
        CaptureProvider::Pi,
        PI_SOURCE_FORMAT,
        &identity,
    ))
}

fn output_source_identity(path: &Path, cursor_stream: &str) -> Result<crate::OutputSourceIdentity> {
    let canonical = fs::canonicalize(path)?;
    Ok(crate::OutputSourceIdentity {
        provider: CaptureProvider::Pi.as_str().to_owned(),
        namespace_id: cursor_stream.to_owned(),
        source_id: format!("pi-jsonl-file:{}", provider_path_identity(&canonical)?),
    })
}

fn root_stream(path: &Path) -> Result<String> {
    let identity = provider_path_identity(path)?;
    Ok(provider_source_cursor_stream_for_path(
        CaptureProvider::Pi,
        PI_ROOT_CURSOR_FORMAT,
        &identity,
    ))
}

fn load_root_manifest(
    store: &Store,
    machine_id: &str,
    configured_root: &Path,
) -> Result<Option<PiRootManifest>> {
    let stream = root_stream(configured_root)?;
    let Some(stored) = store.get_sync_cursor(None, machine_id, &stream)? else {
        return Ok(None);
    };
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let manifest: PiRootManifest = serde_json::from_str(committed.provider_cursor())?;
    if manifest.version != PI_ROOT_MANIFEST_VERSION || manifest.configured_root != configured_root {
        return Err(CaptureError::InvalidPayload(
            "Pi NativePath root manifest is inconsistent".to_owned(),
        ));
    }
    Ok(Some(manifest))
}

fn root_entry_from_store(
    committed_store: &Store,
    live_store: &Store,
    machine_id: &str,
    path: &Path,
) -> Result<PiRootEntry> {
    let cursor_stream = source_cursor_stream(path)?;
    let state = load_core_state(live_store, machine_id, &cursor_stream)?;
    let prior = state.prior;
    let locator_identity = provider_path_identity(path)?;
    let _ = committed_store;
    Ok(PiRootEntry {
        path: path.to_path_buf(),
        locator_identity,
        cursor_stream,
        canonical_source_identity: prior
            .as_ref()
            .and_then(|prior| prior.canonical_source_identity.clone()),
        source_revision: prior
            .as_ref()
            .map_or_else(String::new, |prior| prior.source_revision.clone()),
    })
}

fn publish_root_manifest(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    options: &PiSessionImportOptions,
    configured_root: &Path,
    source_root: &str,
    mut entries: Vec<PiRootEntry>,
) -> Result<ProviderImportSummary> {
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    let manifest = PiRootManifest {
        version: PI_ROOT_MANIFEST_VERSION,
        configured_root: configured_root.to_path_buf(),
        source_root: source_root.to_owned(),
        entries,
    };
    let encoded = serde_json::to_string(&manifest)?;
    if encoded.len() > ctx_history_store::NATIVE_PATH_MAX_RETAINED_PAGE_BYTES {
        return Err(CaptureError::InvalidPayload(
            "Pi NativePath root manifest exceeds the Store bound".to_owned(),
        ));
    }
    let stream = root_stream(configured_root)?;
    let stored = store.get_sync_cursor(None, &options.machine_id, &stream)?;
    if let Some(stored) = &stored {
        if let Ok(committed) = decode_native_path_committed_cursor(&stored.cursor) {
            if committed.provider_cursor() == encoded {
                let mut summary = ProviderImportSummary::default();
                summary.set_work_result(ProviderImportWorkResult::NoOp);
                return Ok(summary);
            }
        }
    }
    let next = SyncCursor {
        id: Uuid::new_v4(),
        team_id: None,
        device_id: options.machine_id.clone(),
        stream: stream.clone(),
        cursor: encoded,
        last_synced_at: Some(options.imported_at),
        timestamps: timestamps(options.imported_at),
    };
    let transition =
        NativePathCursorTransition::new(stored.as_ref().map(|cursor| cursor.cursor.clone()), next);
    let publication_id = root_publication_id(&manifest, &transition);
    let accounting = NativePathGroupAccounting::new(1, 1, transition.next().cursor.len())?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    let mut summary = ProviderImportSummary::default();
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

fn retire_source_route(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    options: &PiSessionImportOptions,
    entry: &PiRootEntry,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<ProviderImportSummary> {
    let Some(canonical_source_identity) = entry.canonical_source_identity.as_ref() else {
        return Ok(ProviderImportSummary::default());
    };
    let stored = store
        .get_sync_cursor(None, &options.machine_id, &entry.cursor_stream)?
        .ok_or(CaptureError::SystemInvariant(
            "Pi route retirement lost its Core cursor",
        ))?;
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::Pi,
        source_format: PI_SOURCE_FORMAT.to_owned(),
        machine_id: options.machine_id.clone(),
        locator_identity: entry.locator_identity.clone(),
        cursor_stream: entry.cursor_stream.clone(),
        expected_canonical_source_identity: canonical_source_identity.clone(),
        expected_source_revision: entry.source_revision.clone(),
        retired_at_ms: options.imported_at.timestamp_millis(),
        reason,
    };
    let publication_id = retirement_publication_id(&retirement);
    if decode_native_path_committed_cursor(&stored.cursor)
        .is_ok_and(|committed| committed.publication_id() == publication_id)
    {
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }
    let provider_cursor = decode_native_path_committed_cursor(&stored.cursor)?
        .provider_cursor()
        .to_owned();
    let next = SyncCursor {
        id: Uuid::new_v4(),
        team_id: None,
        device_id: options.machine_id.clone(),
        stream: entry.cursor_stream.clone(),
        cursor: provider_cursor,
        last_synced_at: Some(options.imported_at),
        timestamps: timestamps(options.imported_at),
    };
    let transition = NativePathCursorTransition::new(Some(stored.cursor), next);
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store
        .begin_native_path_publication_group(admission, NativePathGroupAccounting::new(0, 1, 0)?)?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }
    let disposition = group.retire_provider_source_route(&retirement)?;
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    let mut summary = ProviderImportSummary::default();
    summary.set_work_result(match disposition {
        ProviderSourceRouteRetirementDisposition::Retired => ProviderImportWorkResult::Changed,
        ProviderSourceRouteRetirementDisposition::AlreadyRetired => ProviderImportWorkResult::NoOp,
    });
    Ok(summary)
}

fn publication_id(
    path: &Path,
    page: &crate::provider::native_ingestion::NativeIngestionPage<PiNativeCorePage>,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-pi-nativepath-publication-v1\0");
    digest.update(path.as_os_str().as_encoded_bytes());
    digest.update(page.expected_frontier.version.to_le_bytes());
    digest.update(&page.expected_frontier.bytes);
    digest.update(page.next_safe_frontier.version.to_le_bytes());
    digest.update(&page.next_safe_frontier.bytes);
    digest.update(transition.next().cursor.as_bytes());
    format!("{PI_PUBLICATION_PREFIX}{:x}", digest.finalize())
}

fn root_publication_id(
    manifest: &PiRootManifest,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-pi-nativepath-root-publication-v1\0");
    digest.update(manifest.configured_root.as_os_str().as_encoded_bytes());
    digest.update(transition.next().cursor.as_bytes());
    format!("{PI_ROOT_PUBLICATION_PREFIX}{:x}", digest.finalize())
}

fn retirement_publication_id(retirement: &ProviderSourceRouteRetirement) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-pi-nativepath-retirement-v1\0");
    digest.update(retirement.locator_identity.as_bytes());
    digest.update(retirement.cursor_stream.as_bytes());
    digest.update(retirement.expected_canonical_source_identity.as_bytes());
    digest.update(retirement.expected_source_revision.as_bytes());
    format!("{PI_RETIREMENT_PUBLICATION_PREFIX}{:x}", digest.finalize())
}

fn map_native_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}
