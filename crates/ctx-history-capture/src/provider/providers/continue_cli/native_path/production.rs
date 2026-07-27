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
use super::{
    discover_continue_root, prepare_continue_discovery_with_profile, ContinueEventKind,
    ContinueEventRole, ContinueEventRow, ContinueIndexObservation, ContinueIndexSnapshot,
    ContinueNativePageAdapter, ContinueNativePathError, ContinueNativeProfile,
    ContinueNativeStoreCursor, ContinuePageFrontier, ContinuePreparedSource, ContinueSessionRow,
    ContinueSourceObservation, ContinueSourceOutcome,
};

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

// Publication needs the certified source, cursor, Store, and import policy
// authorities together; grouping them would obscure their ownership.
#[allow(clippy::too_many_arguments)]
fn publish_core_page(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    configured_source_root: &Path,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    source: &ContinuePublicationSource,
    index: &ContinueIndexSnapshot,
    publication_page: NativePublicationPage<super::ContinuePreparedPage>,
) -> Result<ProviderImportSummary> {
    if !index.revalidate() || !source.observation.revalidate().map_err(map_native_error)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let (source_identity, page) = publication_page.into_parts();
    validate_page_source_identity(&source_identity, source)?;
    let stream = source_cursor_stream(&source.observation)?;
    let stored = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let plan = classify_cursor(stored.as_ref(), &source_identity, source, &page)?;
    let CursorPlan::Publish {
        cursor,
        terminal_reconciliation,
    } = plan
    else {
        return Ok(already_committed_summary(&page));
    };

    let transition = NativePathCursorTransition::new(
        stored.as_ref().map(|cursor| cursor.cursor.clone()),
        provider_sync_cursor(
            &context.machine_id,
            stream,
            cursor.encode()?,
            context.imported_at,
        ),
    );
    let publication_id = if terminal_reconciliation {
        terminal_publication_id(&source_identity, &transition)
    } else {
        page_publication_id(&source_identity, &page, &transition)
    };
    if terminal_reconciliation
        && stored
            .as_ref()
            .map(|cursor| decode_native_path_committed_cursor(&cursor.cursor))
            .transpose()?
            .is_some_and(|committed| committed.publication_id() == publication_id)
    {
        return Ok(already_committed_summary(&page));
    }
    let accounting =
        NativePathGroupAccounting::new(1, 1, page.accounting.conservative_serialized_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    match group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))? {
        NativePathCursorSetClassification::AllNextSameGroup { .. } => {
            group.commit()?;
            return Ok(already_committed_summary(&page));
        }
        NativePathCursorSetClassification::AllExpected => {}
    }

    let mut summary = ProviderImportSummary::default();
    let resolved = resolve_source(
        committed_store,
        &mut group,
        configured_source_root,
        context,
        options,
        source,
        &mut summary,
    )?;
    publish_events(
        committed_store,
        &mut group,
        options,
        &resolved,
        &page.core.events,
        &mut summary,
    )?;
    if let Some(authority) = page.core.authority.as_ref() {
        for _ in 0..authority.rejected_items {
            summary.record_failure(ProviderImportFailure {
                line: 0,
                error: "Continue history item was rejected during bounded native parsing"
                    .to_owned(),
            });
        }
    }

    if !index.revalidate() || !source.observation.revalidate().map_err(map_native_error)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

fn classify_cursor(
    stored: Option<&SyncCursor>,
    source_identity: &NativeSourceIdentity,
    source: &ContinuePublicationSource,
    page: &NativeIngestionPage<super::ContinuePreparedPage>,
) -> Result<CursorPlan> {
    let expected = decode_frontier(&page.expected_frontier)?;
    let next = decode_frontier(&page.next_safe_frontier)?;
    let revision = source_revision(source);
    let rejected_records = page
        .core
        .authority
        .as_ref()
        .and_then(|authority| u64::try_from(authority.rejected_items).ok());

    let Some(stored) = stored else {
        ensure_initial_frontier(&expected)?;
        return Ok(CursorPlan::Publish {
            cursor: ContinueNativeStoreCursor {
                version: ContinueNativeStoreCursor::VERSION,
                source_identity: source_identity.source_identity().to_owned(),
                source_revision: revision,
                frontier: next,
                terminal: page.terminal,
                generation: 0,
                rejected_records: rejected_records.unwrap_or(0),
            },
            terminal_reconciliation: page.terminal,
        });
    };

    let provider_cursor = decode_native_path_committed_cursor(&stored.cursor)
        .map(|cursor| cursor.provider_cursor().to_owned())
        .unwrap_or_else(|_| stored.cursor.clone());
    let prior = match ContinueNativeStoreCursor::decode(&provider_cursor) {
        Ok(prior) => Some(prior),
        Err(_) => {
            if CertifiedProviderCursor::decode_if_certified(&provider_cursor)?.is_none() {
                return Err(CaptureError::InvalidPayload(
                    "Continue NativePath cursor is neither current nor a released migration cursor"
                        .to_owned(),
                ));
            }
            None
        }
    };
    let Some(prior) = prior else {
        ensure_initial_frontier(&expected)?;
        return Ok(CursorPlan::Publish {
            cursor: ContinueNativeStoreCursor {
                version: ContinueNativeStoreCursor::VERSION,
                source_identity: source_identity.source_identity().to_owned(),
                source_revision: revision,
                frontier: next,
                terminal: page.terminal,
                generation: 0,
                rejected_records: rejected_records.unwrap_or(0),
            },
            terminal_reconciliation: page.terminal,
        });
    };
    if prior.version != ContinueNativeStoreCursor::VERSION {
        return Err(CaptureError::InvalidPayload(
            "unsupported Continue NativePath cursor version".to_owned(),
        ));
    }

    if prior.source_identity == source_identity.source_identity()
        && prior.source_revision == revision
    {
        if prior.frontier == next || prior.frontier.next_page_ordinal > next.next_page_ordinal {
            if page.core.source.is_some() && prior.terminal {
                return Ok(CursorPlan::Publish {
                    cursor: prior,
                    terminal_reconciliation: true,
                });
            }
            return Ok(CursorPlan::AlreadyCommitted);
        }
        if prior.frontier.next_page_ordinal == next.next_page_ordinal {
            return Err(CaptureError::InvalidPayload(
                "Continue NativePath cursor conflicts at the same page frontier".to_owned(),
            ));
        }
        if prior.frontier != expected {
            return Err(CaptureError::InvalidPayload(
                "Continue NativePath cursor is discontinuous".to_owned(),
            ));
        }
        return Ok(CursorPlan::Publish {
            cursor: ContinueNativeStoreCursor {
                version: ContinueNativeStoreCursor::VERSION,
                source_identity: source_identity.source_identity().to_owned(),
                source_revision: revision,
                frontier: next,
                terminal: page.terminal,
                generation: prior.generation,
                rejected_records: rejected_records.unwrap_or(prior.rejected_records),
            },
            terminal_reconciliation: page.terminal,
        });
    }

    ensure_initial_frontier(&expected)?;
    let generation = prior
        .generation
        .checked_add(1)
        .ok_or(CaptureError::SystemInvariant(
            "Continue NativePath generation is exhausted",
        ))?;
    Ok(CursorPlan::Publish {
        cursor: ContinueNativeStoreCursor {
            version: ContinueNativeStoreCursor::VERSION,
            source_identity: source_identity.source_identity().to_owned(),
            source_revision: revision,
            frontier: next,
            terminal: page.terminal,
            generation,
            rejected_records: rejected_records.unwrap_or(0),
        },
        terminal_reconciliation: page.terminal,
    })
}

