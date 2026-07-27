use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind, Confidence, Event,
    Fidelity, FileTouched, Session, SessionEdge, SessionEdgeType, SyncCursor,
};
use ctx_history_store::{
    CanonicalActor, EventSearchBulkGuard, NativePathCursorSetClassification,
    NativePathCursorTransition, NativePathGroupAccounting, ProviderSourceLocatorObservation, Store,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::complete_content::VERIFIED_CONTENT_LOCATORS_METADATA_KEY;
use crate::provider::importer::{
    provider_event_import_identity_with_exact_legacy_source, provider_file_touch_import_id,
    provider_import_session_uuid, provider_native_event_import_identity_migrating_legacy_hash,
    provider_path_identity, provider_scoped_source_identity_key,
    provider_source_cursor_stream_for_path, provider_source_event_import_identity,
    provider_source_identity, provider_sync_metadata, timestamps,
};
use crate::{
    stable_capture_uuid, CaptureError, ProviderImportSummary, ProviderImportWorkResult, Result,
};

use super::{
    encode_direct_jsonl_cursor,
    reader::{direct_jsonl_source_revision, observe_file},
    DirectJsonlEvent, DirectJsonlPage, DirectJsonlSession,
};

const DIRECT_JSONL_PUBLICATION_DOMAIN: &[u8] = b"ctx-direct-jsonl-publication-v1\0";

pub(crate) struct DirectJsonlPublicationContext<'a> {
    pub(crate) provider: CaptureProvider,
    pub(crate) source_format: &'static str,
    pub(crate) machine_id: &'a str,
    pub(crate) source_root: &'a Path,
    pub(crate) imported_at: DateTime<Utc>,
    pub(crate) history_record_id: Option<Uuid>,
    pub(crate) inventory_observation_token: Option<&'a str>,
}

pub(crate) struct DirectJsonlPendingPage {
    pub(crate) path: PathBuf,
    pub(crate) page: DirectJsonlPage,
}

struct ResolvedSource {
    source_id: Uuid,
    session_id: Option<Uuid>,
    session: Option<Session>,
}

