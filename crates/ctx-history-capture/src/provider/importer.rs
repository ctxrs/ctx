use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind,
    Confidence, Event, Fidelity, FileTouched, ProviderCaptureEnvelope, ProviderCursorCheckpoint,
    ProviderCursorRange, ProviderEventEnvelope, ProviderSessionEnvelope, ProviderSourceEnvelope,
    ProviderSourceTrust, Session, SessionEdge, SessionEdgeType, SessionStatus,
    PROVIDER_CAPTURE_ENVELOPE_MIN_SUPPORTED_SCHEMA_VERSION,
    PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
};
use ctx_history_store::{ProviderEventHashAuthority, Store, StoreError};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::complete_content::{
    PersistedCompleteContentLocatorV1, COMPLETE_CONTENT_LOCATOR_METADATA_KEY,
    RESULT_CONTENT_LOCATOR_METADATA_KEY,
};
use crate::compute_payload_hash;
use crate::{
    CaptureError, NormalizedProviderImportOptions, ProviderAdapterContext,
    ProviderFileTouchedEnvelope, ProviderFixtureLine, ProviderImportSummary,
    ProviderNormalizationResult, Result,
};

mod batches;
mod commands;
mod cursors;
mod existing_session;
mod identity;
mod ids;
mod legacy_identity;
mod normalized;
mod source_relocation;

#[cfg(test)]
pub(crate) use batches::import_normalized_provider_captures_in_batches;
pub(crate) use batches::{
    drain_captured_batches, emit_projected_normalization_units, import_captured_batches,
    project_default_structural_rejection, CapturedBatchCursorFinish, CapturedBatchCursorMode,
    CapturedBatchProjector, CapturedSourceAdmission, ExistingSessionEventOutcome,
    ProviderImportTransaction, ProviderProjectionFatal, ProviderProjectionOutput,
    ProviderProjectionResult,
};
pub(crate) use commands::{
    compact_provider_result_payload, provider_command_run_from_event,
    validate_provider_event_for_import, ProviderCommandRunInput,
};
#[cfg(test)]
pub(crate) use cursors::provider_source_cursor_stream;
#[cfg(all(test, unix))]
pub(crate) use cursors::MAX_PROVIDER_PATH_IDENTITY_RAW_BYTES;
pub(crate) use cursors::{
    captured_batch_cursor_stream, provider_cursor_stream, provider_path_identity,
    provider_source_cursor_range, provider_source_cursor_stream_for_path, BoundedParserCheckpoint,
    CertifiedProviderCursor,
};
pub(crate) use identity::{
    pi_existing_event_identity_by_entry_id,
    provider_event_import_identity_with_exact_legacy_source, provider_file_touch_event_id,
    provider_file_touch_import_id, provider_session_exists_cached, ExactLegacySourceEventCandidate,
    ProviderEventImportIdentity,
};
pub(crate) use ids::{
    provider_edge_uuid, provider_scoped_source_identity_key, provider_scoped_source_uuid,
    provider_session_uuid, provider_source_edge_uuid, provider_source_identity,
    provider_source_root, provider_source_session_uuid, provider_sync_metadata, timestamps,
};
use legacy_identity::legacy_session_matches_source;
pub(crate) use source_relocation::import_provider_capture_line;
use source_relocation::{
    provider_file_touch_source_id, provider_import_source_id, CanonicalProviderSourceOverride,
};

#[cfg(test)]
pub(crate) use identity::{provider_event_import_identity, provider_source_event_import_identity};
#[cfg(test)]
pub(crate) use ids::provider_source_root_identity;
#[cfg(test)]
pub(crate) use ids::{
    provider_event_seq, provider_event_uuid, provider_file_touch_uuid, provider_source_event_seq,
    provider_source_event_uuid, provider_source_uuid,
};

pub fn import_normalized_provider_captures(
    store: &mut Store,
    normalization: ProviderNormalizationResult,
    options: NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    batches::import_normalized_provider_captures(store, normalization, options)
}