fn ensure_initial_frontier(frontier: &ContinuePageFrontier) -> Result<()> {
    if frontier.next_page_ordinal != 0 || frontier.next_history_ordinal != 0 {
        return Err(CaptureError::InvalidPayload(
            "Continue NativePath reset did not begin at the initial frontier".to_owned(),
        ));
    }
    Ok(())
}

fn verify_core_page_committed(
    store: &Store,
    context: &ProviderAdapterContext,
    source: &ContinuePublicationSource,
    publication_page: NativePublicationPage<super::ContinuePreparedPage>,
) -> Result<()> {
    let (source_identity, page) = publication_page.into_parts();
    validate_page_source_identity(&source_identity, source)?;
    if !source.observation.revalidate().map_err(map_native_error)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let stream = source_cursor_stream(&source.observation)?;
    let stored = store
        .get_sync_cursor(None, &context.machine_id, &stream)?
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "Continue output replay requires committed NativePath Core".to_owned(),
            )
        })?;
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    let prior = ContinueNativeStoreCursor::decode(committed.provider_cursor())
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    let next = decode_frontier(&page.next_safe_frontier)?;
    if prior.version != ContinueNativeStoreCursor::VERSION
        || prior.source_identity != source_identity.source_identity()
        || prior.source_revision != source_revision(source)
        || prior.frontier.next_page_ordinal < next.next_page_ordinal
        || (prior.frontier.next_page_ordinal == next.next_page_ordinal && prior.frontier != next)
    {
        return Err(CaptureError::InvalidPayload(
            "Continue output replay no longer matches committed Core authority".to_owned(),
        ));
    }
    Ok(())
}

fn validate_page_source_identity(
    identity: &NativeSourceIdentity,
    source: &ContinuePublicationSource,
) -> Result<()> {
    if identity.provider() != CaptureProvider::Continue.as_str()
        || identity.source_identity() != format!("continue-session:{}", source.session.identity.0)
    {
        return Err(CaptureError::InvalidPayload(
            "Continue NativePath page source identity mismatch".to_owned(),
        ));
    }
    Ok(())
}

fn resolve_source(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    configured_source_root: &Path,
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    source: &ContinuePublicationSource,
    summary: &mut ProviderImportSummary,
) -> Result<ResolvedContinueSource> {
    let raw_source_path = source.observation.canonical_path().display().to_string();
    let source_root = configured_source_root.display().to_string();
    let locator_identity = provider_path_identity(source.observation.canonical_path())?;
    let proposed_source_identity = provider_source_identity(
        CaptureProvider::Continue,
        CONTINUE_CLI_SOURCE_FORMAT,
        Some(&source_root),
        Some(&raw_source_path),
        Some(&source.session.identity.0),
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Continue NativePath source has no canonical identity",
    ))?;
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Continue,
        CONTINUE_CLI_SOURCE_FORMAT,
        &locator_identity,
    );
    let revision = source_revision(source);
    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::Continue,
            source_format: CONTINUE_CLI_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity,
            cursor_stream: stream,
            proposed_source_identity,
            raw_source_path: Some(raw_source_path.clone()),
            source_revision: revision.clone(),
            observed_at_ms: context.imported_at.timestamp_millis(),
        })?;
    let existing = committed_store.capture_source_by_canonical_identity_session(
        CaptureProvider::Continue,
        CONTINUE_CLI_SOURCE_FORMAT,
        &context.machine_id,
        &resolution.canonical_source_identity,
        &source.session.identity.0,
    )?;
    let source_id = existing
        .as_ref()
        .map(|source| source.id)
        .unwrap_or_else(|| {
            provider_scoped_source_uuid(
                CaptureProvider::Continue,
                &source.session.identity.0,
                CONTINUE_CLI_SOURCE_FORMAT,
                Some(&raw_source_path),
            )
        });
    group.upsert_capture_source(&continue_capture_source(
        context,
        source,
        source_id,
        &raw_source_path,
        &source_root,
        &resolution.canonical_source_identity,
        &revision,
    ))?;
    group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;

    let session_id = provider_import_session_uuid(
        committed_store,
        CaptureProvider::Continue,
        &source.session.identity.0,
        source_id,
        Some(&resolution.canonical_source_identity),
    )?;
    let session = continue_session(
        context,
        options,
        source,
        source_id,
        session_id,
        &resolution.canonical_source_identity,
    );
    let existed = committed_store.get_session(session.id).is_ok();
    group.upsert_session(&session)?;
    if existed {
        summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    } else {
        summary.imported_sessions = summary.imported_sessions.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    }
    Ok(ResolvedContinueSource { source_id, session })
}