pub(crate) fn publish_direct_jsonl_group(
    store: &mut Store,
    committed_store: &Store,
    bulk_guard: &EventSearchBulkGuard,
    context: &DirectJsonlPublicationContext<'_>,
    pages: &[DirectJsonlPendingPage],
) -> Result<ProviderImportSummary> {
    if pages.is_empty() {
        return Ok(ProviderImportSummary::default());
    }
    let source_paths = pages
        .iter()
        .map(|pending| pending.path.clone())
        .collect::<BTreeSet<_>>();
    for path in &source_paths {
        let expected = pages
            .iter()
            .find(|pending| &pending.path == path)
            .expect("source path came from pending pages")
            .page
            .next_checkpoint
            .source_observation
            .clone();
        if observe_file(path)? != expected {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
    }

    let mut transitions = Vec::with_capacity(source_paths.len());
    for path in &source_paths {
        let locator_identity = provider_path_identity(path)?;
        let stream = provider_source_cursor_stream_for_path(
            context.provider,
            context.source_format,
            &locator_identity,
        );
        let stored = store.get_sync_cursor(None, context.machine_id, &stream)?;
        let final_checkpoint = pages
            .iter()
            .rev()
            .find(|pending| &pending.path == path)
            .expect("source path came from pending pages")
            .page
            .next_checkpoint
            .clone();
        let provider_cursor = encode_direct_jsonl_cursor(&final_checkpoint)?;
        let next = provider_sync_cursor(
            context.provider,
            context.machine_id,
            stream,
            provider_cursor,
            context.imported_at,
        );
        transitions.push(NativePathCursorTransition::new(
            stored.as_ref().map(|cursor| cursor.cursor.clone()),
            next,
        ));
    }
    let publication_id = publication_id(context, pages, &transitions);
    let retained_bytes = pages.iter().fold(0_usize, |total, pending| {
        total.saturating_add(pending.page.conservative_serialized_bytes)
    });
    let accounting =
        NativePathGroupAccounting::new(pages.len(), source_paths.len(), retained_bytes)?;
    let admission = store.admit_event_search_bulk_group(bulk_guard)?;
    let mut group = store.begin_native_path_publication_group(admission, accounting)?;
    match group.classify_cursor_set(&publication_id, &transitions)? {
        NativePathCursorSetClassification::AllNextSameGroup { .. } => {
            group.commit()?;
            let mut summary = ProviderImportSummary::default();
            summary.skipped = pages.iter().map(|pending| pending.page.events.len()).sum();
            summary.skipped_events = summary.skipped;
            summary.set_work_result(ProviderImportWorkResult::NoOp);
            return Ok(summary);
        }
        NativePathCursorSetClassification::AllExpected => {}
    }

    let mut summary = ProviderImportSummary::default();
    let mut resolved = BTreeMap::new();
    for path in &source_paths {
        let final_checkpoint = &pages
            .iter()
            .rev()
            .find(|pending| &pending.path == path)
            .expect("source path came from pending pages")
            .page
            .next_checkpoint;
        let source_revision = source_revision(
            &final_checkpoint.source_observation,
            context.inventory_observation_token,
        );
        let session_fact = final_checkpoint.session.as_ref();
        if session_fact.is_none() {
            // A malformed, headerless, or empty source has no canonical Core
            // owner. Its path-scoped cursor and bounded rejection summary are
            // durable, but it must not create or rebind capture-source state.
            continue;
        }
        let raw_source_path = path.display().to_string();
        let source_root = context.source_root.display().to_string();
        let locator_identity = provider_path_identity(path)?;
        let proposed_source_identity = provider_source_identity(
            context.provider,
            context.source_format,
            Some(&source_root),
            Some(&raw_source_path),
            None,
            &Value::Null,
        )
        .ok_or(CaptureError::SystemInvariant(
            "direct JSONL source has no canonical identity",
        ))?;
        let stream = provider_source_cursor_stream_for_path(
            context.provider,
            context.source_format,
            &locator_identity,
        );
        let resolution =
            group.reconcile_provider_source_locator(&ProviderSourceLocatorObservation {
                provider: context.provider,
                source_format: context.source_format.to_owned(),
                machine_id: context.machine_id.to_owned(),
                locator_identity,
                cursor_stream: stream,
                proposed_source_identity,
                raw_source_path: Some(raw_source_path.clone()),
                source_revision: source_revision.clone(),
                observed_at_ms: context.imported_at.timestamp_millis(),
            })?;
        let source_id = match session_fact {
            Some(session) => committed_store
                .capture_source_by_canonical_identity_session(
                    context.provider,
                    context.source_format,
                    context.machine_id,
                    &resolution.canonical_source_identity,
                    &session.provider_session_id,
                )?
                .map(|source| source.id)
                .unwrap_or_else(|| {
                    native_source_id(
                        context.provider,
                        context.source_format,
                        &resolution.canonical_source_identity,
                        &session.provider_session_id,
                    )
                }),
            None => native_source_id(
                context.provider,
                context.source_format,
                &resolution.canonical_source_identity,
                "<no-session>",
            ),
        };
        let source = capture_source(
            context,
            session_fact,
            source_id,
            &raw_source_path,
            &source_root,
            &resolution.canonical_source_identity,
            &source_revision,
        );
        group.upsert_capture_source(&source)?;
        group.bind_capture_source_provider_route(source_id, &resolution.route_binding())?;

        let resolved_session = session_fact
            .map(|session| {
                canonical_session(
                    committed_store,
                    context,
                    session,
                    source_id,
                    &resolution.canonical_source_identity,
                )
            })
            .transpose()?;
        if let Some(session) = &resolved_session {
            let existed_as_materialized =
                committed_store
                    .get_session(session.id)
                    .ok()
                    .is_some_and(|existing| {
                        existing.role_hint.as_deref() != Some("relationship_placeholder")
                    });
            if let Some(parent_id) = session.parent_session_id {
                if committed_store.get_session(parent_id).is_err() {
                    group.upsert_session(&relationship_placeholder(
                        context,
                        source_id,
                        parent_id,
                        final_checkpoint
                            .session
                            .as_ref()
                            .and_then(|session| session.parent_provider_session_id.as_deref())
                            .unwrap_or("unknown-parent"),
                        &resolution.canonical_source_identity,
                    ))?;
                }
            }
            group.upsert_session(session)?;
            if existed_as_materialized {
                summary.skipped_sessions = summary.skipped_sessions.saturating_add(1);
                summary.skipped = summary.skipped.saturating_add(1);
            } else {
                summary.imported_sessions = summary.imported_sessions.saturating_add(1);
                summary.imported = summary.imported.saturating_add(1);
            }
            if let Some(parent_id) = session.parent_session_id {
                let edge = relationship_edge(
                    context,
                    source_id,
                    session,
                    parent_id,
                    &resolution.canonical_source_identity,
                );
                let existed = committed_store.session_edge_exists(edge.id)?;
                group.upsert_projection_neutral_session_edge(&actor(session), &edge)?;
                if existed {
                    summary.skipped_edges = summary.skipped_edges.saturating_add(1);
                } else {
                    summary.imported_edges = summary.imported_edges.saturating_add(1);
                    summary.imported = summary.imported.saturating_add(1);
                }
            }
        }
        resolved.insert(
            path.clone(),
            ResolvedSource {
                source_id,
                session_id: resolved_session.as_ref().map(|session| session.id),
                session: resolved_session,
            },
        );
    }

    for pending in pages {
        for rejection in &pending.page.rejections {
            summary.record_failure(crate::ProviderImportFailure {
                line: usize::try_from(rejection.raw_ordinal)
                    .unwrap_or(usize::MAX)
                    .saturating_add(1),
                error: rejection.reason.clone(),
            });
        }
        if pending.page.events.is_empty() {
            continue;
        }
        let source = resolved
            .get(&pending.path)
            .ok_or(CaptureError::SystemInvariant(
                "direct JSONL publication lost its resolved source",
            ))?;
        let session = source.session.as_ref();
        for event in &pending.page.events {
            let session = session.ok_or(CaptureError::SystemInvariant(
                "direct JSONL page with events has no session",
            ))?;
            publish_event(
                &mut group,
                committed_store,
                context,
                source.source_id,
                source.session_id.expect("resolved session is present"),
                session,
                event,
                &mut summary,
            )?;
        }
    }

    for path in &source_paths {
        let expected = pages
            .iter()
            .rev()
            .find(|pending| &pending.path == path)
            .expect("source path came from pending pages")
            .page
            .next_checkpoint
            .source_observation
            .clone();
        if observe_file(path)? != expected {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
    }
    group.prepare_journal_checkpoint()?;
    group.publish_cursor_set()?;
    group.commit()?;
    summary.set_work_result(ProviderImportWorkResult::Changed);
    Ok(summary)
}

fn capture_source(
    context: &DirectJsonlPublicationContext<'_>,
    session: Option<&DirectJsonlSession>,
    source_id: Uuid,
    raw_source_path: &str,
    source_root: &str,
    source_identity: &str,
    source_revision: &str,
) -> CaptureSource {
    let provider_session_id = session.map(|session| session.provider_session_id.as_str());
    CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider: context.provider,
            machine_id: context.machine_id.to_owned(),
            process_id: None,
            cwd: session.and_then(|session| session.cwd.clone()),
            raw_source_path: Some(raw_source_path.to_owned()),
            source_format: Some(context.source_format.to_owned()),
            source_root: Some(source_root.to_owned()),
            source_identity: Some(source_identity.to_owned()),
            external_session_id: provider_session_id.map(str::to_owned),
        },
        started_at: session.map_or(context.imported_at, |session| session.started_at),
        ended_at: session.and_then(|session| session.ended_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": provider_session_id,
                "source_format": context.source_format,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "source_identity": source_identity,
                "source_root": source_root,
                "source_revision": source_revision,
                "source_identity_key": provider_session_id.map(|provider_session_id| {
                    provider_scoped_source_identity_key(
                        context.provider,
                        provider_session_id,
                        context.source_format,
                        Some(raw_source_path),
                    )
                }),
                "session_metadata": session.map(|session| session.metadata.clone()),
            }),
        ),
    }
}

