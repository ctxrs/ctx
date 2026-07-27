use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    Confidence, Event, EventType, Fidelity, FileTouched, Session, SessionStatus, SyncCursor,
};
use ctx_history_store::{
    EventSearchBulkGuard, NativePathCursorSetClassification, NativePathCursorTransition,
    NativePathGroupAccounting, ProviderEventHashAuthority, ProviderSourceLocatorObservation,
    ProviderSourceRouteRetirement, ProviderSourceRouteRetirementDisposition,
    ProviderSourceRouteRetirementReason, Store,
};
use serde::{Deserialize, Serialize};
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
        },
        native_ingestion::{
            process_pro_replay_only, NativePageAccounting, NativeProOutputPage,
            NativeProReplayPage, NativeSafeFrontier, NativeSourceIdentity,
        },
        normalization::{
            provider_capped_json, provider_policy_body, provider_policy_event_text, provider_role,
            provider_timestamp_seconds,
        },
    },
    stable_capture_uuid, CaptureError, CaptureWorkLimit, OutputSourceIdentity, ProOutputProgress,
    ProOutputSink, ProOutputSinkError, ProOutputSourceDisposition, ProviderAdapterContext,
    ProviderImportFailure, ProviderImportOptions, ProviderImportSummary, ProviderImportWorkResult,
    Result, GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS,
    PROVIDER_MAX_TEXT_CHARS,
};

use super::{
    lifecycle::GooseNativePersistedState,
    native_path::{
        GooseNativePage, GooseNativePathReader, GooseNativeProFrontier, GooseNativeProfile,
        GooseNativeSourceSelection,
    },
    normalization::{goose_timestamp, GooseNativeEvent, GooseNativeEventKind, GooseNativeSession},
    position::{GooseNativeScanPhase, GooseNativeScanPosition},
    stream::GooseNativePageLimits,
};

const GOOSE_NATIVE_CURSOR_VERSION: u32 = 1;
const GOOSE_NATIVE_CURSOR_KIND: &str = "goose_nativepath";
const GOOSE_NATIVE_CURSOR_DOMAIN: &[u8] = b"ctx-goose-nativepath-publication-v1\0";
const GOOSE_SOURCE_REVISION_DOMAIN: &[u8] = b"ctx-goose-nativepath-source-revision-v1\0";
const GOOSE_INVENTORY_TOKEN_DOMAIN: &[u8] = b"ctx-goose-nativepath-inventory-token-v1\0";
const GOOSE_OUTPUT_FRONTIER_VERSION: u32 = 1;
const GOOSE_OUTPUT_PARSER_REVISION: &str = "goose-nativepath-output-v1";
const GOOSE_MAX_TOUCHES_PER_EVENT: usize = 32;
const GOOSE_PRODUCTION_PAGE_BYTES: u64 = 6 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct GooseNativeCursorWire {
    version: u32,
    kind: String,
    selected_path: PathBuf,
    source_revision: String,
    frontier: GooseNativeScanPosition,
    retained_events: u64,
    rejected_records: u64,
    terminal_state: Option<GooseNativePersistedState>,
}

#[derive(Clone)]
struct GooseKnownRoute {
    locator_identity: String,
    canonical_source_identity: String,
    source_revision: String,
    current_cursor: SyncCursor,
    wire: GooseNativeCursorWire,
}

struct GoosePublicationContext<'a> {
    machine_id: &'a str,
    source_root: &'a Path,
    imported_at: DateTime<Utc>,
    history_record_id: Option<Uuid>,
}

struct ResolvedSession {
    source_id: Uuid,
    session: Session,
}

