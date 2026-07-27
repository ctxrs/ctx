use std::{
    collections::BTreeSet,
    io,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    Confidence, Event, Fidelity, FileTouched, Session, SessionEdge, SessionEdgeType, SessionStatus,
    SyncCursor,
};
use ctx_history_store::{
    decode_native_path_committed_cursor, CanonicalActor, EventSearchBulkGuard,
    NativePathCursorSetClassification, NativePathCursorTransition, NativePathGroupAccounting,
    ProviderEventHashAuthority, ProviderSourceLocatorObservation, ProviderSourceRouteRetirement,
    ProviderSourceRouteRetirementDisposition, ProviderSourceRouteRetirementReason, Store,
    NATIVE_PATH_MAX_MUTATION_UNITS, NATIVE_PATH_MAX_RETAINED_PAGE_BYTES,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    provider::importer::{
        provider_event_import_identity_with_exact_legacy_source, provider_file_touch_import_id,
        provider_import_session_uuid, provider_path_identity, provider_scoped_source_identity_key,
        provider_scoped_source_uuid, provider_source_cursor_stream_for_path,
        provider_source_identity, provider_sync_metadata, timestamps, CertifiedProviderCursor,
    },
    stable_capture_uuid, CaptureError, CaptureWorkLimit, ProviderAdapterContext,
    ProviderImportFailure, ProviderImportOptions, ProviderImportSummary, ProviderImportWorkResult,
    Result, ZED_THREADS_SQLITE_SOURCE_FORMAT,
};

use super::{
    dto::{
        ZedNativeEvent, ZedNativeGenerationAuthority, ZedNativeMessageIdentity,
        ZedNativeScanOutcome, ZedNativeSession, ZedNativeSourceSelection,
    },
    revalidate_zed_snapshot_revision, scan_zed_nativepath,
    staging::{ZedNativeStaging, ZedStagedEvent, ZedStagedSession},
    ZedNativePathError,
};

const ZED_NATIVE_CURSOR_VERSION: u32 = 1;
const ZED_NATIVE_CAPTURE_REVISION: u32 = 1;
const ZED_NATIVE_POLICY_REVISION: u32 = 1;
const ZED_PUBLICATION_DOMAIN: &[u8] = b"ctx-zed-nativepath-publication-v1\0";
const ZED_SOURCE_REVISION_DOMAIN: &[u8] = b"ctx-zed-nativepath-source-revision-v1\0";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ZedPublicationPhase {
    Sessions,
    Events,
    Complete,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ZedNativeCursor {
    version: u32,
    provider: String,
    source_format: String,
    locator_identity: String,
    cursor_stream: String,
    canonical_source_identity: String,
    raw_source_path: PathBuf,
    source_revision: String,
    snapshot_revision: String,
    capability_digest: String,
    source_integrity_digest: String,
    core_generation_digest: String,
    generation: u64,
    phase: ZedPublicationPhase,
    position: u64,
    session_count: u64,
    event_count: u64,
    rejection_count: u64,
    terminal: bool,
    retired: bool,
}

struct ZedPublicationContext<'a> {
    path: &'a Path,
    raw_source_path: String,
    source_root: String,
    locator_identity: String,
    cursor_stream: String,
    canonical_source_identity: String,
    source_revision: String,
    authority: &'a ZedNativeGenerationAuthority,
    adapter: &'a ProviderAdapterContext,
    options: &'a ProviderImportOptions,
}

struct CursorPlan {
    current: Option<SyncCursor>,
    cursor: ZedNativeCursor,
    publish_core: bool,
}