fn canonical_session(
    committed_store: &Store,
    context: &DirectJsonlPublicationContext<'_>,
    session: &DirectJsonlSession,
    source_id: Uuid,
    source_identity: &str,
) -> Result<Session> {
    let session_id = provider_import_session_uuid(
        committed_store,
        context.provider,
        &session.provider_session_id,
        source_id,
        Some(source_identity),
    )?;
    let parent_session_id = session
        .parent_provider_session_id
        .as_deref()
        .map(|parent| {
            provider_import_session_uuid(
                committed_store,
                context.provider,
                parent,
                source_id,
                Some(source_identity),
            )
        })
        .transpose()?;
    let root_session_id = session
        .root_provider_session_id
        .as_deref()
        .map(|root| {
            provider_import_session_uuid(
                committed_store,
                context.provider,
                root,
                source_id,
                Some(source_identity),
            )
        })
        .transpose()?
        .or(parent_session_id);
    Ok(Session {
        id: session_id,
        history_record_id: context.history_record_id,
        parent_session_id,
        root_session_id,
        capture_source_id: Some(source_id),
        provider: context.provider,
        external_session_id: Some(session.provider_session_id.clone()),
        external_agent_id: session.external_agent_id.clone(),
        agent_type: session.agent_type,
        role_hint: session.role_hint.clone(),
        is_primary: session.is_primary,
        status: session.status,
        transcript_blob_id: None,
        started_at: session.started_at,
        ended_at: session.ended_at,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider_session_id": session.provider_session_id,
                "parent_provider_session_id": session.parent_provider_session_id,
                "root_provider_session_id": session.root_provider_session_id,
                "source_format": context.source_format,
                "source_trust": "provider_native",
                "imported_at": context.imported_at,
                "metadata": session.metadata,
            }),
        ),
    })
}