pub(super) fn import_goose_nativepath(
    path: &Path,
    store: &mut Store,
    mut context: ProviderAdapterContext,
    options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    context.source_path = Some(path.to_path_buf());
    let source_root = context
        .source_root
        .clone()
        .or_else(|| context.source_path.clone())
        .unwrap_or_else(|| path.to_path_buf());
    let known_routes = known_goose_routes(store, &context.machine_id, path)?;
    let sink = options.import_profile.sink().cloned();

    if !path_exists(path)? {
        if options.import_profile.is_replay_only() {
            return Ok(ProviderImportSummary::default());
        }
        if known_routes.is_empty() {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "Goose sessions.db does not exist",
            });
        }
        return retire_missing_goose_route(
            store,
            &context,
            &known_routes[0],
            if source_root.exists() {
                ProviderSourceRouteRetirementReason::SourceMissing
            } else {
                ProviderSourceRouteRetirementReason::RootMissing
            },
        );
    }

    let canonical_path = std::fs::canonicalize(path)?;
    let inventory_token = options
        .inventory_observation_token
        .clone()
        .filter(|token| !token.trim().is_empty())
        .unwrap_or_else(|| goose_inventory_token(&canonical_path));
    let selection = GooseNativeSourceSelection::exact(&canonical_path)
        .with_inventory_observation_token(Some(inventory_token));
    let reader = GooseNativePathReader::acquire(selection)?;
    let source_revision = goose_source_revision(&reader);
    let profile = if sink.is_some() {
        GooseNativeProfile::CoreAndPro
    } else {
        GooseNativeProfile::CoreOnly
    };

    if options.import_profile.is_replay_only() {
        replay_outputs_or_mark_behind(
            &reader,
            &source_root,
            &source_revision,
            context.imported_at,
            sink.as_deref(),
        );
        return Ok(ProviderImportSummary::default());
    }

    let committed_store = Store::open_read_only(store.path())?;
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let publication_context = GoosePublicationContext {
        machine_id: &context.machine_id,
        source_root: &source_root,
        imported_at: context.imported_at,
        history_record_id: options.history_record_id,
    };
    let mut scanner = reader.scanner_with_profile(profile, goose_production_page_limits()?)?;
    let operation = (|| {
        let mut summary = ProviderImportSummary::default();
        let mut retained_events = 0_u64;
        let mut rejected_records = 0_u64;
        let mut emitted = false;
        let mut stopped = false;

        while let Some(page) = scanner.next_page()? {
            emitted = true;
            retained_events = retained_events.saturating_add(page.events.len() as u64);
            rejected_records = rejected_records.saturating_add(page.rejections.len() as u64);
            let terminal_state = if page.terminal {
                Some(GooseNativePersistedState::from_summary(
                    &scanner.finish_core()?,
                )?)
            } else {
                None
            };
            let outcome = publish_goose_page(
                store,
                &committed_store,
                &bulk_guard,
                &publication_context,
                &reader,
                &source_revision,
                &page,
                retained_events,
                rejected_records,
                terminal_state,
            )?;
            let changed = outcome.work_result() == ProviderImportWorkResult::Changed;
            summary.merge_from(outcome);
            if changed && options.capture_work_limit == CaptureWorkLimit::OneSafeGroup {
                summary.work_remaining = !page.terminal;
                stopped = true;
                break;
            }
        }

        if !stopped && !emitted {
            let scan_summary = scanner.finish_core()?;
            let terminal_state = GooseNativePersistedState::from_summary(&scan_summary)?;
            summary.merge_from(publish_goose_observation(
                store,
                &bulk_guard,
                &publication_context,
                &reader,
                &source_revision,
                terminal_state,
            )?);
        }
        Ok((summary, stopped))
    })();
    let finish = store
        .finish_event_search_bulk_mode(&bulk_guard)
        .map_err(CaptureError::from);
    let (summary, stopped) = match (operation, finish) {
        (Ok(result), Ok(())) => result,
        (_, Err(error)) => return Err(error),
        (Err(error), Ok(())) => return Err(error),
    };

    if !stopped {
        replay_outputs_or_mark_behind(
            &reader,
            &source_root,
            &source_revision,
            context.imported_at,
            sink.as_deref(),
        );
    }
    Ok(summary)
}

#[allow(clippy::too_many_arguments)]
fn publish_goose_page(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &GoosePublicationContext<'_>,
    reader: &GooseNativePathReader,
    source_revision: &str,
    page: &GooseNativePage,
    retained_events: u64,
    rejected_records: u64,
    terminal_state: Option<GooseNativePersistedState>,
) -> Result<ProviderImportSummary> {
    if !reader.revalidate_live()? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let raw_source_path = reader
        .source_observation()
        .source_path()
        .display()
        .to_string();
    let locator_identity = provider_path_identity(reader.source_observation().source_path())?;
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Goose,
        GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
        &locator_identity,
    );
    let stored = store.get_sync_cursor(None, context.machine_id, &stream)?;
    if goose_page_already_committed(stored.as_ref(), source_revision, page)? {
        let mut summary = skipped_page_summary(page);
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        return Ok(summary);
    }
    let wire = GooseNativeCursorWire {
        version: GOOSE_NATIVE_CURSOR_VERSION,
        kind: GOOSE_NATIVE_CURSOR_KIND.to_owned(),
        selected_path: reader.source_observation().source_path().to_path_buf(),
        source_revision: source_revision.to_owned(),
        frontier: page.next_frontier,
        retained_events,
        rejected_records,
        terminal_state,
    };
    validate_goose_cursor_predecessor(stored.as_ref(), source_revision, page.expected_frontier)?;
    let transition = NativePathCursorTransition::new(
        stored.as_ref().map(|cursor| cursor.cursor.clone()),
        provider_sync_cursor(
            context.machine_id,
            stream.clone(),
            encode_goose_cursor(&wire)?,
            context.imported_at,
        ),
    );
    let accounting =
        NativePathGroupAccounting::new(1, 1, page.accounting.conservative_serialized_bytes)?;
    let publication_id = goose_publication_id(page.identity.0, &transition);
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllNextSameGroup { .. }
    ) {
        group.commit()?;
        return Ok(skipped_page_summary(page));
    }

    let proposed_source_identity = provider_source_identity(
        CaptureProvider::Goose,
        GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
        Some(&context.source_root.display().to_string()),
        Some(&raw_source_path),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Goose NativePath source has no canonical identity",
    ))?;
    let resolution =
        group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
            provider: CaptureProvider::Goose,
            source_format: GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.to_owned(),
            locator_identity,
            cursor_stream: stream,
            proposed_source_identity,
            raw_source_path: Some(raw_source_path.clone()),
            source_revision: source_revision.to_owned(),
            observed_at_ms: context.imported_at.timestamp_millis(),
        })?;

    let mut summary = ProviderImportSummary::default();
    let mut resolved = BTreeMap::<String, ResolvedSession>::new();
    for native in &page.sessions {
        let value = resolve_goose_session(
            committed_store,
            context,
            native,
            &raw_source_path,
            &resolution.canonical_source_identity,
        )?;
        group.upsert_capture_source(&goose_capture_source(
            context,
            Some(native),
            value.source_id,
            &raw_source_path,
            &resolution.canonical_source_identity,
            source_revision,
        ))?;
        group.bind_capture_source_provider_route(value.source_id, &resolution.route_binding())?;
        let existed = committed_store.get_session(value.session.id).is_ok();
        group.upsert_session(&value.session)?;
        if existed {
            summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
            summary.skipped = summary.skipped.saturating_add(1);
        } else {
            summary.imported_sessions = summary.imported_sessions.saturating_add(1);
            summary.imported = summary.imported.saturating_add(1);
        }
        resolved.insert(native.native_identity.clone(), value);
    }
    for event in &page.events {
        let value = if let Some(value) = resolved.get(&event.session_identity) {
            value
        } else {
            let source = committed_store
                .capture_source_by_canonical_identity_session(
                    CaptureProvider::Goose,
                    GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
                    context.machine_id,
                    &resolution.canonical_source_identity,
                    &event.session_identity,
                )?
                .ok_or(CaptureError::SystemInvariant(
                    "Goose retained event has no committed native session source",
                ))?;
            let session_id = provider_import_session_uuid(
                committed_store,
                CaptureProvider::Goose,
                &event.session_identity,
                source.id,
                Some(&resolution.canonical_source_identity),
            )?;
            resolved
                .entry(event.session_identity.clone())
                .or_insert(ResolvedSession {
                    source_id: source.id,
                    session: committed_store.get_session(session_id)?,
                })
        };
        publish_goose_event(
            &mut group,
            committed_store,
            context,
            value.source_id,
            &value.session,
            event,
            reader.snapshot_connection(),
            &mut summary,
        )?;
    }
    if page.sessions.is_empty() && page.events.is_empty() {
        let source_id = goose_empty_source_id(&resolution.canonical_source_identity);
        group.upsert_capture_source(&goose_capture_source(
            context,
            None,
            source_id,
            &raw_source_path,
            &resolution.canonical_source_identity,
            source_revision,
        ))?;
        group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;
    }
    record_rejections(page, &mut summary);
    if !reader.revalidate_live()? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

