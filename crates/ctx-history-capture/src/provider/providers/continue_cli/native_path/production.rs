use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    Confidence, Event, EventRole, EventType, Fidelity, FileTouched, Session, SessionStatus,
    SyncCursor,
};
use ctx_history_store::{
    decode_native_path_committed_cursor, EventSearchBulkGuard, NativePathCursorSetClassification,
    NativePathCursorTransition, NativePathGroupAccounting, ProviderEventHashAuthority,
    ProviderSourceLocatorObservation, ProviderSourceRouteRetirement,
    ProviderSourceRouteRetirementDisposition, ProviderSourceRouteRetirementReason, Store,
    NATIVE_PATH_MAX_MUTATION_UNITS,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    provider::{
        importer::{
            provider_event_import_identity_with_exact_legacy_source, provider_file_touch_import_id,
            provider_import_session_uuid, provider_path_identity,
            provider_scoped_source_identity_key, provider_scoped_source_uuid,
            provider_source_cursor_stream_for_path, provider_source_identity,
            provider_sync_metadata, timestamps, CertifiedProviderCursor,
            ProviderEventImportIdentity,
        },
        native_ingestion::{
            process_pro_replay_only, NativeIngestionPage, NativePublicationPage,
            NativeSafeFrontier, NativeSourceIdentity,
        },
    },
    stable_capture_uuid, CaptureError, CaptureWorkLimit, ImportProfile, ProviderAdapterContext,
    ProviderImportFailure, ProviderImportOptions, ProviderImportSummary, ProviderImportWorkResult,
    Result, CONTINUE_CLI_SOURCE_FORMAT,
};

use super::normalize::CONTINUE_NATIVE_MAX_FILE_TOUCHES_PER_EVENT;
#[cfg(test)]
use super::{clear_continue_io_failure, inject_continue_io_failure, ContinueInjectedIoOperation};
use super::{
    discover_continue_root, prepare_continue_discovery_with_profile, ContinueEventKind,
    ContinueEventRole, ContinueEventRow, ContinueIndexObservation, ContinueIndexSnapshot,
    ContinueNativePageAdapter, ContinueNativePathError, ContinueNativeProfile,
    ContinueNativeStoreCursor, ContinuePageFrontier, ContinuePreparedSource, ContinueSessionRow,
    ContinueSourceObservation, ContinueSourceOutcome,
};

mod lifecycle;
mod publication;

#[cfg(test)]
mod tests;

use lifecycle::*;
use publication::*;

const CONTINUE_PAGE_PUBLICATION_DOMAIN: &[u8] = b"ctx-continue-nativepath-core-publication-v1\0";
const CONTINUE_TERMINAL_PUBLICATION_DOMAIN: &[u8] =
    b"ctx-continue-nativepath-terminal-reconciliation-v1\0";
const CONTINUE_RETIREMENT_PUBLICATION_DOMAIN: &[u8] = b"ctx-continue-nativepath-retirement-v1\0";
const CONTINUE_RETIRED_FILE_TOUCH_PATH: &str = "__ctx_retired_continue_file_touch__";
// resolve_source performs four mutations and publishing the cursor performs
// one. Event reconciliation and touch upserts are accounted below.
const CONTINUE_CORE_PAGE_FIXED_MUTATION_UNITS: usize = 5;

#[derive(Clone)]
struct ContinuePublicationSource {
    observation: ContinueSourceObservation,
    index_dependency: ContinueIndexObservation,
    session: ContinueSessionRow,
}

impl From<&ContinuePreparedSource> for ContinuePublicationSource {
    fn from(source: &ContinuePreparedSource) -> Self {
        Self {
            observation: source.observation.clone(),
            index_dependency: source.index_dependency.clone(),
            session: source.session.clone(),
        }
    }
}

struct ResolvedContinueSource {
    source_id: Uuid,
    session: Session,
}

struct ContinueEventPublication<'event> {
    event: &'event ContinueEventRow,
    provider_event_index: u64,
    identity: ProviderEventImportIdentity,
    touch_ids: Vec<Uuid>,
}

#[derive(Clone)]
struct KnownContinueRoute {
    path: PathBuf,
    locator_identity: String,
    canonical_source_identity: String,
    source_revision: String,
    current_cursor: SyncCursor,
    provider_cursor: String,
    committed_publication_id: Option<String>,
}

enum CursorPlan {
    AlreadyCommitted,
    Publish {
        cursor: ContinueNativeStoreCursor,
        terminal_reconciliation: bool,
    },
}