pub(crate) fn import_provider_file_touched_line(
    store: &mut Store,
    file: &ProviderFileTouchedEnvelope,
    options: &NormalizedProviderImportOptions,
    canonical_source: Option<&CanonicalProviderSourceOverride>,
) -> Result<()> {
    let source_id = provider_file_touch_source_id(store, file, canonical_source)?;
    let source_root =
        provider_source_root(file.source_root.as_deref(), file.raw_source_path.as_deref());
    let source_identity = canonical_source
        .map(|canonical| canonical.stable_source_identity.clone())
        .or_else(|| {
            provider_source_identity(
                file.provider,
                &file.source_format,
                file.source_root.as_deref(),
                file.raw_source_path.as_deref(),
                None,
                &file.metadata,
            )
        });
    let session_source_identity = canonical_source
        .map(|canonical| canonical.stable_session_identity.as_str())
        .or(source_identity.as_deref());
    let inferred_session_id = provider_import_session_uuid(
        store,
        file.provider,
        &file.provider_session_id,
        source_id,
        session_source_identity,
    )?;
    let event_id = match file.provider_event_index {
        Some(index) => provider_file_touch_event_id(
            store,
            file.provider,
            &file.provider_session_id,
            source_id,
            index,
            inferred_session_id == provider_session_uuid(file.provider, &file.provider_session_id),
        )?,
        None => None,
    };
    // Event-derived file touches must retain the event's already-resolved,
    // source-scoped session identity. A synthesized file-touch envelope does
    // not carry all source metadata from its capture, so independently
    // resolving it can otherwise create a second session identity for the
    // same provider event.
    let session_id = match event_id {
        Some(event_id) => store
            .get_event(event_id)?
            .session_id
            .unwrap_or(inferred_session_id),
        None => inferred_session_id,
    };
    let touch_id = provider_file_touch_import_id(
        store,
        file.provider,
        &file.provider_session_id,
        source_id,
        file.provider_event_index,
        file.provider_touch_index,
        session_id == provider_session_uuid(file.provider, &file.provider_session_id),
    )?;
    let touched = FileTouched {
        id: touch_id,
        history_record_id: options.history_record_id,
        run_id: None,
        event_id,
        vcs_workspace_id: None,
        path: file.path.clone(),
        change_kind: file.change_kind,
        old_path: file.old_path.clone(),
        line_count_delta: file.line_count_delta,
        confidence: file.confidence,
        timestamps: timestamps(file.occurred_at),
        source_id: Some(source_id),
        sync: provider_sync_metadata(
            Fidelity::Imported,
            json!({
                "provider": file.provider.as_str(),
                "provider_session_id": file.provider_session_id,
                "provider_touch_index": file.provider_touch_index,
                "provider_event_index": file.provider_event_index,
                "raw_source_path": file.raw_source_path,
                "source_id": source_id,
                "source_format": file.source_format,
                "source_root": source_root,
                "metadata": file.metadata,
                "session_id": session_id,
            }),
        ),
    };
    store.upsert_file_touched(&touched)?;
    Ok(())
}

#[derive(Default)]
pub(crate) struct ProviderImportCaches {
    pub(crate) imported_sessions: BTreeSet<Uuid>,
    pub(crate) processed_sources: BTreeSet<Uuid>,
    pub(crate) processed_sessions: BTreeMap<Uuid, Session>,
    pub(crate) resolved_existing_sessions: BTreeMap<Uuid, Uuid>,
    pub(crate) codex_eventless_capture_byte_budgets: BTreeMap<Uuid, usize>,
    pub(crate) imported_edges: BTreeSet<Uuid>,
    pub(crate) processed_edges: BTreeSet<Uuid>,
    pub(crate) session_exists: BTreeMap<Uuid, bool>,
    pub(crate) pi_event_identities_by_entry_id:
        BTreeMap<Uuid, BTreeMap<String, ProviderEventImportIdentity>>,
}