fn publish_goose_observation(
    store: &mut Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &GoosePublicationContext<'_>,
    reader: &GooseNativePathReader,
    source_revision: &str,
    terminal_state: GooseNativePersistedState,
) -> Result<ProviderImportSummary> {
    if !reader.revalidate_live()? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let path = reader.source_observation().source_path();
    let raw_source_path = path.display().to_string();
    let locator_identity = provider_path_identity(path)?;
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Goose,
        GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
        &locator_identity,
    );
    let stored = store.get_sync_cursor(None, context.machine_id, &stream)?;
    if stored.as_ref().is_some_and(|cursor| {
        decode_goose_cursor(&cursor.cursor)
            .ok()
            .flatten()
            .is_some_and(|wire| {
                wire.source_revision == source_revision
                    && wire.frontier.phase == GooseNativeScanPhase::Complete
            })
    }) {
        return Ok(ProviderImportSummary::default());
    }
    let wire = GooseNativeCursorWire {
        version: GOOSE_NATIVE_CURSOR_VERSION,
        kind: GOOSE_NATIVE_CURSOR_KIND.to_owned(),
        selected_path: path.to_path_buf(),
        source_revision: source_revision.to_owned(),
        frontier: terminal_state.core_frontier,
        retained_events: 0,
        rejected_records: 0,
        terminal_state: Some(terminal_state),
    };
    let transition = NativePathCursorTransition::new(
        stored.as_ref().map(|cursor| cursor.cursor.clone()),
        provider_sync_cursor(
            context.machine_id,
            stream.clone(),
            encode_goose_cursor(&wire)?,
            context.imported_at,
        ),
    );
    let publication_id = goose_publication_id([0; 32], &transition);
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store
        .begin_native_path_publication_group(admission, NativePathGroupAccounting::new(0, 1, 0)?)?;
    if matches!(
        group.classify_cursor_set(&publication_id, std::slice::from_ref(&transition))?,
        NativePathCursorSetClassification::AllExpected
    ) {
        let proposed_source_identity = provider_source_identity(
            CaptureProvider::Goose,
            GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
            Some(&context.source_root.display().to_string()),
            Some(&raw_source_path),
            None,
            &Value::Null,
        )
        .ok_or(CaptureError::SystemInvariant(
            "Goose empty NativePath source has no canonical identity",
        ))?;
        let resolution =
            group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
                provider: CaptureProvider::Goose,
                source_format: GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT.to_owned(),
                machine_id: context.machine_id.to_owned(),
                locator_identity,
                cursor_stream: stream,
                proposed_source_identity,
                raw_source_path: Some(raw_source_path.clone()),
                source_revision: source_revision.to_owned(),
                observed_at_ms: context.imported_at.timestamp_millis(),
            })?;
        let source_id = goose_empty_source_id(&resolution.canonical_source_identity);
        group.upsert_capture_source(&goose_capture_source(
            context,
            None,
            source_id,
            &raw_source_path,
            &resolution.canonical_source_identity,
            source_revision,
        ))?;
        group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;
        group.prepare_journal_checkpoint()?;
        group.publish_cursor_set()?;
    }
    group.commit()?;
    let mut summary = ProviderImportSummary::default();
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

