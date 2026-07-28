use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
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
    ProviderSourceLocatorObservation, ProviderSourceLocatorResolution,
    ProviderSourceRouteRetirement, ProviderSourceRouteRetirementDisposition,
    ProviderSourceRouteRetirementReason, Store,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    complete_content::VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
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

use super::source::PiNativePathError;
use super::{
    discover_pi_sessions, open_pi_native_session_retained, revalidate_pi_source_revision,
    PiDiscovery, PiNativeCheckpoint, PiNativeCorePage, PiNativeCoreUnit, PiNativeEventRow,
    PiNativeFileTouchRow, PiNativeOpenOutcome, PiNativeOwnedPage, PiNativeProfile, PiNativeResume,
    PiNativeScanOptions, PiNativeSessionRow, PiSourceLifecycle,
};
use crate::provider::providers::pi::PI_SOURCE_FORMAT;

mod lifecycle;
mod publication;

use lifecycle::*;
use publication::*;

#[cfg(test)]
pub(super) use lifecycle::released_cursor_for_test;
pub(super) use lifecycle::{map_native_error, source_cursor_stream};

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

struct PiRootEntryState {
    entry: PiRootEntry,
    source_id: Option<Uuid>,
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
    let discovery = discover_paths(path)?;
    let paths = discovery.paths.clone();
    let root_missing = discovery.root_missing;
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
        let mut relocated_source_ids = BTreeSet::new();
        let mut changed_core_pages = 0_usize;
        for source_path in &paths {
            let opened = discovery
                .discovery
                .as_ref()
                .ok_or(CaptureError::SystemInvariant(
                    "Pi discovered source lost its root authority",
                ))?
                .opened(source_path)
                .map_err(map_native_error)?;
            let result = import_one_source(
                source_path,
                opened,
                store,
                &committed_store,
                &bulk_guard,
                &configured_root,
                &source_root,
                &options,
            )?;
            changed_core_pages = changed_core_pages.saturating_add(result.changed_core_pages);
            relocated_source_ids.extend(result.relocated_source_ids);
            summary.merge(result.summary);
            completed_paths.insert(source_path.clone());
            if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
                && changed_core_pages != 0
            {
                summary.work_remaining = true;
                return Ok(summary);
            }
        }

        let entries = completed_paths
            .iter()
            .map(|source_path| {
                root_entry_from_store(&committed_store, store, &options.machine_id, source_path)
            })
            .collect::<Result<Vec<_>>>()?;
        if let Some(opening) = discovery.discovery.as_ref() {
            let closing = opening.rediscover().map_err(map_native_error)?;
            if closing.sessions != paths {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
        }
        if let Some(prior) = &prior_manifest {
            let current = paths.iter().collect::<BTreeSet<_>>();
            for entry in &prior.entries {
                if current.contains(&entry.path) {
                    continue;
                }
                if root_entry_was_superseded(
                    store,
                    &options.machine_id,
                    entry,
                    &entries,
                    &relocated_source_ids,
                )? {
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

        summary.merge(publish_root_manifest(
            store,
            &bulk_guard,
            &options,
            &configured_root,
            &source_root,
            entries.into_iter().map(|state| state.entry).collect(),
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
    relocated_source_ids: BTreeSet<Uuid>,
}

#[allow(clippy::too_many_arguments)]
fn import_one_source(
    path: &Path,
    opened: Arc<crate::common::io::OpenedProviderSourceFile>,
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
        open_pi_native_session_retained(path, opened, scan_options).map_err(map_native_error)?
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
    let mut relocated_source_ids = BTreeSet::new();
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
                let publication = publish_core_page(
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
                if publication.summary.work_result() == ProviderImportWorkResult::Changed {
                    changed_core_pages = changed_core_pages.saturating_add(1);
                }
                if let Some(source_id) = publication.relocated_source_id {
                    relocated_source_ids.insert(source_id);
                }
                current_core = load_core_state(store, &options.machine_id, &cursor_stream)?
                    .prior
                    .map(|prior| prior.checkpoint);
                summary.merge(publication.summary);
                if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
                    && changed_core_pages != 0
                {
                    summary.work_remaining = true;
                    return Ok(SourceImportResult {
                        summary,
                        changed_core_pages,
                        relocated_source_ids,
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
        relocated_source_ids,
    })
}