pub(crate) fn provider_import_session_uuid(
    store: &Store,
    provider: CaptureProvider,
    provider_session_id: &str,
    source_id: Uuid,
    source_identity: Option<&str>,
) -> Result<Uuid> {
    let legacy_session_id = provider_session_uuid(provider, provider_session_id);
    let Some(source_identity) = source_identity else {
        return Ok(legacy_session_id);
    };
    if provider == CaptureProvider::Custom {
        return Ok(legacy_session_id);
    }

    if let Some(existing) = store.session_by_capture_source_and_external_session(
        source_id,
        provider,
        provider_session_id,
    )? {
        return Ok(existing.id);
    }

    let source_session_id = provider_source_session_uuid(source_identity, provider_session_id);
    match store.get_session(source_session_id) {
        Ok(_) => return Ok(source_session_id),
        Err(StoreError::NotFound(_)) => {}
        Err(err) => return Err(CaptureError::Store(err)),
    }

    match store.get_session(legacy_session_id) {
        Ok(existing)
            if legacy_session_matches_source(store, &existing, source_id, source_identity)? =>
        {
            Ok(legacy_session_id)
        }
        Ok(_) => Ok(source_session_id),
        Err(StoreError::NotFound(_)) => Ok(source_session_id),
        Err(err) => Err(CaptureError::Store(err)),
    }
}

fn provider_import_edge_uuid(
    provider: CaptureProvider,
    provider_session_id: &str,
    source_identity: Option<&str>,
    session_id: Uuid,
    edge_kind: &str,
) -> Uuid {
    if provider != CaptureProvider::Custom
        && session_id != provider_session_uuid(provider, provider_session_id)
    {
        if let Some(source_identity) = source_identity {
            return provider_source_edge_uuid(source_identity, provider_session_id, edge_kind);
        }
    }
    provider_edge_uuid(provider, provider_session_id, edge_kind)
}

fn exact_legacy_source_event_candidate(
    provider: CaptureProvider,
    session: &ProviderSessionEnvelope,
    source: &ProviderSourceEnvelope,
    event: &ProviderEventEnvelope,
) -> Option<ExactLegacySourceEventCandidate> {
    if provider != CaptureProvider::OpenHands
        || source.source_format != crate::OPENHANDS_FILE_EVENTS_SOURCE_FORMAT
    {
        return None;
    }
    let candidate = event.metadata.get("legacy_source_event_candidate_v1")?;
    let raw_source_path = candidate.get("raw_source_path")?.as_str()?;
    let provider_event_index = candidate.get("provider_event_index")?.as_u64()?;
    let current_source_path = source.raw_source_path.as_deref()?;
    if Path::new(current_source_path).parent()? != Path::new(raw_source_path) {
        return None;
    }
    Some(ExactLegacySourceEventCandidate {
        source_id: provider_scoped_source_uuid(
            provider,
            &session.provider_session_id,
            &source.source_format,
            Some(raw_source_path),
        ),
        provider_event_index,
    })
}

struct RelationshipPlaceholder<'a> {
    id: Uuid,
    current_session_id: Uuid,
    provider: CaptureProvider,
    external_session_id: &'a str,
    source_format: &'a str,
    source_identity: Option<&'a str>,
    source_root: Option<&'a str>,
    observed_at: DateTime<Utc>,
}

fn ensure_relationship_placeholder(
    store: &Store,
    caches: &mut ProviderImportCaches,
    summary: &mut ProviderImportSummary,
    placeholder: RelationshipPlaceholder<'_>,
) -> Result<()> {
    if placeholder.id == placeholder.current_session_id
        || provider_session_exists_cached(store, placeholder.id, &mut caches.session_exists)?
    {
        return Ok(());
    }

    let session = Session {
        id: placeholder.id,
        history_record_id: None,
        parent_session_id: None,
        root_session_id: None,
        capture_source_id: None,
        provider: placeholder.provider,
        external_session_id: Some(placeholder.external_session_id.to_owned()),
        external_agent_id: None,
        agent_type: AgentType::Unknown,
        role_hint: None,
        is_primary: false,
        status: SessionStatus::Imported,
        transcript_blob_id: None,
        started_at: placeholder.observed_at,
        ended_at: None,
        timestamps: timestamps(placeholder.observed_at),
        sync: provider_sync_metadata(
            Fidelity::Partial,
            json!({
                "relationship_placeholder": true,
                "provider_session_id": placeholder.external_session_id,
                "source_format": placeholder.source_format,
                "source_identity": placeholder.source_identity,
                "source_root": placeholder.source_root,
                "imported_at": placeholder.observed_at,
            }),
        ),
    };
    if store.insert_session_if_absent(&session)? {
        caches.imported_sessions.insert(placeholder.id);
        summary.imported_sessions += 1;
        summary.imported += 1;
    }
    caches.session_exists.insert(placeholder.id, true);
    Ok(())
}