fn continue_capture_source(
    context: &ProviderAdapterContext,
    source: &ContinuePublicationSource,
    source_id: Uuid,
    raw_source_path: &str,
    source_root: &str,
    canonical_source_identity: &str,
    revision: &str,
) -> CaptureSource {
    CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Continue,
            machine_id: context.machine_id.clone(),
            process_id: None,
            cwd: source.session.workspace_directory.clone(),
            raw_source_path: Some(raw_source_path.to_owned()),
            source_format: Some(CONTINUE_CLI_SOURCE_FORMAT.to_owned()),
            source_root: Some(source_root.to_owned()),
            source_identity: Some(canonical_source_identity.to_owned()),
            external_session_id: Some(source.session.identity.0.clone()),
        },
        started_at: source.session.started_at.unwrap_or(context.imported_at),
        ended_at: None,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": source.session.identity.0,
                "source_format": CONTINUE_CLI_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "source_identity": canonical_source_identity,
                "source_revision": revision,
                "source_identity_key": provider_scoped_source_identity_key(
                    CaptureProvider::Continue,
                    &source.session.identity.0,
                    CONTINUE_CLI_SOURCE_FORMAT,
                    Some(raw_source_path),
                ),
                "nativepath_publication": 1,
            }),
        ),
    }
}

fn continue_session(
    context: &ProviderAdapterContext,
    options: &ProviderImportOptions,
    source: &ContinuePublicationSource,
    source_id: Uuid,
    session_id: Uuid,
    canonical_source_identity: &str,
) -> Session {
    let metadata = serde_json::from_str::<Value>(&source.session.metadata_json)
        .unwrap_or_else(|_| Value::Object(Default::default()));
    Session {
        id: session_id,
        history_record_id: options.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Continue,
        external_session_id: Some(source.session.identity.0.clone()),
        external_agent_id: None,
        agent_type: AgentType::Primary,
        role_hint: Some("continue-cli".to_owned()),
        is_primary: true,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: source.session.started_at.unwrap_or(context.imported_at),
        ended_at: None,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": source.session.identity.0,
                "source_format": CONTINUE_CLI_SOURCE_FORMAT,
                "session_idempotency_key": format!(
                    "provider-session:{}:{}",
                    CaptureProvider::Continue.as_str(),
                    source.session.identity.0,
                ),
                "canonical_source_identity": canonical_source_identity,
                "metadata": metadata,
                "metadata_hash": source.session.metadata_hash,
            }),
        ),
    }
}

fn publish_events(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    options: &ProviderImportOptions,
    resolved: &ResolvedContinueSource,
    events: &[ContinueEventRow],
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let publications = prepare_event_publications(committed_store, resolved, events)?;
    for publication in publications {
        let event = publication.event;
        let provider_event_index = publication.provider_event_index;
        let identity = publication.identity;
        let dedupe_key = Store::provider_event_dedupe_key_with_payload_hash(
            &identity.dedupe_key,
            &event.content_hash,
        )
        .unwrap_or(identity.dedupe_key);
        let body = serde_json::from_str::<Value>(&event.body_json).map_err(|error| {
            CaptureError::InvalidPayload(format!(
                "Continue sanitized event body is invalid: {error}"
            ))
        })?;
        let occurred_at = event.occurred_at.unwrap_or(resolved.session.started_at);
        let normalized = Event {
            id: identity.id,
            seq: identity.seq,
            history_record_id: options.history_record_id,
            session_id: Some(resolved.session.id),
            run_id: None,
            event_type: match event.kind {
                ContinueEventKind::Message => EventType::Message,
                ContinueEventKind::ToolCall => EventType::ToolCall,
            },
            role: Some(match event.role {
                ContinueEventRole::User => EventRole::User,
                ContinueEventRole::Assistant => EventRole::Assistant,
                ContinueEventRole::System => EventRole::System,
                ContinueEventRole::Tool => EventRole::Tool,
                ContinueEventRole::Unknown => EventRole::Unknown,
            }),
            occurred_at,
            capture_source_id: Some(resolved.source_id),
            payload: json!({
                "provider": CaptureProvider::Continue.as_str(),
                "provider_session_id": resolved.session.external_session_id,
                "provider_event_index": provider_event_index,
                "provider_event_hash": event.content_hash,
                "native_item_id": event.native_item_id,
                "body": body,
                "preview": event.preview,
                "searchable_text": event.search_text,
                "calls": event.calls.iter().map(|call| json!({
                    "state_ordinal": call.state_ordinal,
                    "call_id": call.call_id,
                    "nested_call_id": call.nested_call_id,
                    "tool_name": call.tool_name,
                    "status": call.status,
                })).collect::<Vec<_>>(),
                "artifacts": [],
            }),
            payload_blob_id: None,
            dedupe_key: Some(dedupe_key),
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({
                    "provider_session_id": resolved.session.external_session_id,
                    "provider_event_index": provider_event_index,
                    "provider_event_hash": event.content_hash,
                    "provider_event_hash_authority": "normalized_payload_fallback",
                    "source_format": CONTINUE_CLI_SOURCE_FORMAT,
                    "source_trust": "provider_native",
                    "source_record_ordinal": event.identity.history_ordinal,
                    "source_record_subrecord_index": 0,
                    "native_item_id": event.native_item_id,
                }),
            ),
        };
        if group.reconcile_provider_event(
            &normalized,
            ProviderEventHashAuthority::NormalizedPayloadFallback,
        )? {
            summary.imported_events = summary.imported_events.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        } else {
            summary.skipped_events = summary.skipped_events.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        }
        summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);
        for (touch, id) in event
            .file_touches
            .iter()
            .zip(publication.touch_ids.iter().copied())
        {
            group.upsert_file_touched(&FileTouched {
                id,
                history_record_id: options.history_record_id,
                run_id: None,
                event_id: Some(normalized.id),
                vcs_workspace_id: None,
                path: touch.path.clone(),
                change_kind: touch.change_kind,
                old_path: touch.old_path.clone(),
                line_count_delta: None,
                confidence: touch.confidence,
                timestamps: timestamps(occurred_at),
                source_id: Some(resolved.source_id),
                sync: provider_sync_metadata(
                    Fidelity::Imported,
                    json!({
                        "provider": CaptureProvider::Continue.as_str(),
                        "provider_session_id": resolved.session.external_session_id,
                        "provider_event_index": provider_event_index,
                        "source_format": CONTINUE_CLI_SOURCE_FORMAT,
                        "metadata": touch.metadata,
                    }),
                ),
            })?;
        }
        for id in publication.touch_ids[event.file_touches.len()..]
            .iter()
            .copied()
        {
            let retired_at = resolved.session.timestamps.updated_at;
            let mut sync = provider_sync_metadata(
                Fidelity::Imported,
                json!({
                    "provider": CaptureProvider::Continue.as_str(),
                    "provider_session_id": resolved.session.external_session_id,
                    "provider_event_index": provider_event_index,
                    "source_format": CONTINUE_CLI_SOURCE_FORMAT,
                    "retired_by": "continue_file_touch_rewrite",
                }),
            );
            sync.deleted_at = Some(retired_at);
            group.upsert_file_touched(&FileTouched {
                id,
                history_record_id: options.history_record_id,
                run_id: None,
                event_id: Some(normalized.id),
                vcs_workspace_id: None,
                path: CONTINUE_RETIRED_FILE_TOUCH_PATH.to_owned(),
                change_kind: None,
                old_path: None,
                line_count_delta: None,
                confidence: Confidence::Unknown,
                timestamps: timestamps(retired_at),
                source_id: Some(resolved.source_id),
                sync,
            })?;
        }
    }
    Ok(())
}