pub(in crate::provider::providers::zed) fn import_zed_nativepath(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "Zed SQLite source must be a regular non-symlink file",
            });
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return retire_missing_zed_source(path, store, &context);
        }
        Err(error) => return Err(error.into()),
    }

    let canonical_path = std::fs::canonicalize(path)?;
    let selection = ZedNativeSourceSelection::exact(&canonical_path)
        .with_inventory_observation_token(options.inventory_observation_token.clone());
    let mut staging = ZedNativeStaging::new().map_err(map_native_error)?;
    let authority = match scan_zed_nativepath(&selection, &mut staging).map_err(map_native_error)? {
        ZedNativeScanOutcome::Complete(authority) => *authority,
        ZedNativeScanOutcome::Incomplete(_) => {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
    };
    staging.validate_relationships().map_err(map_native_error)?;

    let raw_source_path = canonical_path.display().to_string();
    let source_root = context
        .source_root_display()
        .unwrap_or_else(|| raw_source_path.clone());
    let locator_identity = provider_path_identity(&canonical_path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Zed,
        ZED_THREADS_SQLITE_SOURCE_FORMAT,
        &locator_identity,
    );
    let source_revision = zed_source_revision(&authority, &options);
    let proposed_source_identity = provider_source_identity(
        CaptureProvider::Zed,
        ZED_THREADS_SQLITE_SOURCE_FORMAT,
        Some(&source_root),
        Some(&raw_source_path),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Zed NativePath source has no canonical identity",
    ))?;
    let canonical_source_identity = predict_canonical_source_identity(
        store,
        &context.machine_id,
        &raw_source_path,
        &source_revision,
        &proposed_source_identity,
    )?;
    let session_count = staging.session_count().map_err(map_native_error)?;
    let event_count = staging.event_count().map_err(map_native_error)?;
    let rejection_count = staging.rejection_count().map_err(map_native_error)?;
    let plan = cursor_plan(
        store,
        &context,
        &cursor_stream,
        &locator_identity,
        &canonical_source_identity,
        &canonical_path,
        &source_revision,
        &authority,
        session_count,
        event_count,
        rejection_count,
    )?;
    let publication = ZedPublicationContext {
        path: &canonical_path,
        raw_source_path,
        source_root,
        locator_identity,
        cursor_stream,
        canonical_source_identity,
        source_revision,
        authority: &authority,
        adapter: &context,
        options: &options,
    };
    let output_authority = super::output::ZedOutputReplayAuthority::new(
        &publication.canonical_source_identity,
        &publication.source_revision,
        publication.authority,
    );

    if options.import_profile.is_replay_only() {
        if !plan.cursor.terminal
            || plan.cursor.retired
            || plan.cursor.source_revision != publication.source_revision
        {
            if let Some(sink) = options.import_profile.sink() {
                sink.mark_behind(crate::ProOutputSinkError::new(
                    "zed_core_not_committed",
                    "Zed output replay requires an exact completed Core generation",
                ));
            }
            return Ok(ProviderImportSummary::default());
        }
        super::output::replay_zed_outputs_or_mark_behind(
            publication.path,
            &staging,
            &output_authority,
            options.import_profile.sink(),
        );
        return Ok(ProviderImportSummary::default());
    }

    let mut summary = if plan.publish_core {
        publish_zed_core(store, &staging, &publication, plan)?
    } else {
        ProviderImportSummary::default()
    };
    let completed = load_native_cursor(store, &context.machine_id, &publication.cursor_stream)?
        .is_some_and(|cursor| {
            cursor.terminal
                && !cursor.retired
                && cursor.source_revision == publication.source_revision
        });
    if completed {
        super::output::replay_zed_outputs_or_mark_behind(
            publication.path,
            &staging,
            &output_authority,
            options.import_profile.sink(),
        );
    } else if options.capture_work_limit == CaptureWorkLimit::OneSafeGroup {
        summary.work_remaining = true;
    }
    Ok(summary)
}