pub(crate) fn import_continue_nativepath_history(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let configured_source_root = context
        .source_root
        .clone()
        .or(context.source_path.clone())
        .unwrap_or_else(|| path.to_path_buf());
    let known_routes = known_continue_routes(store, &context.machine_id, &configured_source_root)?;

    match fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if known_routes.is_empty() {
                return Err(CaptureError::InvalidProviderTranscriptPath {
                    path: path.to_path_buf(),
                    reason: "no Continue CLI session JSON files found",
                });
            }
            if options.import_profile.is_replay_only() {
                return Ok(ProviderImportSummary::default());
            }
            return retire_missing_routes(
                store,
                &context,
                &known_routes,
                &BTreeSet::new(),
                ProviderSourceRouteRetirementReason::RootMissing,
                options.capture_work_limit,
            );
        }
        Err(error) => return Err(error.into()),
    }

    let discovery = discover_continue_root(path).map_err(map_native_error)?;
    let live_paths = discovery
        .paths()
        .map_err(map_native_error)?
        .collect::<std::result::Result<BTreeSet<_>, _>>()
        .map_err(map_native_error)?;
    if live_paths.is_empty() && known_routes.is_empty() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "no Continue CLI session JSON files found",
        });
    }

    let committed_store = Store::open_read_only(store.path())?;
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let native_profile = match &options.import_profile {
        ImportProfile::CoreOnly => ContinueNativeProfile::CoreOnly,
        ImportProfile::CoreAndPro(_) | ImportProfile::ProReplayOnly(_) => {
            ContinueNativeProfile::CoreAndPro
        }
    };
    let replay_only = options.import_profile.is_replay_only();
    let operation = (|| {
        let mut preparation = prepare_continue_discovery_with_profile(&discovery, native_profile)
            .map_err(map_native_error)?;
        let mut adapter = ContinueNativePageAdapter::new(&options.import_profile);
        let mut active_source: Option<ContinuePublicationSource> = None;
        let mut summary = ProviderImportSummary::default();
        let mut changed_groups = 0_usize;

        for outcome in preparation.by_ref() {
            match outcome.map_err(map_native_error)? {
                ContinueSourceOutcome::Page(page) => {
                    if let Some(source) = page.source.as_deref() {
                        active_source = Some(source.into());
                    }
                    let source = active_source.as_ref().ok_or(CaptureError::SystemInvariant(
                        "Continue NativePath page lost its source authority",
                    ))?;
                    let terminal = page.terminal;
                    let adapted = adapter
                        .adapt(*page)
                        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
                    if replay_only {
                        verify_core_page_committed(store, &context, source, adapted.core)?;
                    } else {
                        let core_summary = publish_core_page(
                            store,
                            &committed_store,
                            &bulk_guard,
                            &configured_source_root,
                            &context,
                            &options,
                            source,
                            discovery.index(),
                            adapted.core,
                        )?;
                        if core_summary.work_result() == ProviderImportWorkResult::Changed {
                            changed_groups = changed_groups.saturating_add(1);
                        }
                        summary.merge_from(core_summary);
                    }

                    if let Some(output) = adapted.output {
                        let sink =
                            options
                                .import_profile
                                .sink()
                                .ok_or(CaptureError::SystemInvariant(
                                    "Continue output page has no configured Pro sink",
                                ))?;
                        let _ = process_pro_replay_only(output, sink.as_ref());
                    }

                    if terminal {
                        active_source = None;
                    }
                    if !replay_only
                        && options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
                        && changed_groups != 0
                    {
                        summary.work_remaining = true;
                        return Ok(summary);
                    }
                }
                ContinueSourceOutcome::Incomplete(incomplete) => {
                    summary.record_failure(ProviderImportFailure {
                        line: 0,
                        error: format!(
                            "incomplete Continue session JSON: {}",
                            incomplete.observation.requested_path().display()
                        ),
                    });
                }
                ContinueSourceOutcome::Failed(failure) => {
                    summary.record_failure(ProviderImportFailure {
                        line: 0,
                        error: format!("{}: {}", failure.path.display(), failure.message),
                    });
                }
            }
        }

        if replay_only {
            return Ok(summary);
        }
        if !preparation
            .root_authority()
            .revalidate()
            .map_err(map_native_error)?
            .authoritative
        {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        summary.merge_from(retire_missing_routes_in_bulk(
            store,
            &bulk_guard,
            &context,
            &known_routes,
            &live_paths,
            ProviderSourceRouteRetirementReason::SourceMissing,
            options.capture_work_limit,
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