pub(crate) fn import_provider_capture_line_with_canonical_source(
    store: &mut Store,
    capture: &ProviderCaptureEnvelope,
    options: &NormalizedProviderImportOptions,
    line_number: usize,
    caches: &mut ProviderImportCaches,
    canonical_source: Option<&CanonicalProviderSourceOverride>,
) -> Result<ProviderImportSummary> {
    if !(PROVIDER_CAPTURE_ENVELOPE_MIN_SUPPORTED_SCHEMA_VERSION
        ..=PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION)
        .contains(&capture.schema_version)
    {
        return Err(CaptureError::InvalidPayload(format!(
            "unsupported provider capture envelope schema version {} on line {line_number}",
            capture.schema_version
        )));
    }
    if let Some(event) = &capture.event {
        validate_provider_event_for_import(event)?;
    }

    let mut summary = ProviderImportSummary::default();
    let provider = capture.provider;
    let session = &capture.session;
    let source = &capture.source;
    let imported_at = source.observed_at;
    let source_identity_key = provider_scoped_source_identity_key(
        provider,
        &session.provider_session_id,
        &source.source_format,
        source.raw_source_path.as_deref(),
    );
    let (source_id, source_identity) = provider_import_source_id(
        store,
        provider,
        &session.provider_session_id,
        source,
        canonical_source,
    )?;
    let session_source_identity = canonical_source
        .map(|canonical| canonical.stable_session_identity.as_str())
        .or(source_identity.as_deref());
    let source_root = provider_source_root(
        source.source_root.as_deref(),
        source.raw_source_path.as_deref(),
    );
    let source_cursor = provider_source_cursor_range(capture);
    let source_metadata = source.metadata.clone();
    let session_metadata = session.metadata.clone();
    let source_record = CaptureSource {
        id: source_id,
        descriptor: CaptureSourceDescriptor {
            kind: CaptureSourceKind::ProviderImport,
            provider,
            machine_id: source.machine_id.clone(),
            process_id: None,
            cwd: session.cwd.clone(),
            raw_source_path: source.raw_source_path.clone(),
            source_format: Some(source.source_format.clone()),
            source_root: source_root.clone(),
            source_identity: source_identity.clone(),
            external_session_id: Some(session.provider_session_id.clone()),
        },
        started_at: session.started_at,
        ended_at: session.ended_at,
        sync: provider_sync_metadata(
            source.fidelity,
            json!({
                "provider_session_id": session.provider_session_id,
                "source_format": source.source_format,
                "source_trust": source.trust,
                "cursor": source_cursor,
                "fixture_line": line_number,
                "imported_at": imported_at,
                "source_idempotency_key": source.idempotency_key,
                "source_identity": source_identity.clone(),
                "source_root": source_root.clone(),
                "source_identity_key": source_identity_key,
                "source_metadata": source_metadata,
                "session_metadata": session_metadata,
            }),
        ),
    };
    if caches.processed_sources.insert(source_id) {
        store.upsert_capture_source(&source_record)?;
    }

    let session_id = provider_import_session_uuid(
        store,
        provider,
        &session.provider_session_id,
        source_id,
        session_source_identity,
    )?;
    let requested_parent_session_id = session
        .parent_provider_session_id
        .as_ref()
        .map(|id| {
            provider_import_session_uuid(store, provider, id, source_id, session_source_identity)
        })
        .transpose()?;
    if let (Some(parent_id), Some(parent_external_id)) = (
        requested_parent_session_id,
        session.parent_provider_session_id.as_deref(),
    ) {
        ensure_relationship_placeholder(
            store,
            caches,
            &mut summary,
            RelationshipPlaceholder {
                id: parent_id,
                current_session_id: session_id,
                provider,
                external_session_id: parent_external_id,
                source_format: &source.source_format,
                source_identity: session_source_identity,
                source_root: source_root.as_deref(),
                observed_at: imported_at,
            },
        )?;
    }
    let explicit_root_session_id = session
        .root_provider_session_id
        .as_ref()
        .map(|id| {
            provider_import_session_uuid(store, provider, id, source_id, session_source_identity)
        })
        .transpose()?;
    if let (Some(root_id), Some(root_external_id)) = (
        explicit_root_session_id,
        session.root_provider_session_id.as_deref(),
    ) {
        ensure_relationship_placeholder(
            store,
            caches,
            &mut summary,
            RelationshipPlaceholder {
                id: root_id,
                current_session_id: session_id,
                provider,
                external_session_id: root_external_id,
                source_format: &source.source_format,
                source_identity: session_source_identity,
                source_root: source_root.as_deref(),
                observed_at: imported_at,
            },
        )?;
    }
    let parent_session_id = requested_parent_session_id;
    let root_session_id = explicit_root_session_id.or(requested_parent_session_id);
    let process_session = !caches.processed_sessions.contains_key(&session_id);
    let is_new_session = if process_session {
        !provider_session_exists_cached(store, session_id, &mut caches.session_exists)?
    } else {
        false
    };
    let normalized_session = Session {
        id: session_id,
        history_record_id: options.history_record_id,
        parent_session_id,
        root_session_id,
        capture_source_id: Some(source_id),
        provider,
        external_session_id: Some(session.provider_session_id.clone()),
        external_agent_id: session.external_agent_id.clone(),
        agent_type: session.agent_type,
        role_hint: session.role_hint.clone(),
        is_primary: session.is_primary,
        status: session.status,
        transcript_blob_id: None,
        started_at: session.started_at,
        ended_at: session.ended_at,
        timestamps: timestamps(imported_at),
        sync: provider_sync_metadata(
            session.fidelity,
            json!({
                "provider_session_id": session.provider_session_id,
                "parent_provider_session_id": session.parent_provider_session_id,
                "root_provider_session_id": session.root_provider_session_id,
                "source_format": source.source_format,
                "source_trust": source.trust,
                "fixture_line": line_number,
                "imported_at": imported_at,
                "session_idempotency_key": session.idempotency_key,
                "artifacts": session.artifacts,
                "metadata": session_metadata,
            }),
        ),
    };
    if let Some(first_session) = caches.processed_sessions.get_mut(&session_id) {
        // A bounded batch may contain out-of-order events for one session.
        // Preserve the first normalized envelope exactly, as the prior cache
        // did, while letting Store observe expanding temporal bounds without a
        // whole-source pre-scan.
        first_session.started_at = first_session.started_at.min(normalized_session.started_at);
        first_session.ended_at = match (first_session.ended_at, normalized_session.ended_at) {
            (Some(first), Some(next)) => Some(first.max(next)),
            (first @ Some(_), None) => first,
            (None, next) => next,
        };
        store.upsert_session(first_session)?;
    } else {
        store.upsert_session(&normalized_session)?;
        caches
            .processed_sessions
            .insert(session_id, normalized_session.clone());
        caches.session_exists.insert(session_id, true);
        if is_new_session && caches.imported_sessions.insert(session_id) {
            summary.imported_sessions += 1;
            summary.imported += 1;
        } else {
            summary.skipped_sessions += 1;
            summary.skipped += 1;
        }
    }

    if let Some(parent_id) = parent_session_id {
        let edge_id = provider_import_edge_uuid(
            provider,
            &session.provider_session_id,
            session_source_identity,
            session_id,
            "parent_child",
        );
        if caches.processed_edges.insert(edge_id) {
            let was_present = store.session_edge_exists(edge_id)?;
            let edge = SessionEdge {
                id: edge_id,
                from_session_id: parent_id,
                to_session_id: session_id,
                edge_type: SessionEdgeType::ParentChild,
                confidence: Confidence::Explicit,
                source_id: Some(source_id),
                timestamps: timestamps(imported_at),
                sync: provider_sync_metadata(
                    session.fidelity,
                    json!({
                        "provider_session_id": session.provider_session_id,
                        "parent_provider_session_id": session.parent_provider_session_id,
                        "source_format": source.source_format,
                        "fixture_line": line_number,
                        "imported_at": imported_at,
                    }),
                ),
            };
            store.upsert_session_edge(&edge)?;
            if !was_present && caches.imported_edges.insert(edge_id) {
                summary.imported_edges += 1;
                summary.imported += 1;
            } else {
                summary.skipped_edges += 1;
                summary.skipped += 1;
            }
        }
    }

    if let Some(event) = &capture.event {
        import_provider_event_for_session(
            store,
            capture,
            event,
            options,
            line_number,
            caches,
            source_id,
            session_id,
            &mut summary,
        )?;
    }

    Ok(summary)
}