fn publish_zed_core(
    store: &mut Store,
    staging: &ZedNativeStaging,
    context: &ZedPublicationContext<'_>,
    mut plan: CursorPlan,
) -> Result<ProviderImportSummary> {
    let committed_store = Store::open_read_only(store.path())?;
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
        let mut summary = ProviderImportSummary::default();
        let mut changed_groups = 0_usize;
        loop {
            let phase = plan.cursor.phase;
            if phase == ZedPublicationPhase::Complete {
                break;
            }
            let (sessions, events, next_phase, next_position, retained_bytes) = match phase {
                ZedPublicationPhase::Sessions => {
                    let sessions = staging
                        .session_batch(
                            plan.cursor.position,
                            NATIVE_PATH_MAX_MUTATION_UNITS,
                            NATIVE_PATH_MAX_RETAINED_PAGE_BYTES,
                        )
                        .map_err(map_native_error)?;
                    let consumed = u64::try_from(sessions.len()).unwrap_or(u64::MAX);
                    let next = plan.cursor.position.checked_add(consumed).ok_or(
                        CaptureError::SystemInvariant("Zed session publication cursor overflowed"),
                    )?;
                    let terminal = next >= plan.cursor.session_count;
                    let bytes = sessions.iter().fold(0_usize, |total, item| {
                        total.saturating_add(item.estimated_bytes)
                    });
                    (
                        sessions,
                        Vec::new(),
                        if terminal {
                            ZedPublicationPhase::Events
                        } else {
                            ZedPublicationPhase::Sessions
                        },
                        if terminal { 0 } else { next },
                        bytes,
                    )
                }
                ZedPublicationPhase::Events => {
                    let events = staging
                        .event_batch(
                            plan.cursor.position,
                            NATIVE_PATH_MAX_MUTATION_UNITS,
                            NATIVE_PATH_MAX_RETAINED_PAGE_BYTES,
                        )
                        .map_err(map_native_error)?;
                    let next = events
                        .last()
                        .map_or(plan.cursor.position, |item| item.ordinal);
                    let terminal = next >= plan.cursor.event_count;
                    let bytes = events.iter().fold(0_usize, |total, item| {
                        total.saturating_add(item.estimated_bytes)
                    });
                    (
                        Vec::new(),
                        events,
                        if terminal {
                            ZedPublicationPhase::Complete
                        } else {
                            ZedPublicationPhase::Events
                        },
                        next,
                        bytes,
                    )
                }
                ZedPublicationPhase::Complete => unreachable!(),
            };
            if next_phase == phase && sessions.is_empty() && events.is_empty() {
                return Err(CaptureError::SystemInvariant(
                    "Zed staged publication made no cursor progress",
                ));
            }
            let mut next_cursor = plan.cursor.clone();
            next_cursor.phase = next_phase;
            next_cursor.position = next_position;
            next_cursor.terminal = next_phase == ZedPublicationPhase::Complete;
            let next_cursor_json = encode_cursor(&next_cursor)?;
            let transition = NativePathCursorTransition::new(
                plan.current.as_ref().map(|cursor| cursor.cursor.clone()),
                provider_sync_cursor(
                    &context.adapter.machine_id,
                    context.cursor_stream.clone(),
                    next_cursor_json,
                    context.adapter.imported_at,
                ),
            );
            let changed = publish_zed_group(
                store,
                &committed_store,
                &bulk_guard,
                context,
                &transition,
                &sessions,
                &events,
                retained_bytes,
                &mut summary,
            )?;
            if changed {
                changed_groups = changed_groups.saturating_add(1);
                summary.set_work_result(ProviderImportWorkResult::Changed);
            }
            plan.current =
                store.get_sync_cursor(None, &context.adapter.machine_id, &context.cursor_stream)?;
            plan.cursor = next_cursor;
            if context.options.capture_work_limit == CaptureWorkLimit::OneSafeGroup
                && changed_groups != 0
                && !plan.cursor.terminal
            {
                summary.work_remaining = true;
                break;
            }
        }
        if summary.work_result() == ProviderImportWorkResult::Changed {
            for reason in staging
                .rejection_samples(crate::summaries::MAX_RETAINED_PROVIDER_FAILURES)
                .map_err(map_native_error)?
            {
                summary.record_failure(ProviderImportFailure {
                    line: 0,
                    error: reason,
                });
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

#[allow(clippy::too_many_arguments)]
fn publish_zed_group(
    store: &Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &ZedPublicationContext<'_>,
    transition: &NativePathCursorTransition,
    sessions: &[ZedStagedSession],
    events: &[ZedStagedEvent],
    retained_bytes: usize,
    summary: &mut ProviderImportSummary,
) -> Result<bool> {
    if !revalidate_zed_snapshot_revision(context.path, &context.authority.snapshot_revision)
        .map_err(map_native_error)?
    {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let publication_id = publication_id(context, transition, sessions, events);
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(
        admission,
        NativePathGroupAccounting::new(1, 1, retained_bytes)?,
    )?;
    let changed = match group
        .classify_cursor_set(&publication_id, std::slice::from_ref(transition))?
    {
        NativePathCursorSetClassification::AllExpected => {
            let resolution =
                group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
                    provider: CaptureProvider::Zed,
                    source_format: ZED_THREADS_SQLITE_SOURCE_FORMAT.to_owned(),
                    machine_id: context.adapter.machine_id.clone(),
                    locator_identity: context.locator_identity.clone(),
                    cursor_stream: context.cursor_stream.clone(),
                    proposed_source_identity: context.canonical_source_identity.clone(),
                    raw_source_path: Some(context.raw_source_path.clone()),
                    source_revision: context.source_revision.clone(),
                    observed_at_ms: context.adapter.imported_at.timestamp_millis(),
                })?;
            if resolution.canonical_source_identity != context.canonical_source_identity {
                return Err(CaptureError::SystemInvariant(
                    "Zed source reconciliation disagreed with preflight authority",
                ));
            }
            for staged in sessions {
                publish_session(
                    committed_store,
                    &mut group,
                    context,
                    &resolution.route_binding(),
                    staged,
                    summary,
                )?;
            }
            for staged in events {
                publish_event(committed_store, &mut group, context, staged, summary)?;
            }
            if !revalidate_zed_snapshot_revision(context.path, &context.authority.snapshot_revision)
                .map_err(map_native_error)?
            {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            group.prepare_journal_checkpoint()?;
            group.publish_cursor_set()?;
            true
        }
        NativePathCursorSetClassification::AllNextSameGroup { .. } => false,
    };
    group.commit()?;
    Ok(changed)
}

fn publish_session(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    context: &ZedPublicationContext<'_>,
    route: &ctx_history_store::ProviderSourceRouteBinding,
    staged: &ZedStagedSession,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let source_id = source_id_for_thread(committed_store, context, &staged.session.thread_id)?;
    let session = canonical_session(committed_store, context, staged, source_id)?;
    let existed = committed_store.get_session(session.id).is_ok();
    group.upsert_capture_source(&capture_source(context, &staged.session, source_id))?;
    group.bind_capture_source_provider_route(source_id, route)?;
    group.upsert_session(&session)?;
    if let Some(parent_id) = session.parent_session_id {
        group.upsert_projection_neutral_session_edge(
            &canonical_actor(&session),
            &SessionEdge {
                id: stable_capture_uuid(
                    &format!(
                        "provider-source-root:{}:session:{}:parent_child",
                        context.canonical_source_identity, staged.session.thread_id
                    ),
                    "session-edge",
                ),
                from_session_id: session.id,
                to_session_id: parent_id,
                edge_type: SessionEdgeType::ParentChild,
                confidence: Confidence::Explicit,
                source_id: Some(source_id),
                timestamps: timestamps(context.adapter.imported_at),
                sync: provider_sync_metadata(
                    Fidelity::Imported,
                    json!({
                        "provider_session_id": staged.session.thread_id,
                        "source_format": ZED_THREADS_SQLITE_SOURCE_FORMAT,
                        "imported_at": context.adapter.imported_at,
                    }),
                ),
            },
        )?;
        if existed {
            summary.skipped_edges = summary.skipped_edges.saturating_add(1);
        } else {
            summary.imported_edges = summary.imported_edges.saturating_add(1);
        }
    }
    if existed {
        summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    } else {
        summary.imported_sessions = summary.imported_sessions.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    }
    Ok(())
}

fn publish_event(
    committed_store: &Store,
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    context: &ZedPublicationContext<'_>,
    staged: &ZedStagedEvent,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let event = &staged.event;
    let thread_id = &event.identity.thread_id;
    let source_id = source_id_for_thread(committed_store, context, thread_id)?;
    let session_id = session_id_for_thread(committed_store, context, thread_id, source_id)?;
    let provider_event_index = event
        .native_order
        .message_ordinal
        .checked_mul(2)
        .and_then(|value| value.checked_add(u64::from(event.native_order.sub_ordinal)))
        .ok_or(CaptureError::SystemInvariant(
            "Zed provider event index overflowed",
        ))?;
    let identity = provider_event_import_identity_with_exact_legacy_source(
        committed_store,
        CaptureProvider::Zed,
        thread_id,
        source_id,
        provider_event_index,
        provider_event_index,
        &event.content_hash,
        None,
        Some(provider_event_index),
        session_id
            == crate::provider::importer::provider_session_uuid(CaptureProvider::Zed, thread_id),
    )?;
    let dedupe_key = Store::provider_event_dedupe_key_with_payload_hash(
        &identity.dedupe_key,
        &event.content_hash,
    )
    .unwrap_or(identity.dedupe_key);
    let normalized = Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id: context.options.history_record_id,
        session_id: Some(session_id),
        run_id: None,
        event_type: event.event_type,
        role: Some(event.role),
        occurred_at: event.occurred_at,
        capture_source_id: Some(source_id),
        payload: json!({
            "provider": CaptureProvider::Zed.as_str(),
            "provider_session_id": thread_id,
            "provider_event_index": provider_event_index,
            "provider_event_hash": event.content_hash,
            "cursor": event_cursor(event),
            "artifacts": [],
            "body": {
                "message_kind": event.kind,
                "text": event.body,
                "preview": event.preview,
                "call_ids": event.call_ids,
            },
        }),
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": thread_id,
                "provider_event_index": provider_event_index,
                "provider_event_hash": event.content_hash,
                "provider_event_hash_authority": "provider_supplied",
                "source_format": ZED_THREADS_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "source_record_ordinal": event.sqlite_rowid,
                "source_record_subrecord_index": event.native_order.message_ordinal,
                "message_identity": event.identity.message,
                "nativepath_publication": ZED_NATIVE_CAPTURE_REVISION,
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
    for (touch_index, path) in event.safe_file_touches.iter().enumerate() {
        let touch_index = u64::try_from(touch_index)
            .ok()
            .and_then(|index| {
                provider_event_index
                    .checked_shl(16)
                    .and_then(|base| base.checked_add(index))
            })
            .ok_or(CaptureError::SystemInvariant(
                "Zed file-touch identity overflowed",
            ))?;
        let id = provider_file_touch_import_id(
            committed_store,
            CaptureProvider::Zed,
            thread_id,
            source_id,
            Some(provider_event_index),
            touch_index,
            session_id
                == crate::provider::importer::provider_session_uuid(
                    CaptureProvider::Zed,
                    thread_id,
                ),
        )?;
        group.upsert_file_touched(&FileTouched {
            id,
            history_record_id: context.options.history_record_id,
            run_id: None,
            event_id: Some(normalized.id),
            vcs_workspace_id: None,
            path: path.clone(),
            change_kind: None,
            old_path: None,
            line_count_delta: None,
            confidence: Confidence::Explicit,
            timestamps: timestamps(event.occurred_at),
            source_id: Some(source_id),
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({
                    "provider": CaptureProvider::Zed.as_str(),
                    "provider_session_id": thread_id,
                    "provider_touch_index": touch_index,
                    "provider_event_index": provider_event_index,
                    "source_format": ZED_THREADS_SQLITE_SOURCE_FORMAT,
                    "session_id": session_id,
                }),
            ),
        })?;
    }
    Ok(())
}

fn capture_source(
    context: &ZedPublicationContext<'_>,
    session: &ZedNativeSession,
    source_id: Uuid,
) -> CaptureSource {
    CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: CaptureProvider::Zed,
            machine_id: context.adapter.machine_id.clone(),
            process_id: None,
            cwd: session.cwd.clone(),
            raw_source_path: Some(context.raw_source_path.clone()),
            source_format: Some(ZED_THREADS_SQLITE_SOURCE_FORMAT.to_owned()),
            source_root: Some(context.source_root.clone()),
            source_identity: Some(context.canonical_source_identity.clone()),
            external_session_id: Some(session.thread_id.clone()),
        },
        started_at: session.created_at,
        ended_at: Some(session.updated_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": session.thread_id,
                "source_format": ZED_THREADS_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.adapter.imported_at,
                "source_identity": context.canonical_source_identity,
                "source_root": context.source_root,
                "source_revision": context.source_revision,
                "source_identity_key": provider_scoped_source_identity_key(
                    CaptureProvider::Zed,
                    &session.thread_id,
                    ZED_THREADS_SQLITE_SOURCE_FORMAT,
                    Some(&context.raw_source_path),
                ),
                "nativepath_publication": ZED_NATIVE_CAPTURE_REVISION,
            }),
        ),
    }
}