fn prepare_event_publications<'event>(
    committed_store: &Store,
    resolved: &ResolvedContinueSource,
    events: &'event [ContinueEventRow],
) -> Result<Vec<ContinueEventPublication<'event>>> {
    let provider_session_id = resolved
        .session
        .external_session_id
        .as_deref()
        .unwrap_or_default();
    let allow_legacy_provider_identity = resolved.session.id
        == crate::provider::importer::provider_session_uuid(
            CaptureProvider::Continue,
            provider_session_id,
        );
    let mut mutation_units = CONTINUE_CORE_PAGE_FIXED_MUTATION_UNITS;
    let mut publications = Vec::with_capacity(events.len());
    for event in events {
        let provider_event_index =
            event
                .identity
                .history_ordinal
                .checked_add(1)
                .ok_or(CaptureError::SystemInvariant(
                    "Continue event index is exhausted",
                ))?;
        let identity = provider_event_import_identity_with_exact_legacy_source(
            committed_store,
            CaptureProvider::Continue,
            provider_session_id,
            resolved.source_id,
            provider_event_index,
            provider_event_index,
            &event.content_hash,
            None,
            Some(provider_event_index),
            allow_legacy_provider_identity,
        )?;
        let mut touch_ids = Vec::new();
        for touch_index in 0..=CONTINUE_NATIVE_MAX_FILE_TOUCHES_PER_EVENT {
            let id = continue_file_touch_id(
                committed_store,
                resolved,
                provider_event_index,
                touch_index,
            )?;
            if !committed_store.file_touched_exists(id)? {
                break;
            }
            if touch_index == CONTINUE_NATIVE_MAX_FILE_TOUCHES_PER_EVENT {
                return Err(CaptureError::InvalidPayload(format!(
                    "stored Continue event {provider_event_index} exceeds the {} file-touch \
                     transaction bound",
                    CONTINUE_NATIVE_MAX_FILE_TOUCHES_PER_EVENT
                )));
            }
            touch_ids.push(id);
        }
        while touch_ids.len() < event.file_touches.len() {
            touch_ids.push(continue_file_touch_id(
                committed_store,
                resolved,
                provider_event_index,
                touch_ids.len(),
            )?);
        }
        mutation_units = mutation_units
            .checked_add(1_usize.saturating_add(touch_ids.len()))
            .ok_or(CaptureError::SystemInvariant(
                "Continue publication mutation accounting overflowed",
            ))?;
        publications.push(ContinueEventPublication {
            event,
            provider_event_index,
            identity,
            touch_ids,
        });
    }
    if mutation_units > NATIVE_PATH_MAX_MUTATION_UNITS {
        return Err(CaptureError::InvalidPayload(format!(
            "Continue page requires {mutation_units} Store mutation units, exceeding the \
             {NATIVE_PATH_MAX_MUTATION_UNITS} unit transaction bound"
        )));
    }
    Ok(publications)
}

fn continue_file_touch_id(
    committed_store: &Store,
    resolved: &ResolvedContinueSource,
    provider_event_index: u64,
    touch_index: usize,
) -> Result<Uuid> {
    let packed_touch_index = provider_event_index
        .checked_mul(u64::from(u16::MAX) + 1)
        .and_then(|base| base.checked_add(u64::try_from(touch_index).ok()?))
        .ok_or(CaptureError::SystemInvariant(
            "Continue file-touch identity overflowed",
        ))?;
    let provider_session_id = resolved
        .session
        .external_session_id
        .as_deref()
        .unwrap_or_default();
    provider_file_touch_import_id(
        committed_store,
        CaptureProvider::Continue,
        provider_session_id,
        resolved.source_id,
        Some(provider_event_index),
        packed_touch_index,
        resolved.session.id
            == crate::provider::importer::provider_session_uuid(
                CaptureProvider::Continue,
                provider_session_id,
            ),
    )
}