fn relationship_placeholder(
    context: &DirectJsonlPublicationContext<'_>,
    source_id: Uuid,
    id: Uuid,
    external_session_id: &str,
    source_identity: &str,
) -> Session {
    Session {
        id,
        history_record_id: context.history_record_id,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: Some(source_id),
        provider: context.provider,
        external_session_id: Some(external_session_id.to_owned()),
        external_agent_id: None,
        agent_type: ctx_history_core::AgentType::Unknown,
        role_hint: Some("relationship_placeholder".to_owned()),
        is_primary: false,
        status: ctx_history_core::SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: context.imported_at,
        ended_at: None,
        timestamps: timestamps(context.imported_at),
        sync: provider_sync_metadata(
            Fidelity::Partial,
            json!({
                "provider_session_id": external_session_id,
                "source_format": context.source_format,
                "source_identity": source_identity,
                "relationship_placeholder": true,
            }),
        ),
    }
}

fn relationship_edge(
    context: &DirectJsonlPublicationContext<'_>,
    source_id: Uuid,
    session: &Session,
    parent_id: Uuid,
    source_identity: &str,
) -> SessionEdge {
    SessionEdge {
        id: stable_capture_uuid(
            &format!(
                "provider-source-root:{source_identity}:session:{}:parent_child",
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
                "source_format": context.source_format,
                "imported_at": context.imported_at,
            }),
        ),
    }
}