fn resolve_goose_session(
    committed_store: &Store,
    context: &GoosePublicationContext<'_>,
    native: &GooseNativeSession,
    raw_source_path: &str,
    source_identity: &str,
) -> Result<ResolvedSession> {
    let source_id = committed_store
        .capture_source_by_canonical_identity_session(
            CaptureProvider::Goose,
            GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
            context.machine_id,
            source_identity,
            &native.native_identity,
        )?
        .map(|source| source.id)
        .unwrap_or_else(|| {
            provider_scoped_source_uuid(
                CaptureProvider::Goose,
                &native.native_identity,
                GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
                Some(raw_source_path),
            )
        });
    let id = provider_import_session_uuid(
        committed_store,
        CaptureProvider::Goose,
        &native.native_identity,
        source_id,
        Some(source_identity),
    )?;
    let started_at = goose_timestamp(native.row.created_at.as_deref(), context.imported_at);
    let ended_at = native
        .row
        .updated_at
        .as_deref()
        .map(|value| goose_timestamp(Some(value), started_at));
    Ok(ResolvedSession {
        source_id,
        session: Session {
            id,
            history_record_id: context.history_record_id,
            parent_session_id: None,
            root_session_id: None,
            capture_source_id: Some(source_id),
            provider: CaptureProvider::Goose,
            external_session_id: Some(native.native_identity.clone()),
            external_agent_id: native.row.provider_name.clone(),
            agent_type: AgentType::Primary,
            role_hint: native
                .row
                .session_type
                .clone()
                .or_else(|| Some("primary".to_owned())),
            is_primary: true,
            status: SessionStatus::Imported,
            transcript_blob_id: None,
            started_at,
            ended_at,
            timestamps: timestamps(context.imported_at),
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({
                    "provider_session_id": native.native_identity,
                    "source_format": GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
                    "source_trust": "provider_native",
                    "native_rowid": native.sqlite_rowid,
                    "name": native.row.name,
                    "description": native.row.description,
                    "user_set_name": native.row.user_set_name,
                    "session_type": native.row.session_type,
                    "working_dir": native.row.working_dir,
                    "extension_data": native.row.extension_data,
                    "provider_name": native.row.provider_name,
                    "model_config": native.row.model_config_json,
                    "goose_mode": native.row.goose_mode,
                    "archived_at": native.row.archived_at,
                    "project_id": native.row.project_id,
                    "tokens": {
                        "total": native.row.total_tokens,
                        "input": native.row.input_tokens,
                        "output": native.row.output_tokens,
                        "accumulated_total": native.row.accumulated_total_tokens,
                        "accumulated_input": native.row.accumulated_input_tokens,
                        "accumulated_output": native.row.accumulated_output_tokens,
                    },
                    "accumulated_cost": native.row.accumulated_cost,
                }),
            ),
        },
    })
}