#[allow(clippy::too_many_arguments)]
fn import_provider_event_for_session(
    store: &mut Store,
    capture: &ProviderCaptureEnvelope,
    event: &ProviderEventEnvelope,
    options: &NormalizedProviderImportOptions,
    line_number: usize,
    caches: &mut ProviderImportCaches,
    source_id: Uuid,
    session_id: Uuid,
    summary: &mut ProviderImportSummary,
) -> Result<()> {
    let provider = capture.provider;
    let session = &capture.session;
    let source = &capture.source;
    let imported_at = source.observed_at;
    let payload = event.payload.clone();
    let mut event_metadata = event.metadata.clone();
    let source_record_coordinates = take_source_record_coordinates(&mut event_metadata)?;
    let complete_content_locator = take_complete_content_locator(&mut event_metadata)?;
    let result_content_locator =
        take_content_locator(&mut event_metadata, RESULT_CONTENT_LOCATOR_METADATA_KEY)?;
    let (event_hash, event_hash_authority) = match &event.provider_event_hash {
        Some(hash) => (hash.clone(), ProviderEventHashAuthority::ProviderSupplied),
        None => (
            compute_payload_hash(&payload)?,
            ProviderEventHashAuthority::NormalizedPayloadFallback,
        ),
    };
    let pi_entry_id = event
        .metadata
        .get("entry_id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty());
    let legacy_provider_event_index = event
        .metadata
        .get("legacy_provider_event_index")
        .and_then(Value::as_u64)
        .filter(|_| !(provider == CaptureProvider::Pi && pi_entry_id.is_some()));
    let provider_event_identity_index = event
        .metadata
        .get("provider_event_identity_index")
        .and_then(Value::as_u64)
        .unwrap_or(event.provider_event_index);
    let existing_pi_entry_identity =
        pi_existing_event_identity_by_entry_id(store, provider, session_id, pi_entry_id, caches)?;
    let stable_pi_entry_replay = existing_pi_entry_identity.is_some()
        && event_hash_authority == ProviderEventHashAuthority::NormalizedPayloadFallback;
    let event_identity = match existing_pi_entry_identity {
        Some(identity) => identity,
        None => provider_event_import_identity_with_exact_legacy_source(
            store,
            provider,
            &session.provider_session_id,
            source_id,
            provider_event_identity_index,
            event.provider_event_index,
            &event_hash,
            exact_legacy_source_event_candidate(provider, session, source, event),
            legacy_provider_event_index,
            session_id == provider_session_uuid(provider, &session.provider_session_id),
        )?,
    };
    let command_run = provider_command_run_from_event(ProviderCommandRunInput {
        provider,
        provider_session_id: &session.provider_session_id,
        session_id,
        source_id,
        run_source_id: event_identity.run_source_id,
        history_record_id: options.history_record_id,
        event,
        payload: &payload,
        event_hash: &event_hash,
    })?;
    let dedupe_key =
        Store::provider_event_dedupe_key_with_payload_hash(&event_identity.dedupe_key, &event_hash)
            .unwrap_or_else(|| event_identity.dedupe_key.clone());
    let mut normalized_metadata = json!({
        "provider_session_id": session.provider_session_id,
        "provider_event_index": event.provider_event_index,
        "provider_event_hash": event_hash,
        "provider_event_hash_authority": event_hash_authority.as_str(),
        "cursor": event.cursor,
        "source_format": source.source_format,
        "source_trust": source.trust,
        "fixture_line": line_number,
        "imported_at": imported_at,
        "event_idempotency_key": event.idempotency_key,
        "source_record_ordinal": source_record_coordinates
            .as_ref()
            .map(|coordinates| coordinates.0),
        "source_record_subrecord_index": source_record_coordinates
            .as_ref()
            .map(|coordinates| coordinates.1),
        "metadata": event_metadata,
    });
    if let Some(locator) = complete_content_locator {
        normalized_metadata
            .as_object_mut()
            .expect("normalized provider metadata is an object")
            .insert(COMPLETE_CONTENT_LOCATOR_METADATA_KEY.to_owned(), locator);
    }
    if let Some(locator) = result_content_locator {
        if let Some(object) = normalized_metadata.as_object_mut() {
            object.insert(RESULT_CONTENT_LOCATOR_METADATA_KEY.to_owned(), locator);
        }
    }
    let persisted_payload = compact_provider_result_payload(event.event_type, &payload);
    let normalized_event = Event {
        id: event_identity.id,
        seq: event_identity.seq,
        history_record_id: options.history_record_id,
        session_id: Some(session_id),
        run_id: command_run.as_ref().map(|run| run.id),
        event_type: event.event_type,
        role: event.role,
        occurred_at: event.occurred_at,
        capture_source_id: Some(source_id),
        payload: json!({
            "provider": provider.as_str(),
            "provider_session_id": session.provider_session_id,
            "provider_event_index": event.provider_event_index,
            "provider_event_hash": event_hash,
            "cursor": event.cursor,
            "artifacts": event.artifacts,
            "body": persisted_payload,
        }),
        payload_blob_id: None,
        dedupe_key: Some(dedupe_key),
        sync: provider_sync_metadata(event.fidelity, normalized_metadata),
    };
    if stable_pi_entry_replay {
        // Pi entry IDs are stable provider identities. An exact entry-ID match
        // proves a replay of a legacy line-indexed row even when that old row
        // predates verifiable fallback-hash metadata. Preserve it byte-for-byte.
    } else {
        if options.fast_event_inserts {
            if let Some(run) = &command_run {
                store.insert_run_if_absent(run)?;
            }
        } else if let Some(run) = &command_run {
            store.upsert_run(run)?;
        }
    }
    let was_present = if stable_pi_entry_replay {
        true
    } else {
        !store.reconcile_provider_event(&normalized_event, event_hash_authority)?
    };
    if was_present {
        summary.skipped_events += 1;
        summary.skipped += 1;
    } else {
        summary.imported_events += 1;
        summary.imported += 1;
    }

    summary.accepted_content_records += 1;
    Ok(())
}

fn take_complete_content_locator(metadata: &mut Value) -> Result<Option<Value>> {
    take_content_locator(metadata, COMPLETE_CONTENT_LOCATOR_METADATA_KEY)
}

fn take_content_locator(metadata: &mut Value, key: &str) -> Result<Option<Value>> {
    let Some(object) = metadata.as_object_mut() else {
        return Ok(None);
    };
    let Some(value) = object.remove(key) else {
        return Ok(None);
    };
    let locator =
        PersistedCompleteContentLocatorV1::from_metadata_value(&value).ok_or_else(|| {
            CaptureError::InvalidPayload(
                "complete content locator annotation is malformed".to_owned(),
            )
        })?;
    Ok(Some(locator.to_metadata_value()))
}

fn take_source_record_coordinates(metadata: &mut Value) -> Result<Option<(u64, u32)>> {
    let Some(object) = metadata.as_object_mut() else {
        return Ok(None);
    };
    let ordinal = object.remove("source_record_ordinal");
    let subrecord = object.remove("source_record_subrecord_index");
    if ordinal.is_none() && subrecord.is_none() {
        return Ok(None);
    }
    let ordinal = ordinal.and_then(|value| value.as_u64()).ok_or_else(|| {
        CaptureError::InvalidPayload("source record ordinal annotation is malformed".to_owned())
    })?;
    let subrecord = subrecord
        .and_then(|value| value.as_u64())
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "source record subrecord annotation is malformed".to_owned(),
            )
        })?;
    Ok(Some((ordinal, subrecord)))
}