fn already_committed_summary(
    page: &NativeIngestionPage<super::ContinuePreparedPage>,
) -> ProviderImportSummary {
    let mut summary = ProviderImportSummary::default();
    summary.skipped_events = page.core.events.len();
    summary.skipped = summary.skipped.saturating_add(summary.skipped_events);
    if page.core.source.is_some() {
        summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    }
    summary.accepted_content_records = summary
        .accepted_content_records
        .saturating_add(page.core.events.len());
    summary.set_work_result(ProviderImportWorkResult::NoOp);
    summary
}

fn known_continue_routes(
    store: &Store,
    machine_id: &str,
    configured_source_root: &Path,
) -> Result<Vec<KnownContinueRoute>> {
    let source_root = configured_source_root.display().to_string();
    let mut routes = BTreeMap::<String, KnownContinueRoute>::new();
    for source in store.list_capture_sources()? {
        if source.descriptor.provider != CaptureProvider::Continue
            || source.descriptor.machine_id != machine_id
            || source.descriptor.source_format.as_deref() != Some(CONTINUE_CLI_SOURCE_FORMAT)
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
        let mut path = PathBuf::from(raw_source_path);
        if let Ok(canonical) = fs::canonicalize(&path) {
            path = canonical;
        }
        let locator_identity = provider_path_identity(&path)?;
        let stream = provider_source_cursor_stream_for_path(
            CaptureProvider::Continue,
            CONTINUE_CLI_SOURCE_FORMAT,
            &locator_identity,
        );
        let Some(current_cursor) = store.get_sync_cursor(None, machine_id, &stream)? else {
            continue;
        };
        let provider_cursor = decode_native_path_committed_cursor(&current_cursor.cursor)
            .map(|cursor| cursor.provider_cursor().to_owned())
            .unwrap_or_else(|_| current_cursor.cursor.clone());
        let cursor_revision = ContinueNativeStoreCursor::decode(&provider_cursor)
            .ok()
            .map(|cursor| cursor.source_revision)
            .or_else(|| {
                CertifiedProviderCursor::decode_if_certified(&provider_cursor)
                    .ok()
                    .flatten()
                    .map(|cursor| cursor.source_revision().to_owned())
            });
        let Some(source_revision) = source
            .sync
            .metadata
            .get("source_revision")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or(cursor_revision)
        else {
            continue;
        };
        let route = KnownContinueRoute {
            path,
            locator_identity: locator_identity.clone(),
            canonical_source_identity: canonical_source_identity.to_owned(),
            source_revision,
            current_cursor,
            provider_cursor,
        };
        if let Some(existing) = routes.get(&locator_identity) {
            if existing.canonical_source_identity != route.canonical_source_identity
                || existing.source_revision != route.source_revision
            {
                return Err(CaptureError::SystemInvariant(
                    "Continue persisted conflicting routes for one locator",
                ));
            }
            continue;
        }
        routes.insert(locator_identity, route);
    }
    Ok(routes.into_values().collect())
}