fn goose_capture_source(
    context: &GoosePublicationContext<'_>,
    native: Option<&GooseNativeSession>,
    source_id: Uuid,
    raw_source_path: &str,
    source_identity: &str,
    source_revision: &str,
) -> CaptureSource {
    let started_at = native.map_or(context.imported_at, |session| {
        goose_timestamp(session.row.created_at.as_deref(), context.imported_at)
    });
    let ended_at = native.and_then(|session| {
        session
            .row
            .updated_at
            .as_deref()
            .map(|value| goose_timestamp(Some(value), started_at))
    });
    CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Goose,
            machine_id: context.machine_id.to_owned(),
            process_id: None,
            cwd: native.and_then(|session| session.row.working_dir.clone()),
            raw_source_path: Some(raw_source_path.to_owned()),
            source_format: Some(GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT.to_owned()),
            source_root: Some(context.source_root.display().to_string()),
            source_identity: Some(source_identity.to_owned()),
            external_session_id: native.map(|session| session.native_identity.clone()),
        },
        started_at,
        ended_at,
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": native.map(|session| &session.native_identity),
                "source_format": GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "source_identity": source_identity,
                "source_revision": source_revision,
                "source_identity_key": native.map(|session| {
                    provider_scoped_source_identity_key(
                        CaptureProvider::Goose,
                        &session.native_identity,
                        GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
                        Some(raw_source_path),
                    )
                }),
            }),
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn publish_goose_event(
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    committed_store: &Store,
    context: &GoosePublicationContext<'_>,
    source_id: Uuid,
    session: &Session,
    native: &GooseNativeEvent,
    snapshot: &rusqlite::Connection,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    if native.file_touches.len() > GOOSE_MAX_TOUCHES_PER_EVENT {
        return Err(CaptureError::InvalidPayload(format!(
            "Goose native event {} has {} file relationships; bounded Store publication permits at most {GOOSE_MAX_TOUCHES_PER_EVENT}",
            native.native_identity,
            native.file_touches.len()
        )));
    }
    let provider_event_index = u64::try_from(native.native_order).map_err(|_| {
        CaptureError::InvalidPayload(format!(
            "Goose native event {} has a negative message order",
            native.native_identity
        ))
    })?;
    let legacy_provider_event_index = goose_legacy_event_index(native);
    let identity = provider_event_import_identity_with_exact_legacy_source(
        committed_store,
        CaptureProvider::Goose,
        &native.session_identity,
        source_id,
        provider_event_index,
        provider_event_index,
        &native.provider_message_identity,
        None,
        Some(legacy_provider_event_index),
        session.id
            == crate::provider::importer::provider_session_uuid(
                CaptureProvider::Goose,
                &native.session_identity,
            ),
    )?;
    let payload_hash = goose_event_payload_hash(native);
    let dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&identity.dedupe_key, &payload_hash)
            .unwrap_or(identity.dedupe_key);
    let event_type = match native.kind {
        GooseNativeEventKind::Message => EventType::Message,
        GooseNativeEventKind::ToolCall => EventType::ToolCall,
        GooseNativeEventKind::ToolOutput => EventType::ToolOutput,
    };
    let occurred_at = native.created_timestamp.map_or_else(
        || goose_timestamp(native.timestamp.as_deref(), session.started_at),
        |timestamp| provider_timestamp_seconds(Some(timestamp as f64), session.started_at),
    );
    let retained_text =
        provider_policy_event_text(event_type, &native.searchable_text, &native.content);
    let body = provider_capped_json(
        &provider_policy_body(event_type, &native.content),
        PROVIDER_MAX_PREVIEW_CHARS,
    );
    let mut sync_metadata = json!({
        "provider_session_id": native.session_identity,
        "provider_event_index": provider_event_index,
        "legacy_provider_event_index": legacy_provider_event_index,
        "provider_event_hash": &payload_hash,
        "provider_event_hash_authority": "normalized_payload_fallback",
        "source_format": GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
        "source_trust": "provider_native",
        "cursor": format!(
            "session:{}:message:{}:rowid:{}",
            native.session_identity, native.provider_message_identity, native.sqlite_rowid
        ),
        "source_record_ordinal": provider_event_index,
        "native_order": native.native_order,
        "native_rowid": native.sqlite_rowid,
        "native_identity": native.native_identity,
        "identity_degraded": native.identity_degraded,
        "tokens": native.tokens_json,
        "metadata": native.metadata_json,
    });
    let payload = json!({
        "provider": CaptureProvider::Goose.as_str(),
        "provider_session_id": native.session_identity,
        "provider_event_index": provider_event_index,
        "provider_event_hash": &payload_hash,
        "text": retained_text.text,
        "text_retention": retained_text.retention.as_json(),
        "output_preview": (event_type == EventType::ToolOutput)
            .then_some(native.searchable_text.as_str()),
        "result_outcome": native.content.get("result_outcome"),
        "exit_code": native.content.get("exit_code"),
        "duration_ms": native.content.get("duration_ms"),
        "timed_out": native.content.get("timed_out"),
        "call_id": native.content.get("call_id"),
        "body": body,
        "artifacts": [],
    });
    if event_type == EventType::Message
        && native.searchable_text.chars().count() > PROVIDER_MAX_TEXT_CHARS
    {
        let complete_text = super::normalization::goose_complete_content_text(&native.content)
            .unwrap_or_else(|| native.searchable_text.clone());
        super::content::attach_message_locator(
            snapshot,
            native.sqlite_rowid,
            &native.provider_message_identity,
            &payload,
            &mut sync_metadata,
            complete_text,
        )?;
    }
    let event = Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id: context.history_record_id,
        session_id: Some(session.id),
        run_id: None,
        event_type,
        role: Some(provider_role(Some(&native.role))),
        occurred_at,
        capture_source_id: Some(source_id),
        payload,
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(Fidelity::Imported, sync_metadata),
    };
    if group.reconcile_provider_event(
        &event,
        ProviderEventHashAuthority::NormalizedPayloadFallback,
    )? {
        summary.imported_events = summary.imported_events.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    } else {
        summary.skipped_events = summary.skipped_events.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    }
    summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);

    for touch in &native.file_touches {
        let packed_touch = provider_event_index
            .checked_mul(u64::from(u16::MAX) + 1)
            .and_then(|base| base.checked_add(u64::from(touch.ordinal)))
            .ok_or(CaptureError::SystemInvariant(
                "Goose file-touch identity overflowed",
            ))?;
        let id = provider_file_touch_import_id(
            committed_store,
            CaptureProvider::Goose,
            &native.session_identity,
            source_id,
            Some(provider_event_index),
            packed_touch,
            session.id
                == crate::provider::importer::provider_session_uuid(
                    CaptureProvider::Goose,
                    &native.session_identity,
                ),
        )?;
        group.upsert_file_touched(&FileTouched {
            id,
            history_record_id: context.history_record_id,
            run_id: None,
            event_id: Some(event.id),
            vcs_workspace_id: None,
            path: touch.path.clone(),
            change_kind: Some(touch.change_kind),
            old_path: touch.old_path.clone(),
            line_count_delta: None,
            confidence: Confidence::Explicit,
            timestamps: timestamps(occurred_at),
            source_id: Some(source_id),
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({
                    "provider": CaptureProvider::Goose.as_str(),
                    "provider_session_id": native.session_identity,
                    "provider_event_index": provider_event_index,
                    "provider_touch_index": touch.ordinal,
                    "source_format": GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
                    "evidence": touch.evidence,
                }),
            ),
        })?;
    }
    Ok(())
}