pub(crate) fn fixture_line_to_capture(
    fixture: &ProviderFixtureLine,
    context: &ProviderAdapterContext,
    source_format: &str,
    fidelity: Fidelity,
) -> ProviderCaptureEnvelope {
    let cursor = fixture
        .event
        .as_ref()
        .and_then(|event| event.cursor.as_ref())
        .map(|cursor| ProviderCursorRange {
            before: None,
            after: Some(ProviderCursorCheckpoint {
                stream: provider_cursor_stream(fixture.provider, source_format),
                cursor: cursor.clone(),
                observed_at: fixture
                    .event
                    .as_ref()
                    .map(|event| event.occurred_at)
                    .unwrap_or(context.imported_at),
            }),
        });

    ProviderCaptureEnvelope {
        schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
        provider: fixture.provider,
        source: ProviderSourceEnvelope {
            source_format: source_format.to_owned(),
            machine_id: context.machine_id.clone(),
            observed_at: context.imported_at,
            raw_source_path: context
                .source_path
                .as_ref()
                .map(|path| path.display().to_string()),
            source_root: context.source_root_display(),
            trust: ProviderSourceTrust::Fixture,
            fidelity,
            cursor,
            idempotency_key: Some(format!(
                "provider-source:{}:{}:{}",
                fixture.provider.as_str(),
                source_format,
                fixture.session.provider_session_id
            )),
            metadata: json!({
                "adapter": "provider_fixture_jsonl",
            }),
        },
        session: ProviderSessionEnvelope {
            provider_session_id: fixture.session.provider_session_id.clone(),
            parent_provider_session_id: fixture.session.parent_provider_session_id.clone(),
            root_provider_session_id: fixture.session.root_provider_session_id.clone(),
            external_agent_id: fixture.session.external_agent_id.clone(),
            agent_type: fixture.session.agent_type,
            role_hint: fixture.session.role_hint.clone(),
            is_primary: fixture.session.is_primary,
            status: fixture.session.status,
            started_at: fixture.session.started_at,
            ended_at: fixture.session.ended_at,
            cwd: fixture.session.cwd.clone(),
            fidelity,
            idempotency_key: Some(format!(
                "provider-session:{}:{}",
                fixture.provider.as_str(),
                fixture.session.provider_session_id
            )),
            artifacts: Vec::new(),
            metadata: fixture.session.metadata.clone(),
        },
        event: fixture.event.as_ref().map(|event| ProviderEventEnvelope {
            provider_event_index: event.provider_event_index,
            provider_event_hash: event.provider_event_hash.clone(),
            cursor: event.cursor.clone(),
            event_type: event.event_type,
            role: event.role,
            occurred_at: event.occurred_at,
            fidelity,
            idempotency_key: Some(format!(
                "provider-event:{}:{}:{}",
                fixture.provider.as_str(),
                fixture.session.provider_session_id,
                event.provider_event_index
            )),
            artifacts: Vec::new(),
            payload: event.payload.clone(),
            metadata: event.metadata.clone(),
        }),
    }
}