fn canonical_session(
    committed_store: &Store,
    context: &ZedPublicationContext<'_>,
    staged: &ZedStagedSession,
    source_id: Uuid,
) -> Result<Session> {
    let session = &staged.session;
    let id = session_id_for_thread(committed_store, context, &session.thread_id, source_id)?;
    let parent_session_id = staged
        .parent_thread_id
        .as_deref()
        .map(|parent| session_identity_for_thread(committed_store, context, parent))
        .transpose()?;
    let root_session_id = (staged.root_thread_id != session.thread_id)
        .then(|| session_identity_for_thread(committed_store, context, &staged.root_thread_id))
        .transpose()?;
    Ok(Session {
        id,
        history_record_id: context.options.history_record_id,
        parent_session_id,
        root_session_id,
        capture_source_id: Some(source_id),
        provider: CaptureProvider::Zed,
        external_session_id: Some(session.thread_id.clone()),
        external_agent_id: None,
        agent_type: if parent_session_id.is_some() {
            AgentType::Subagent
        } else {
            AgentType::Primary
        },
        role_hint: Some(if parent_session_id.is_some() {
            "subagent".to_owned()
        } else {
            "primary".to_owned()
        }),
        is_primary: parent_session_id.is_none(),
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: session.created_at,
        ended_at: Some(session.updated_at),
        timestamps: timestamps(context.adapter.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": session.thread_id,
                "parent_provider_session_id": staged.parent_thread_id,
                "root_provider_session_id": staged.root_thread_id,
                "source_format": ZED_THREADS_SQLITE_SOURCE_FORMAT,
                "source_trust": "provider_native",
                "imported_at": context.adapter.imported_at,
                "metadata": {
                    "title": session.title,
                    "summary": session.summary,
                    "cwd": session.cwd,
                    "folder_paths": session.folder_paths,
                    "encoding": format!("{:?}", session.encoding).to_lowercase(),
                    "nativepath_publication": ZED_NATIVE_CAPTURE_REVISION,
                },
            }),
        ),
    })
}