fn record_rejections(page: &GooseNativePage, summary: &mut ProviderImportSummary) {
    for rejection in &page.rejections {
        summary.record_failure(ProviderImportFailure {
            line: usize::try_from(rejection.sqlite_rowid.max(0))
                .unwrap_or(usize::MAX)
                .saturating_add(1),
            error: rejection.reason.clone(),
        });
    }
}

fn skipped_page_summary(page: &GooseNativePage) -> ProviderImportSummary {
    let mut summary = ProviderImportSummary::default();
    summary.skipped_sessions = page.sessions.len();
    summary.skipped_events = page.events.len();
    summary.skipped = page.sessions.len().saturating_add(page.events.len());
    record_rejections(page, &mut summary);
    summary.set_work_result(ProviderImportWorkResult::NoOp);
    summary
}

fn goose_page_already_committed(
    stored: Option<&SyncCursor>,
    source_revision: &str,
    page: &GooseNativePage,
) -> Result<bool> {
    let Some(stored) = stored else {
        return Ok(false);
    };
    let Some(wire) = decode_goose_cursor(&stored.cursor)? else {
        return Ok(false);
    };
    if wire.source_revision != source_revision {
        return Ok(false);
    }
    Ok(wire.frontier == page.next_frontier
        || wire.frontier.phase == GooseNativeScanPhase::Complete
        || wire.frontier.native_rows_seen > page.next_frontier.native_rows_seen)
}

fn validate_goose_cursor_predecessor(
    stored: Option<&SyncCursor>,
    source_revision: &str,
    expected: GooseNativeScanPosition,
) -> Result<()> {
    let Some(stored) = stored else {
        if expected == GooseNativeScanPosition::initial() {
            return Ok(());
        }
        return Err(CaptureError::InvalidPayload(
            "Goose NativePath page has no committed predecessor".to_owned(),
        ));
    };
    let Some(wire) = decode_goose_cursor(&stored.cursor)? else {
        let _ = CertifiedProviderCursor::decode_if_certified(&stored.cursor)?;
        if expected == GooseNativeScanPosition::initial() {
            return Ok(());
        }
        return Err(CaptureError::InvalidPayload(
            "Goose released-cursor migration must restart at the initial frontier".to_owned(),
        ));
    };
    if wire.source_revision != source_revision {
        if expected == GooseNativeScanPosition::initial() {
            return Ok(());
        }
        return Err(CaptureError::InvalidPayload(
            "Goose changed generation did not restart at the initial frontier".to_owned(),
        ));
    }
    if wire.frontier != expected {
        return Err(CaptureError::InvalidPayload(
            "Goose NativePath cursor does not match the next bounded page".to_owned(),
        ));
    }
    Ok(())
}

fn replay_outputs_or_mark_behind(
    reader: &GooseNativePathReader,
    source_root: &Path,
    source_revision: &str,
    imported_at: DateTime<Utc>,
    sink: Option<&dyn ProOutputSink>,
) {
    let Some(sink) = sink else {
        return;
    };
    if let Err(error) =
        replay_goose_outputs(reader, source_root, source_revision, imported_at, sink)
    {
        sink.mark_behind(ProOutputSinkError::new(
            "goose_nativepath_output_replay",
            error.to_string(),
        ));
    }
}

fn replay_goose_outputs(
    reader: &GooseNativePathReader,
    source_root: &Path,
    source_revision: &str,
    _imported_at: DateTime<Utc>,
    sink: &dyn ProOutputSink,
) -> Result<()> {
    let locator_identity = provider_path_identity(reader.source_observation().source_path())?;
    let output_source = OutputSourceIdentity {
        provider: CaptureProvider::Goose.as_str().to_owned(),
        namespace_id: source_root.display().to_string(),
        source_id: locator_identity.clone(),
    };
    let progress = match sink.observe_source(&output_source) {
        Ok(progress) => progress,
        Err(error) => {
            sink.mark_behind(error);
            return Ok(());
        }
    };
    let progress_frontier = progress
        .as_ref()
        .and_then(|progress| progress.cursor.as_ref())
        .filter(|cursor| cursor.version == GOOSE_OUTPUT_FRONTIER_VERSION)
        .and_then(|cursor| serde_json::from_slice::<GooseNativeProFrontier>(&cursor.payload).ok());
    let can_resume = progress.as_ref().is_some_and(|progress| {
        progress.observed_revision == source_revision
            && progress.parser_revision == GOOSE_OUTPUT_PARSER_REVISION
            && progress.materializer_revision == sink.materializer_revision()
            && progress_frontier.is_some()
    });
    let mut scanner = reader.scanner_with_profile(
        GooseNativeProfile::CoreAndPro,
        goose_production_page_limits()?,
    )?;
    while scanner.next_page()?.is_some() {}
    let _ = scanner.finish_core()?;
    if can_resume {
        scanner.resume_pro_from(progress_frontier.expect("resume frontier is present"))?;
    }
    let mut state = GooseOutputState::new(progress, can_resume, sink.materializer_revision())?;
    while let Some(page) = scanner.next_pro_output_page()? {
        let expected_frontier = goose_safe_output_frontier(page.expected_frontier)?;
        let next_frontier = goose_safe_output_frontier(page.next_frontier)?;
        let output = NativeProOutputPage {
            inventory_generation: sink.inventory_generation(),
            source: output_source.clone(),
            source_epoch: state.source_epoch,
            observed_revision: source_revision.to_owned(),
            parser_revision: GOOSE_OUTPUT_PARSER_REVISION.to_owned(),
            materializer_revision: sink.materializer_revision().to_owned(),
            disposition: state.disposition,
            expected_prior_source_epoch: state.expected_source_epoch,
            expected_prior_frontier: state.expected_frontier.clone(),
            observations: page.observations,
        };
        let replay = NativeProReplayPage::new_with_source_identity(
            NativeSourceIdentity::new(CaptureProvider::Goose.as_str(), &locator_identity),
            expected_frontier,
            next_frontier.clone(),
            page.terminal,
            NativePageAccounting {
                logical_units: page.accounting.logical_units,
                conservative_serialized_bytes: page.accounting.conservative_serialized_bytes,
            },
            output,
        )
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        if let Err(failure) = process_pro_replay_only(replay, sink) {
            sink.mark_behind(ProOutputSinkError::new(
                "goose_nativepath_output_replay_page",
                format!("{:?}", failure.output_error),
            ));
            return Ok(());
        }
        state.expected_source_epoch = Some(state.source_epoch);
        state.expected_frontier = Some(next_frontier);
        state.disposition = ProOutputSourceDisposition::AppendOrResume;
    }
    let _ = scanner.finish_pro_replay()?;
    Ok(())
}