fn retire_missing_routes(
    store: &mut Store,
    context: &ProviderAdapterContext,
    known_routes: &[KnownContinueRoute],
    live_paths: &BTreeSet<PathBuf>,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<ProviderImportSummary> {
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = retire_missing_routes_in_bulk(
        store,
        &bulk_guard,
        context,
        known_routes,
        live_paths,
        reason,
        CaptureWorkLimit::Drain,
    );
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    match (operation, finish) {
        (Ok(summary), Ok(())) => Ok(summary),
        (_, Err(error)) => Err(error),
        (Err(error), Ok(())) => Err(error),
    }
}

fn retire_missing_routes_in_bulk(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    known_routes: &[KnownContinueRoute],
    live_paths: &BTreeSet<PathBuf>,
    reason: ProviderSourceRouteRetirementReason,
    work_limit: CaptureWorkLimit,
) -> Result<ProviderImportSummary> {
    let mut summary = ProviderImportSummary::default();
    for route in known_routes
        .iter()
        .filter(|route| !live_paths.contains(&route.path))
    {
        if retire_route(store, bulk_guard, context, route, reason)? {
            summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
            summary.set_work_result(ProviderImportWorkResult::Changed);
            if work_limit == CaptureWorkLimit::OneSafeGroup {
                summary.work_remaining = true;
                break;
            }
        }
    }
    Ok(summary)
}

fn retire_route(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ProviderAdapterContext,
    route: &KnownContinueRoute,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<bool> {
    let stream = route.current_cursor.stream.clone();
    let transition = NativePathCursorTransition::new(
        Some(route.current_cursor.cursor.clone()),
        provider_sync_cursor(
            &context.machine_id,
            stream.clone(),
            route.provider_cursor.clone(),
            context.imported_at,
        ),
    );
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::Continue,
        source_format: CONTINUE_CLI_SOURCE_FORMAT.to_owned(),
        machine_id: context.machine_id.clone(),
        locator_identity: route.locator_identity.clone(),
        cursor_stream: stream,
        expected_canonical_source_identity: route.canonical_source_identity.clone(),
        expected_source_revision: route.source_revision.clone(),
        retired_at_ms: context.imported_at.timestamp_millis(),
        reason,
    };
    let publication_id = retirement_publication_id(&retirement);
    if decode_native_path_committed_cursor(&route.current_cursor.cursor)?.publication_id()
        == publication_id
    {
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

fn source_cursor_stream(source: &ContinueSourceObservation) -> Result<String> {
    let identity = provider_path_identity(source.canonical_path())?;
    Ok(provider_source_cursor_stream_for_path(
        CaptureProvider::Continue,
        CONTINUE_CLI_SOURCE_FORMAT,
        &identity,
    ))
}

fn source_revision(source: &ContinuePublicationSource) -> String {
    format!(
        "{};index={}",
        source.observation.session_revision(),
        source.index_dependency.dependency_revision()
    )
}

fn decode_frontier(frontier: &NativeSafeFrontier) -> Result<ContinuePageFrontier> {
    if frontier.version != 1 {
        return Err(CaptureError::InvalidPayload(
            "unsupported Continue NativePath frontier version".to_owned(),
        ));
    }
    serde_json::from_slice(&frontier.bytes)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
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
                CaptureProvider::Continue.as_str(),
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

fn page_publication_id(
    source: &NativeSourceIdentity,
    page: &NativeIngestionPage<super::ContinuePreparedPage>,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(CONTINUE_PAGE_PUBLICATION_DOMAIN);
    hash_publication_common(&mut digest, source, transition);
    digest.update(page.expected_frontier.version.to_le_bytes());
    hash_field(&mut digest, &page.expected_frontier.bytes);
    digest.update(page.next_safe_frontier.version.to_le_bytes());
    hash_field(&mut digest, &page.next_safe_frontier.bytes);
    digest.update([u8::from(page.terminal)]);
    format!("continue-nativepath-page:{}", hex(&digest.finalize()))
}

fn terminal_publication_id(
    source: &NativeSourceIdentity,
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(CONTINUE_TERMINAL_PUBLICATION_DOMAIN);
    hash_publication_common(&mut digest, source, transition);
    format!("continue-nativepath-terminal:{}", hex(&digest.finalize()))
}

fn hash_publication_common(
    digest: &mut Sha256,
    source: &NativeSourceIdentity,
    transition: &NativePathCursorTransition,
) {
    hash_field(digest, source.provider().as_bytes());
    hash_field(digest, source.source_identity().as_bytes());
    hash_field(digest, transition.next().stream.as_bytes());
    hash_field(digest, transition.next().cursor.as_bytes());
}

fn retirement_publication_id(retirement: &ProviderSourceRouteRetirement) -> String {
    let mut digest = Sha256::new();
    digest.update(CONTINUE_RETIREMENT_PUBLICATION_DOMAIN);
    hash_field(&mut digest, retirement.provider.as_str().as_bytes());
    hash_field(&mut digest, retirement.source_format.as_bytes());
    hash_field(&mut digest, retirement.machine_id.as_bytes());
    hash_field(&mut digest, retirement.locator_identity.as_bytes());
    hash_field(&mut digest, retirement.cursor_stream.as_bytes());
    hash_field(
        &mut digest,
        retirement.expected_canonical_source_identity.as_bytes(),
    );
    hash_field(&mut digest, retirement.expected_source_revision.as_bytes());
    format!("continue-nativepath-retire:{}", hex(&digest.finalize()))
}

fn hash_field(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_le_bytes());
    digest.update(value);
}

fn hex(bytes: &[u8]) -> String {
    let mut value = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(value, "{byte:02x}");
    }
    value
}

fn map_native_error(error: ContinueNativePathError) -> CaptureError {
    match error {
        ContinueNativePathError::SourceChanged { .. } => CaptureError::SourceChangedDuringCapture,
        other => CaptureError::InvalidPayload(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc, Mutex,
        },
    };

    use ctx_history_store::{RawSqlOptions, RawSqlValue};
    use serde_json::json;

    use crate::{
        test_support_paths::tempdir, ImportProfile, OutputSourceIdentity,
        ProOutputMaterializationPage, ProOutputPageResult, ProOutputProgress, ProOutputSink,
        ProOutputSinkError, ProviderImportWorkResult,
    };

    use super::*;

    fn write_session(path: &Path, session_id: &str, messages: &[&str]) {
        let history = messages
            .iter()
            .enumerate()
            .map(|(ordinal, message)| {
                if ordinal == 0 {
                    json!({
                        "id": format!("item-{ordinal}"),
                        "timestamp": "2026-01-01T00:00:00Z",
                        "message": {"role": "assistant", "content": message},
                        "toolCallStates": [{
                            "toolCallId": "call-0",
                            "toolCall": {
                                "id": "call-0",
                                "type": "function",
                                "function": {
                                    "name": "shell",
                                    "arguments": "{\"command\":\"printf test\"}",
                                }
                            },
                            "status": "done",
                            "output": [{
                                "name": "Result",
                                "content": "SUCCESS-OUTPUT-MUST-STAY-OUT-OF-CORE",
                            }],
                        }],
                    })
                } else {
                    json!({
                        "id": format!("item-{ordinal}"),
                        "timestamp": "2026-01-01T00:00:01Z",
                        "message": {"role": "user", "content": message},
                    })
                }
            })
            .collect::<Vec<_>>();
        fs::write(
            path,
            serde_json::to_vec(&json!({
                "sessionId": session_id,
                "title": format!("Session {session_id}"),
                "createdAt": "2026-01-01T00:00:00Z",
                "history": history,
            }))
            .unwrap(),
        )
        .unwrap();
    }

    fn write_output_session(path: &Path) {
        fs::write(
            path,
            serde_json::to_vec(&json!({
                "sessionId": "stable",
                "title": "Output replay",
                "createdAt": "2026-01-01T00:00:00Z",
                "history": [
                    {
                        "id": "request",
                        "timestamp": "2026-01-01T00:00:00Z",
                        "message": {"role": "user", "content": "run it"},
                    },
                    {
                        "id": "tool",
                        "timestamp": "2026-01-01T00:00:01Z",
                        "message": {"role": "assistant", "content": ""},
                        "toolCallStates": [{
                            "toolCallId": "call-0",
                            "toolCall": {
                                "id": "call-0",
                                "type": "function",
                                "function": {
                                    "name": "shell",
                                    "arguments": "{\"command\":\"printf test\"}",
                                }
                            },
                            "status": "done",
                            "output": [{
                                "name": "Result",
                                "content": "SUCCESS-OUTPUT-MUST-STAY-OUT-OF-CORE",
                            }],
                        }],
                    },
                ],
            }))
            .unwrap(),
        )
        .unwrap();
    }

    fn write_touch_session(path: &Path, touch_paths: &[String]) {
        let mut patch = String::from("*** Begin Patch\n");
        for touch_path in touch_paths {
            patch.push_str(&format!("*** Update File: {touch_path}\n"));
        }
        patch.push_str("*** End Patch\n");
        fs::write(
            path,
            serde_json::to_vec(&json!({
                "sessionId": "touch-stable",
                "title": "Touch rewrite",
                "createdAt": "2026-01-01T00:00:00Z",
                "history": [{
                    "id": "touch-event",
                    "timestamp": "2026-01-01T00:00:00Z",
                    "message": {"role": "assistant", "content": ""},
                    "toolCallStates": [{
                        "toolCallId": "touch-call",
                        "toolCall": {
                            "id": "touch-call",
                            "type": "function",
                            "function": {
                                "name": "apply_patch",
                                "arguments": patch,
                            }
                        },
                        "status": "done",
                    }],
                }],
            }))
            .unwrap(),
        )
        .unwrap();
    }

    fn import(root: &Path, store: &mut Store) -> Result<ProviderImportSummary> {
        import_with_profile(root, store, ImportProfile::CoreOnly)
    }

    fn import_with_profile(
        root: &Path,
        store: &mut Store,
        import_profile: ImportProfile,
    ) -> Result<ProviderImportSummary> {
        import_continue_nativepath_history(
            root,
            store,
            ProviderAdapterContext {
                machine_id: "continue-nativepath-test".to_owned(),
                source_path: Some(root.to_path_buf()),
                source_root: None,
                imported_at: DateTime::<Utc>::from_timestamp(1_767_225_600, 0).unwrap(),
            },
            ProviderImportOptions {
                import_profile,
                ..Default::default()
            },
        )
    }

    fn events(store: &Store) -> Vec<Event> {
        store
            .list_sessions()
            .unwrap()
            .into_iter()
            .flat_map(|session| store.events_for_session(session.id).unwrap())
            .collect()
    }

    fn visible_touch_paths(store: &Store) -> Vec<String> {
        store
            .raw_sql_query(
                "SELECT path FROM ctx_files_touched ORDER BY path",
                RawSqlOptions::default(),
            )
            .unwrap()
            .rows
            .into_iter()
            .map(|row| match row.into_iter().next().unwrap() {
                RawSqlValue::Text { value, .. } => value,
                value => panic!("expected text file-touch path, got {value:?}"),
            })
            .collect()
    }

    #[derive(Default)]
    struct ReplaySink {
        fail: AtomicBool,
        behind: AtomicUsize,
        materialized_pages: AtomicUsize,
        materialized_outputs: AtomicUsize,
        behind_errors: Mutex<Vec<String>>,
        output_bodies: Mutex<Vec<Vec<u8>>>,
        progress: Mutex<Option<ProOutputProgress>>,
    }

    impl ProOutputSink for ReplaySink {
        fn inventory_generation(&self) -> u64 {
            1
        }

        fn materializer_revision(&self) -> &str {
            "continue-production-test-v1"
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
            self.materialized_pages.fetch_add(1, Ordering::SeqCst);
            self.materialized_outputs
                .fetch_add(page.observations.len(), Ordering::SeqCst);
            if self.fail.load(Ordering::SeqCst) {
                return Err(ProOutputSinkError::new(
                    "continue_test_sink_failure",
                    "intentional output failure",
                ));
            }
            self.output_bodies.lock().unwrap().extend(
                page.observations
                    .iter()
                    .map(|output| output.content.clone()),
            );
            *self.progress.lock().unwrap() = Some(ProOutputProgress {
                source_epoch: page.source_epoch,
                observed_revision: page.observed_revision.clone(),
                cursor: Some(page.next_safe_cursor.clone()),
                parser_revision: page.parser_revision.clone(),
                materializer_revision: page.materializer_revision.clone(),
                terminal: page.terminal,
            });
            Ok(ProOutputPageResult {
                source_epoch: page.source_epoch,
                committed_cursor: page.next_safe_cursor,
                accepted_outputs: u32::try_from(page.observations.len()).unwrap(),
                materialized_facts: u32::try_from(page.observations.len()).unwrap(),
                replayed: false,
            })
        }

        fn mark_behind(&self, error: ProOutputSinkError) {
            self.behind.fetch_add(1, Ordering::SeqCst);
            self.behind_errors.lock().unwrap().push(error.to_string());
        }
    }

    #[test]
    fn pro_failure_cannot_block_core_and_later_replay_recovers_output() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("continue");
        fs::create_dir(&root).unwrap();
        write_output_session(&root.join("session.json"));
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let sink = Arc::new(ReplaySink::default());
        sink.fail.store(true, Ordering::SeqCst);

        let core = import_with_profile(&root, &mut store, ImportProfile::CoreAndPro(sink.clone()))
            .unwrap();
        assert_eq!(core.work_result(), ProviderImportWorkResult::Changed);
        assert_eq!(events(&store).len(), 2);
        assert!(events(&store).iter().all(|event| {
            !event
                .payload
                .to_string()
                .contains("SUCCESS-OUTPUT-MUST-STAY-OUT-OF-CORE")
        }));
        assert_ne!(sink.behind.load(Ordering::SeqCst), 0);

        sink.fail.store(false, Ordering::SeqCst);
        import_with_profile(
            &root,
            &mut store,
            ImportProfile::ProReplayOnly(sink.clone()),
        )
        .unwrap();
        let output_bodies = sink
            .output_bodies
            .lock()
            .unwrap()
            .iter()
            .map(|body| String::from_utf8_lossy(body).into_owned())
            .collect::<Vec<_>>();
        assert!(
            output_bodies
                .iter()
                .any(|body| body.contains("SUCCESS-OUTPUT-MUST-STAY-OUT-OF-CORE")),
            "pages={}, outputs={}, behind={:?}, bodies={output_bodies:?}",
            sink.materialized_pages.load(Ordering::SeqCst),
            sink.materialized_outputs.load(Ordering::SeqCst),
            sink.behind_errors.lock().unwrap(),
        );
        assert_eq!(events(&store).len(), 2);
        let materialized_pages = sink.materialized_pages.load(Ordering::SeqCst);
        import_with_profile(
            &root,
            &mut store,
            ImportProfile::ProReplayOnly(sink.clone()),
        )
        .unwrap();
        assert_eq!(
            sink.materialized_pages.load(Ordering::SeqCst),
            materialized_pages
        );
    }

    #[test]
    fn production_core_lifecycle_is_idempotent_private_and_restorable() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("continue");
        fs::create_dir(&root).unwrap();
        let source = root.join("session.json");
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();

        write_session(&source, "stable", &["first"]);
        let fresh = import(&root, &mut store).unwrap();
        assert_eq!(fresh.work_result(), ProviderImportWorkResult::Changed);
        assert_eq!(store.list_sessions().unwrap().len(), 1);
        let first_events = events(&store);
        assert_eq!(first_events.len(), 1);
        assert!(!first_events[0]
            .payload
            .to_string()
            .contains("SUCCESS-OUTPUT-MUST-STAY-OUT-OF-CORE"));

        let no_op = import(&root, &mut store).unwrap();
        assert_eq!(no_op.work_result(), ProviderImportWorkResult::NoOp);

        write_session(&source, "stable", &["first", "appended"]);
        let append = import(&root, &mut store).unwrap();
        assert_eq!(append.work_result(), ProviderImportWorkResult::Changed);
        assert_eq!(events(&store).len(), 2);

        write_session(&source, "stable", &["rewritten", "appended"]);
        let rewrite = import(&root, &mut store).unwrap();
        assert_eq!(rewrite.work_result(), ProviderImportWorkResult::Changed);
        assert!(events(&store)
            .iter()
            .any(|event| event.payload.to_string().contains("rewritten")));

        write_session(&source, "stable", &["truncated"]);
        assert_eq!(
            import(&root, &mut store).unwrap().work_result(),
            ProviderImportWorkResult::Changed
        );
        write_session(&source, "stable", &["rewritten", "appended"]);
        assert_eq!(
            import(&root, &mut store).unwrap().work_result(),
            ProviderImportWorkResult::Changed
        );

        fs::write(&source, br#"{"sessionId":"stable","history":["#).unwrap();
        let incomplete = import(&root, &mut store).unwrap();
        assert_eq!(incomplete.failed, 1);
        assert!(store
            .authorized_source_route_for_event(first_events[0].id)
            .is_ok());

        write_session(&source, "stable", &["rewritten", "appended"]);
        assert_eq!(
            import(&root, &mut store).unwrap().work_result(),
            ProviderImportWorkResult::NoOp
        );
        fs::remove_file(&source).unwrap();
        assert_eq!(
            import(&root, &mut store).unwrap().work_result(),
            ProviderImportWorkResult::Changed
        );
        assert!(store
            .authorized_source_route_for_event(first_events[0].id)
            .is_err());
        assert_eq!(
            import(&root, &mut store).unwrap().work_result(),
            ProviderImportWorkResult::NoOp
        );

        write_session(&source, "stable", &["rewritten", "appended"]);
        assert_eq!(
            import(&root, &mut store).unwrap().work_result(),
            ProviderImportWorkResult::Changed
        );
        assert!(store
            .authorized_source_route_for_event(first_events[0].id)
            .is_ok());

        fs::remove_dir_all(&root).unwrap();
        assert_eq!(
            import(&root, &mut store).unwrap().work_result(),
            ProviderImportWorkResult::Changed
        );
        fs::create_dir(&root).unwrap();
        write_session(&source, "stable", &["rewritten", "appended"]);
        assert_eq!(
            import(&root, &mut store).unwrap().work_result(),
            ProviderImportWorkResult::Changed
        );
        assert!(store
            .authorized_source_route_for_event(first_events[0].id)
            .is_ok());
    }

    #[test]
    fn oversized_touch_event_is_rejected_before_store_publication() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("continue");
        fs::create_dir(&root).unwrap();
        let source = root.join("session.json");
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let oversized = (0..=CONTINUE_NATIVE_MAX_FILE_TOUCHES_PER_EVENT)
            .map(|index| format!("src/oversized-{index}.rs"))
            .collect::<Vec<_>>();

        write_touch_session(&source, &oversized);
        let rejected = import(&root, &mut store).unwrap();
        assert_eq!(rejected.failed, 1);
        assert!(store.list_sessions().unwrap().is_empty());
        assert!(events(&store).is_empty());
        assert!(visible_touch_paths(&store).is_empty());

        write_touch_session(&source, &["src/bounded.rs".to_owned()]);
        let bounded = import(&root, &mut store).unwrap();
        assert_eq!(bounded.work_result(), ProviderImportWorkResult::Changed);
        assert_eq!(visible_touch_paths(&store), ["src/bounded.rs"]);
    }

    #[test]
    fn touch_only_rewrite_retires_surplus_touch_without_stale_blame() {
        let temp = tempdir().unwrap();
        let root = temp.path().join("continue");
        fs::create_dir(&root).unwrap();
        let source = root.join("session.json");
        let mut store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let first_paths = ["src/one.rs".to_owned(), "src/two.rs".to_owned()];

        write_touch_session(&source, &first_paths);
        assert_eq!(
            import(&root, &mut store).unwrap().work_result(),
            ProviderImportWorkResult::Changed
        );
        let first_event = events(&store).pop().unwrap();
        let first_hash = first_event.payload["provider_event_hash"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(visible_touch_paths(&store), ["src/one.rs", "src/two.rs"]);
        assert!(!first_event.payload.to_string().contains("src/one.rs"));

        write_touch_session(&source, &["src/one.rs".to_owned()]);
        assert_eq!(
            import(&root, &mut store).unwrap().work_result(),
            ProviderImportWorkResult::Changed
        );
        let rewritten_event = events(&store).pop().unwrap();
        assert_eq!(rewritten_event.id, first_event.id);
        assert_ne!(
            rewritten_event.payload["provider_event_hash"]
                .as_str()
                .unwrap(),
            first_hash
        );
        assert_eq!(visible_touch_paths(&store), ["src/one.rs"]);
        assert!(store.file_touch_scope("src/two.rs").unwrap().is_empty());
        assert!(store
            .file_touch_scope("src/one.rs")
            .unwrap()
            .event_ids
            .contains(&rewritten_event.id));
        assert!(!rewritten_event.payload.to_string().contains("src/one.rs"));
    }
}