fn source_id_for_thread(
    store: &Store,
    context: &ZedPublicationContext<'_>,
    thread_id: &str,
) -> Result<Uuid> {
    Ok(store
        .capture_source_by_canonical_identity_session(
            CaptureProvider::Zed,
            ZED_THREADS_SQLITE_SOURCE_FORMAT,
            &context.adapter.machine_id,
            &context.canonical_source_identity,
            thread_id,
        )?
        .map(|source| source.id)
        .unwrap_or_else(|| {
            provider_scoped_source_uuid(
                CaptureProvider::Zed,
                thread_id,
                ZED_THREADS_SQLITE_SOURCE_FORMAT,
                Some(&context.raw_source_path),
            )
        }))
}

fn session_id_for_thread(
    store: &Store,
    context: &ZedPublicationContext<'_>,
    thread_id: &str,
    source_id: Uuid,
) -> Result<Uuid> {
    provider_import_session_uuid(
        store,
        CaptureProvider::Zed,
        thread_id,
        source_id,
        Some(&context.canonical_source_identity),
    )
}

fn session_identity_for_thread(
    store: &Store,
    context: &ZedPublicationContext<'_>,
    thread_id: &str,
) -> Result<Uuid> {
    let source_id = source_id_for_thread(store, context, thread_id)?;
    session_id_for_thread(store, context, thread_id, source_id)
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

fn event_cursor(event: &ZedNativeEvent) -> String {
    match &event.identity.message {
        ZedNativeMessageIdentity::ProviderId {
            value,
            message_ordinal,
        } => format!(
            "thread:{}:message:{message_ordinal}:id:{value}",
            event.identity.thread_id
        ),
        ZedNativeMessageIdentity::MessageOrdinal(message_ordinal) => {
            format!(
                "thread:{}:message:{message_ordinal}",
                event.identity.thread_id
            )
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn cursor_plan(
    store: &Store,
    context: &ProviderAdapterContext,
    cursor_stream: &str,
    locator_identity: &str,
    canonical_source_identity: &str,
    path: &Path,
    source_revision: &str,
    authority: &ZedNativeGenerationAuthority,
    session_count: u64,
    event_count: u64,
    rejection_count: u64,
) -> Result<CursorPlan> {
    let current = store.get_sync_cursor(None, &context.machine_id, cursor_stream)?;
    let fresh = || ZedNativeCursor {
        version: ZED_NATIVE_CURSOR_VERSION,
        provider: CaptureProvider::Zed.as_str().to_owned(),
        source_format: ZED_THREADS_SQLITE_SOURCE_FORMAT.to_owned(),
        locator_identity: locator_identity.to_owned(),
        cursor_stream: cursor_stream.to_owned(),
        canonical_source_identity: canonical_source_identity.to_owned(),
        raw_source_path: path.to_path_buf(),
        source_revision: source_revision.to_owned(),
        snapshot_revision: authority.snapshot_revision.clone(),
        capability_digest: authority.capability_digest.clone(),
        source_integrity_digest: authority.source_integrity_digest.clone(),
        core_generation_digest: authority.core_generation_digest.clone(),
        generation: 0,
        phase: if session_count == 0 {
            ZedPublicationPhase::Events
        } else {
            ZedPublicationPhase::Sessions
        },
        position: 0,
        session_count,
        event_count,
        rejection_count,
        terminal: false,
        retired: false,
    };
    let Some(stored) = current.as_ref() else {
        return Ok(CursorPlan {
            current,
            cursor: fresh(),
            publish_core: true,
        });
    };
    let prior = match decode_native_path_committed_cursor(&stored.cursor) {
        Ok(committed) => decode_cursor(committed.provider_cursor())?,
        Err(_) => {
            if CertifiedProviderCursor::decode_if_certified(&stored.cursor)?.is_some() {
                return Ok(CursorPlan {
                    current,
                    cursor: fresh(),
                    publish_core: true,
                });
            }
            return Err(CaptureError::InvalidPayload(
                "Zed NativePath cursor is neither a committed NativePath cursor nor a released legacy cursor"
                    .to_owned(),
            ));
        }
    };
    validate_cursor_authority(
        &prior,
        cursor_stream,
        locator_identity,
        canonical_source_identity,
        path,
    )?;
    if prior.source_revision == source_revision {
        if prior.session_count != session_count
            || prior.event_count != event_count
            || prior.rejection_count != rejection_count
            || prior.snapshot_revision != authority.snapshot_revision
            || prior.capability_digest != authority.capability_digest
            || prior.source_integrity_digest != authority.source_integrity_digest
            || prior.core_generation_digest != authority.core_generation_digest
            || prior.retired
        {
            return Err(CaptureError::InvalidPayload(
                "Zed NativePath cursor disagrees with exact source authority".to_owned(),
            ));
        }
        return Ok(CursorPlan {
            current,
            publish_core: !prior.terminal,
            cursor: prior,
        });
    }
    let mut cursor = fresh();
    cursor.generation = prior
        .generation
        .checked_add(1)
        .ok_or(CaptureError::SystemInvariant(
            "Zed NativePath generation overflowed",
        ))?;
    Ok(CursorPlan {
        current,
        cursor,
        publish_core: true,
    })
}

fn validate_cursor_authority(
    cursor: &ZedNativeCursor,
    cursor_stream: &str,
    locator_identity: &str,
    canonical_source_identity: &str,
    path: &Path,
) -> Result<()> {
    let phase_valid = match cursor.phase {
        ZedPublicationPhase::Sessions => {
            cursor.position <= cursor.session_count && !cursor.terminal
        }
        ZedPublicationPhase::Events => cursor.position <= cursor.event_count && !cursor.terminal,
        ZedPublicationPhase::Complete => cursor.terminal,
    };
    if cursor.version != ZED_NATIVE_CURSOR_VERSION
        || cursor.provider != CaptureProvider::Zed.as_str()
        || cursor.source_format != ZED_THREADS_SQLITE_SOURCE_FORMAT
        || cursor.cursor_stream != cursor_stream
        || cursor.locator_identity != locator_identity
        || cursor.canonical_source_identity != canonical_source_identity
        || cursor.raw_source_path != path
        || !phase_valid
    {
        return Err(CaptureError::InvalidPayload(
            "Zed NativePath cursor has inconsistent route or frontier authority".to_owned(),
        ));
    }
    Ok(())
}

fn load_native_cursor(
    store: &Store,
    machine_id: &str,
    cursor_stream: &str,
) -> Result<Option<ZedNativeCursor>> {
    let Some(stored) = store.get_sync_cursor(None, machine_id, cursor_stream)? else {
        return Ok(None);
    };
    let committed = decode_native_path_committed_cursor(&stored.cursor)?;
    decode_cursor(committed.provider_cursor()).map(Some)
}

fn encode_cursor(cursor: &ZedNativeCursor) -> Result<String> {
    serde_json::to_string(cursor).map_err(CaptureError::from)
}

fn decode_cursor(cursor: &str) -> Result<ZedNativeCursor> {
    serde_json::from_str(cursor).map_err(|error| {
        CaptureError::InvalidPayload(format!("invalid Zed NativePath cursor: {error}"))
    })
}

fn zed_source_revision(
    authority: &ZedNativeGenerationAuthority,
    options: &ProviderImportOptions,
) -> String {
    let mut digest = Sha256::new();
    digest.update(ZED_SOURCE_REVISION_DOMAIN);
    digest.update(ZED_NATIVE_CAPTURE_REVISION.to_be_bytes());
    digest.update(ZED_NATIVE_POLICY_REVISION.to_be_bytes());
    hash_field(&mut digest, authority.snapshot_revision.as_bytes());
    hash_field(&mut digest, authority.capability_digest.as_bytes());
    hash_field(&mut digest, authority.source_integrity_digest.as_bytes());
    hash_field(&mut digest, authority.core_generation_digest.as_bytes());
    if let Some(token) = options.inventory_observation_token.as_deref() {
        hash_field(&mut digest, token.as_bytes());
    }
    format!("zed-nativepath-sha256-v1:{:x}", digest.finalize())
}

fn predict_canonical_source_identity(
    store: &Store,
    machine_id: &str,
    raw_source_path: &str,
    source_revision: &str,
    proposed: &str,
) -> Result<String> {
    let sources = store.list_capture_sources()?;
    let exact = sources
        .iter()
        .filter(|source| {
            source.descriptor.provider == CaptureProvider::Zed
                && source.descriptor.machine_id == machine_id
                && source.descriptor.source_format.as_deref()
                    == Some(ZED_THREADS_SQLITE_SOURCE_FORMAT)
                && source.descriptor.raw_source_path.as_deref() == Some(raw_source_path)
        })
        .filter_map(|source| source.descriptor.source_identity.clone())
        .collect::<BTreeSet<_>>();
    if exact.len() == 1 {
        return Ok(exact
            .into_iter()
            .next()
            .unwrap_or_else(|| proposed.to_owned()));
    }
    let relocation = sources
        .iter()
        .filter(|source| {
            source.descriptor.provider == CaptureProvider::Zed
                && source.descriptor.machine_id == machine_id
                && source.descriptor.source_format.as_deref()
                    == Some(ZED_THREADS_SQLITE_SOURCE_FORMAT)
                && source
                    .sync
                    .metadata
                    .get("source_revision")
                    .and_then(Value::as_str)
                    == Some(source_revision)
                && source
                    .descriptor
                    .raw_source_path
                    .as_deref()
                    .is_some_and(|path| std::fs::symlink_metadata(path).is_err())
        })
        .filter_map(|source| source.descriptor.source_identity.clone())
        .collect::<BTreeSet<_>>();
    Ok(if relocation.len() == 1 {
        relocation
            .into_iter()
            .next()
            .unwrap_or_else(|| proposed.to_owned())
    } else {
        proposed.to_owned()
    })
}

fn retire_missing_zed_source(
    path: &Path,
    store: &mut Store,
    context: &ProviderAdapterContext,
) -> Result<ProviderImportSummary> {
    let locator_identity = provider_path_identity(path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Zed,
        ZED_THREADS_SQLITE_SOURCE_FORMAT,
        &locator_identity,
    );
    let Some(current) = store.get_sync_cursor(None, &context.machine_id, &cursor_stream)? else {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Zed SQLite source does not exist",
        });
    };
    let committed = decode_native_path_committed_cursor(&current.cursor).map_err(|_| {
        CaptureError::InvalidPayload(
            "missing Zed source has no NativePath route authority to retire".to_owned(),
        )
    })?;
    let mut cursor = decode_cursor(committed.provider_cursor())?;
    validate_cursor_authority(
        &cursor,
        &cursor_stream,
        &locator_identity,
        &cursor.canonical_source_identity.clone(),
        path,
    )?;
    if cursor.retired {
        return Ok(ProviderImportSummary::default());
    }
    cursor.retired = true;
    cursor.terminal = true;
    cursor.phase = ZedPublicationPhase::Complete;
    let transition = NativePathCursorTransition::new(
        Some(current.cursor.clone()),
        provider_sync_cursor(
            &context.machine_id,
            cursor_stream.clone(),
            encode_cursor(&cursor)?,
            context.imported_at,
        ),
    );
    let retirement = ProviderSourceRouteRetirement {
        provider: CaptureProvider::Zed,
        source_format: ZED_THREADS_SQLITE_SOURCE_FORMAT.to_owned(),
        machine_id: context.machine_id.clone(),
        locator_identity,
        cursor_stream,
        expected_canonical_source_identity: cursor.canonical_source_identity.clone(),
        expected_source_revision: cursor.source_revision.clone(),
        retired_at_ms: context.imported_at.timestamp_millis(),
        reason: if path
            .parent()
            .is_some_and(|parent| std::fs::symlink_metadata(parent).is_err())
        {
            ProviderSourceRouteRetirementReason::RootMissing
        } else {
            ProviderSourceRouteRetirementReason::SourceMissing
        },
    };
    let publication_id = retirement_publication_id(&retirement, transition.next().cursor.as_str());
    let bulk_guard = store.begin_event_search_bulk_mode()?;
    let operation = (|| {
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
        if changed {
            summary.skipped_sessions = 1;
            summary.skipped = 1;
            summary.set_work_result(ProviderImportWorkResult::Changed);
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
                CaptureProvider::Zed.as_str(),
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

fn publication_id(
    context: &ZedPublicationContext<'_>,
    transition: &NativePathCursorTransition,
    sessions: &[ZedStagedSession],
    events: &[ZedStagedEvent],
) -> String {
    let mut digest = Sha256::new();
    digest.update(ZED_PUBLICATION_DOMAIN);
    hash_field(&mut digest, context.source_revision.as_bytes());
    hash_field(&mut digest, context.locator_identity.as_bytes());
    hash_field(&mut digest, transition.next().cursor.as_bytes());
    for session in sessions {
        hash_field(&mut digest, session.session.thread_id.as_bytes());
    }
    for event in events {
        hash_field(&mut digest, event.event.content_hash.as_bytes());
    }
    format!("zed-nativepath-group-sha256-v1:{:x}", digest.finalize())
}

fn retirement_publication_id(
    retirement: &ProviderSourceRouteRetirement,
    next_cursor: &str,
) -> String {
    let mut digest = Sha256::new();
    digest.update(b"ctx-zed-nativepath-route-retirement-v1\0");
    hash_field(&mut digest, retirement.machine_id.as_bytes());
    hash_field(&mut digest, retirement.locator_identity.as_bytes());
    hash_field(
        &mut digest,
        retirement.expected_canonical_source_identity.as_bytes(),
    );
    hash_field(&mut digest, retirement.expected_source_revision.as_bytes());
    hash_field(&mut digest, next_cursor.as_bytes());
    format!(
        "zed-nativepath-retirement-sha256-v1:{:x}",
        digest.finalize()
    )
}

fn hash_field(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
}

fn map_native_error(error: ZedNativePathError) -> CaptureError {
    match error {
        ZedNativePathError::Capture(error) => error,
        ZedNativePathError::Io(error) => CaptureError::Io(error),
        ZedNativePathError::Sqlite(error) => CaptureError::Sqlite(error),
        other => CaptureError::InvalidPayload(other.to_string()),
    }
}