struct GooseOutputState {
    source_epoch: u64,
    expected_source_epoch: Option<u64>,
    expected_frontier: Option<NativeSafeFrontier>,
    disposition: ProOutputSourceDisposition,
}

impl GooseOutputState {
    fn new(
        progress: Option<ProOutputProgress>,
        can_resume: bool,
        materializer_revision: &str,
    ) -> Result<Self> {
        let Some(progress) = progress else {
            return Ok(Self {
                source_epoch: 0,
                expected_source_epoch: None,
                expected_frontier: None,
                disposition: ProOutputSourceDisposition::NewSource,
            });
        };
        let expected_frontier = progress
            .cursor
            .as_ref()
            .map(|cursor| NativeSafeFrontier::new(cursor.version, cursor.payload.clone()))
            .transpose()
            .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
        let rewrite = !can_resume || progress.materializer_revision != materializer_revision;
        Ok(Self {
            source_epoch: if rewrite {
                progress
                    .source_epoch
                    .checked_add(1)
                    .ok_or(CaptureError::SystemInvariant(
                        "Goose output source epoch exhausted",
                    ))?
            } else {
                progress.source_epoch
            },
            expected_source_epoch: Some(progress.source_epoch),
            expected_frontier,
            disposition: if rewrite {
                ProOutputSourceDisposition::Rewrite
            } else {
                ProOutputSourceDisposition::AppendOrResume
            },
        })
    }
}

fn goose_safe_output_frontier(frontier: GooseNativeProFrontier) -> Result<NativeSafeFrontier> {
    NativeSafeFrontier::new(
        GOOSE_OUTPUT_FRONTIER_VERSION,
        serde_json::to_vec(&frontier)?,
    )
    .map_err(|error| CaptureError::InvalidPayload(error.to_string()))
}

fn goose_production_page_limits() -> Result<GooseNativePageLimits> {
    GooseNativePageLimits::new(64, GOOSE_PRODUCTION_PAGE_BYTES)
}

fn known_goose_routes(
    store: &Store,
    machine_id: &str,
    selected: &Path,
) -> Result<Vec<GooseKnownRoute>> {
    let selected = selected
        .canonicalize()
        .unwrap_or_else(|_| selected.to_path_buf());
    let mut routes = BTreeMap::<String, GooseKnownRoute>::new();
    for source in store.list_capture_sources()? {
        if source.descriptor.provider != CaptureProvider::Goose
            || source.descriptor.machine_id != machine_id
            || source.descriptor.source_format.as_deref()
                != Some(GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT)
        {
            continue;
        }
        let (Some(raw_path), Some(canonical_source_identity), Some(source_revision)) = (
            source.descriptor.raw_source_path.as_deref(),
            source.descriptor.source_identity.as_deref(),
            source
                .sync
                .metadata
                .get("source_revision")
                .and_then(Value::as_str),
        ) else {
            continue;
        };
        let path = PathBuf::from(raw_path);
        if path != selected && path.canonicalize().ok().as_ref() != Some(&selected) {
            continue;
        }
        let locator_identity = provider_path_identity(&path)?;
        let stream = provider_source_cursor_stream_for_path(
            CaptureProvider::Goose,
            GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
            &locator_identity,
        );
        let Some(current_cursor) = store.get_sync_cursor(None, machine_id, &stream)? else {
            continue;
        };
        let Some(wire) = decode_goose_cursor(&current_cursor.cursor)? else {
            continue;
        };
        routes
            .entry(locator_identity.clone())
            .or_insert(GooseKnownRoute {
                locator_identity,
                canonical_source_identity: canonical_source_identity.to_owned(),
                source_revision: source_revision.to_owned(),
                current_cursor,
                wire,
            });
    }
    Ok(routes.into_values().collect())
}