fn actor(session: &Session) -> CanonicalActor {
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
fn publish_event(
    group: &mut ctx_history_store::NativePathPublicationGroup<'_>,
    committed_store: &Store,
    context: &DirectJsonlPublicationContext<'_>,
    source_id: Uuid,
    session_id: Uuid,
    session: &Session,
    event: &DirectJsonlEvent,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let identity = direct_jsonl_event_publication_identity(
        committed_store,
        context,
        source_id,
        session_id,
        session,
        event,
    )?;
    let mut provider_metadata = event.metadata.clone();
    let verified_locators = provider_metadata
        .as_object_mut()
        .and_then(|metadata| metadata.remove(VERIFIED_CONTENT_LOCATORS_METADATA_KEY));
    let dedupe_key = Store::provider_event_dedupe_key_with_payload_hash(
        &identity.dedupe_key,
        &event.provider_event_hash,
    )
    .unwrap_or(identity.dedupe_key);
    let mut sync_metadata = json!({
        "provider_session_id": session.external_session_id,
        "provider_event_index": event.provider_event_index,
        "provider_event_hash": event.provider_event_hash,
        "provider_event_hash_authority": "normalized_payload_fallback",
        "legacy_provider_event_hash": event.legacy_provider_event_hash,
        "cursor": event.cursor,
        "source_format": context.source_format,
        "source_trust": "provider_native",
        "fixture_line": event.raw_ordinal.saturating_add(1),
        "imported_at": context.imported_at,
        "source_record_ordinal": event.raw_ordinal,
        "source_record_subrecord_index": event.sub_ordinal,
        "metadata": provider_metadata,
    });
    if let (Some(metadata), Some(locators)) = (sync_metadata.as_object_mut(), verified_locators) {
        metadata.insert(VERIFIED_CONTENT_LOCATORS_METADATA_KEY.to_owned(), locators);
    }
    let normalized = Event {
        id: identity.id,
        seq: identity.seq,
        history_record_id: context.history_record_id,
        session_id: Some(session_id),
        run_id: None,
        event_type: event.event_type,
        role: Some(event.role),
        occurred_at: event.occurred_at,
        capture_source_id: Some(source_id),
        payload: json!({
            "provider": context.provider.as_str(),
            "provider_session_id": session.external_session_id,
            "provider_event_index": event.provider_event_index,
            "provider_event_hash": event.provider_event_hash,
            "cursor": event.cursor,
            "artifacts": [],
            "body": crate::provider::importer::compact_provider_result_payload(
                event.event_type,
                &event.payload,
            ),
        }),
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(Fidelity::Imported, sync_metadata),
    };
    let inserted = group.reconcile_provider_event_migrating_exact_legacy_provider_hash(
        &normalized,
        &event.legacy_provider_event_hash,
    )?;
    let native_identity =
        provider_source_event_import_identity(source_id, event.provider_event_index, "");
    group.bind_event_identity_alias(
        native_identity.id,
        normalized.id,
        context.imported_at.timestamp_millis(),
    )?;
    if inserted {
        summary.imported_events = summary.imported_events.saturating_add(1);
        summary.imported = summary.imported.saturating_add(1);
    } else {
        summary.skipped_events = summary.skipped_events.saturating_add(1);
        summary.skipped = summary.skipped.saturating_add(1);
    }
    summary.accepted_content_records = summary.accepted_content_records.saturating_add(1);

    for (touch_index, touch) in event.touches.iter().enumerate() {
        let touch_index = u64::try_from(touch_index)
            .ok()
            .and_then(|touch| {
                event
                    .raw_ordinal
                    .checked_mul(u64::from(u16::MAX) + 1)
                    .and_then(|base| base.checked_add(touch))
            })
            .ok_or(CaptureError::SystemInvariant(
                "direct JSONL file-touch identity overflowed",
            ))?;
        let id = provider_file_touch_import_id(
            committed_store,
            context.provider,
            session.external_session_id.as_deref().unwrap_or_default(),
            source_id,
            Some(event.provider_event_index),
            touch_index,
            session_id
                == crate::provider::importer::provider_session_uuid(
                    context.provider,
                    session.external_session_id.as_deref().unwrap_or_default(),
                ),
        )?;
        group.upsert_file_touched(&FileTouched {
            id,
            history_record_id: context.history_record_id,
            run_id: None,
            event_id: Some(normalized.id),
            vcs_workspace_id: None,
            path: touch.path.clone(),
            change_kind: touch.change_kind,
            old_path: touch.old_path.clone(),
            line_count_delta: None,
            confidence: Confidence::Explicit,
            timestamps: timestamps(event.occurred_at),
            source_id: Some(source_id),
            sync: provider_sync_metadata(
                Fidelity::Imported,
                json!({
                    "provider": context.provider.as_str(),
                    "provider_session_id": session.external_session_id,
                    "provider_touch_index": touch_index,
                    "provider_event_index": event.provider_event_index,
                    "source_format": context.source_format,
                    "session_id": session_id,
                }),
            ),
        })?;
    }
    Ok(())
}

fn direct_jsonl_event_publication_identity(
    committed_store: &Store,
    context: &DirectJsonlPublicationContext<'_>,
    source_id: Uuid,
    session_id: Uuid,
    session: &Session,
    event: &DirectJsonlEvent,
) -> Result<crate::provider::importer::ProviderEventImportIdentity> {
    let provider_session_id = session.external_session_id.as_deref().unwrap_or_default();
    let allow_legacy_provider_identity = session_id
        == crate::provider::importer::provider_session_uuid(context.provider, provider_session_id);
    let current = provider_event_import_identity_with_exact_legacy_source(
        committed_store,
        context.provider,
        provider_session_id,
        source_id,
        event.provider_event_index,
        event.provider_event_sequence_index,
        &event.provider_event_hash,
        None,
        None,
        allow_legacy_provider_identity,
    )?;
    match committed_store.get_event(current.id) {
        Ok(_) => return Ok(current),
        Err(ctx_history_store::StoreError::NotFound(_)) => {}
        Err(error) => return Err(CaptureError::Store(error)),
    }

    let released = provider_native_event_import_identity_migrating_legacy_hash(
        committed_store,
        context.provider,
        provider_session_id,
        source_id,
        event.provider_event_index,
        event.provider_event_sequence_index,
        &event.provider_event_hash,
        event.legacy_provider_event_index,
        &event.legacy_provider_event_hash,
        allow_legacy_provider_identity,
    )?;
    if released.id == current.id {
        return Ok(current);
    }
    match committed_store.get_event(released.id) {
        Ok(existing)
            if exact_released_direct_jsonl_event(&existing, context, source_id, session, event) =>
        {
            Ok(released)
        }
        Ok(_) | Err(ctx_history_store::StoreError::NotFound(_)) => Ok(current),
        Err(error) => Err(CaptureError::Store(error)),
    }
}

fn exact_released_direct_jsonl_event(
    existing: &Event,
    context: &DirectJsonlPublicationContext<'_>,
    source_id: Uuid,
    session: &Session,
    incoming: &DirectJsonlEvent,
) -> bool {
    let metadata = &existing.sync.metadata;
    let stored_hash = metadata.get("provider_event_hash").and_then(Value::as_str);
    let hash_authority = metadata
        .get("provider_event_hash_authority")
        .and_then(Value::as_str);
    let ordinal_matches = metadata
        .get("source_record_ordinal")
        .and_then(Value::as_u64)
        .or_else(|| metadata.get("provider_event_index").and_then(Value::as_u64))
        == Some(incoming.legacy_provider_event_index);
    let subrecord_matches = metadata
        .get("source_record_subrecord_index")
        .and_then(Value::as_u64)
        .map_or(incoming.sub_ordinal == 0, |stored| {
            stored == u64::from(incoming.sub_ordinal)
        });
    let exact_hash_matches = match hash_authority {
        Some("provider_supplied") => {
            stored_hash == Some(incoming.legacy_provider_event_hash.as_str())
        }
        Some("normalized_payload_fallback") => {
            stored_hash == Some(incoming.provider_event_hash.as_str())
        }
        _ => false,
    };
    existing.capture_source_id == Some(source_id)
        && existing.session_id == Some(session.id)
        && metadata.get("provider_session_id").and_then(Value::as_str)
            == session.external_session_id.as_deref()
        && metadata.get("source_format").and_then(Value::as_str) == Some(context.source_format)
        && ordinal_matches
        && subrecord_matches
        && metadata.get("cursor").and_then(Value::as_str) == Some(incoming.cursor.as_str())
        && exact_hash_matches
        && existing.dedupe_key.as_deref().is_some_and(|dedupe_key| {
            stored_hash.is_some_and(|stored_hash| {
                Store::provider_event_dedupe_key_with_payload_hash(dedupe_key, stored_hash)
                    .as_deref()
                    == Some(dedupe_key)
            })
        })
}

fn native_source_id(
    provider: CaptureProvider,
    source_format: &str,
    source_identity: &str,
    provider_session_id: &str,
) -> Uuid {
    stable_capture_uuid(
        &serde_json::to_string(&(
            "native-path-provider-source-v1",
            provider.as_str(),
            source_format,
            source_identity,
            provider_session_id,
        ))
        .expect("native source identity is serializable"),
        "source",
    )
}

fn source_revision(
    observation: &super::DirectJsonlFileObservation,
    inventory_token: Option<&str>,
) -> String {
    let revision = direct_jsonl_source_revision(observation);
    let Some(token) = inventory_token else {
        return revision;
    };
    let mut digest = Sha256::new();
    digest.update(b"ctx-direct-jsonl-inventory-observation-v1\0");
    digest.update((revision.len() as u64).to_be_bytes());
    digest.update(revision.as_bytes());
    digest.update((token.len() as u64).to_be_bytes());
    digest.update(token.as_bytes());
    format!("inventory-observation-sha256-v1:{:x}", digest.finalize())
}

fn provider_sync_cursor(
    provider: CaptureProvider,
    machine_id: &str,
    stream: String,
    cursor: String,
    observed_at: DateTime<Utc>,
) -> SyncCursor {
    SyncCursor {
        id: stable_capture_uuid(
            &format!(
                "provider-cursor:{}:{}:{}",
                provider.as_str(),
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
    context: &DirectJsonlPublicationContext<'_>,
    pages: &[DirectJsonlPendingPage],
    transitions: &[NativePathCursorTransition],
) -> String {
    let mut digest = Sha256::new();
    digest.update(DIRECT_JSONL_PUBLICATION_DOMAIN);
    digest.update(context.provider.as_str().as_bytes());
    digest.update(context.source_format.as_bytes());
    digest.update((pages.len() as u64).to_be_bytes());
    for transition in transitions {
        digest.update(transition.key().stream().as_bytes());
        if let Some(expected) = transition.expected_cursor() {
            digest.update((expected.len() as u64).to_be_bytes());
            digest.update(expected.as_bytes());
        } else {
            digest.update(0_u64.to_be_bytes());
        }
        digest.update((transition.next().cursor.len() as u64).to_be_bytes());
        digest.update(transition.next().cursor.as_bytes());
    }
    format!("direct-jsonl-nativepath-v1:{:x}", digest.finalize())
}