fn retire_missing_goose_route(
    store: &mut Store,
    context: &ProviderAdapterContext,
    route: &GooseKnownRoute,
    reason: ProviderSourceRouteRetirementReason,
) -> Result<ProviderImportSummary> {
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let transition = NativePathCursorTransition::new(
            Some(route.current_cursor.cursor.clone()),
            provider_sync_cursor(
                &context.machine_id,
                route.current_cursor.stream.clone(),
                encode_goose_cursor(&route.wire)?,
                context.imported_at,
            ),
        );
        let retirement = ProviderSourceRouteRetirement {
            provider: CaptureProvider::Goose,
            source_format: GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            locator_identity: route.locator_identity.clone(),
            cursor_stream: route.current_cursor.stream.clone(),
            expected_canonical_source_identity: route.canonical_source_identity.clone(),
            expected_source_revision: route.source_revision.clone(),
            retired_at_ms: context.imported_at.timestamp_millis(),
            reason,
        };
        let publication_id = goose_retirement_publication_id(&retirement);
        let admission = store.admit_event_search_bulk_group(&bulk_guard)?;
        let mut group = store.begin_native_path_publication_group(
            admission,
            NativePathGroupAccounting::new(0, 1, 0)?,
        )?;
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
        let mut summary = ProviderImportSummary::default();
        summary.set_work_result(if changed {
            ProviderImportWorkResult::Changed
        } else {
            ProviderImportWorkResult::NoOp
        });
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

fn decode_goose_cursor(encoded: &str) -> Result<Option<GooseNativeCursorWire>> {
    let provider_cursor = ctx_history_store::decode_native_path_committed_cursor(encoded)
        .map(|cursor| cursor.provider_cursor().to_owned())
        .unwrap_or_else(|_| encoded.to_owned());
    let Ok(wire) = serde_json::from_str::<GooseNativeCursorWire>(&provider_cursor) else {
        return Ok(None);
    };
    if wire.version != GOOSE_NATIVE_CURSOR_VERSION || wire.kind != GOOSE_NATIVE_CURSOR_KIND {
        return Err(CaptureError::InvalidPayload(
            "Goose NativePath cursor has an unsupported version or kind".to_owned(),
        ));
    }
    Ok(Some(wire))
}

fn encode_goose_cursor(wire: &GooseNativeCursorWire) -> Result<String> {
    serde_json::to_string(wire).map_err(CaptureError::from)
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
                CaptureProvider::Goose.as_str(),
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

fn goose_publication_id(
    page_identity: [u8; 32],
    transition: &NativePathCursorTransition,
) -> String {
    let mut digest = Sha256::new();
    digest.update(GOOSE_NATIVE_CURSOR_DOMAIN);
    digest.update(page_identity);
    digest.update(transition.key().stream().as_bytes());
    if let Some(expected) = transition.expected_cursor() {
        digest.update(expected.as_bytes());
    }
    digest.update(transition.next().cursor.as_bytes());
    format!("goose-nativepath:{:x}", digest.finalize())
}

fn goose_retirement_publication_id(retirement: &ProviderSourceRouteRetirement) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-goose-nativepath-retirement-v1\0");
    digest.update(retirement.machine_id.as_bytes());
    digest.update(retirement.locator_identity.as_bytes());
    digest.update(retirement.expected_canonical_source_identity.as_bytes());
    digest.update(retirement.expected_source_revision.as_bytes());
    format!("goose-nativepath-retirement:{:x}", digest.finalize())
}

fn goose_source_revision(reader: &GooseNativePathReader) -> String {
    let mut digest = Sha256::new();
    digest.update(GOOSE_SOURCE_REVISION_DOMAIN);
    digest.update(reader.source_observation().generation_digest().as_bytes());
    digest.update(reader.schema().capability_digest.as_bytes());
    format!("{:x}", digest.finalize())
}

fn goose_inventory_token(path: &Path) -> String {
    let mut digest = Sha256::new();
    digest.update(GOOSE_INVENTORY_TOKEN_DOMAIN);
    digest.update(path.as_os_str().as_encoded_bytes());
    format!("{:x}", digest.finalize())
}

fn goose_empty_source_id(source_identity: &str) -> Uuid {
    stable_capture_uuid(
        &format!("goose-nativepath-empty:{source_identity}"),
        "source",
    )
}

fn goose_legacy_event_index(event: &GooseNativeEvent) -> u64 {
    let base = event.created_timestamp.unwrap_or(event.sqlite_rowid).max(0) as u64;
    base.saturating_mul(4_096).saturating_add(
        crate::provider::normalization::text_id_index(&event.provider_message_identity, 0) % 4_096,
    )
}

fn goose_event_payload_hash(event: &GooseNativeEvent) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-goose-nativepath-canonical-event-v1\0");
    digest.update(event.native_order.to_le_bytes());
    digest.update(event.native_identity.as_bytes());
    digest.update(event.provider_message_identity.as_bytes());
    digest.update(event.session_identity.as_bytes());
    digest.update(event.role.as_bytes());
    digest.update(event.content.to_string().as_bytes());
    digest.update(event.searchable_text.as_bytes());
    digest.update(event.created_timestamp.unwrap_or_default().to_le_bytes());
    if let Some(timestamp) = &event.timestamp {
        digest.update(timestamp.as_bytes());
    }
    if let Some(tokens) = &event.tokens_json {
        digest.update(tokens.as_bytes());
    }
    if let Some(metadata) = &event.metadata_json {
        digest.update(metadata.as_bytes());
    }
    format!("{:x}", digest.finalize())
}

fn path_exists(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}
