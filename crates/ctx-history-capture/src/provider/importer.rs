use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    CaptureProvider, CaptureSource, CaptureSourceDescriptor, CaptureSourceKind, Confidence, Event,
    EventRole, EventType, Fidelity, FileTouched, ProviderCaptureEnvelope, ProviderCursorCheckpoint,
    ProviderCursorRange, ProviderEventEnvelope, ProviderSessionEnvelope, ProviderSourceEnvelope,
    ProviderSourceTrust, Session, SessionEdge, SessionEdgeType,
    PROVIDER_CAPTURE_ENVELOPE_MIN_SUPPORTED_SCHEMA_VERSION,
    PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
};
use ctx_history_store::{Store, StoreError};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{compute_payload_hash, stable_capture_uuid};

use crate::provider::file_touches::provider_file_touches_from_event;
use crate::{
    CaptureError, NormalizedProviderImportOptions, ProviderAdapterContext, ProviderCaptureAdapter,
    ProviderFileTouchedEnvelope, ProviderFixtureLine, ProviderImportFailure, ProviderImportSummary,
    ProviderNormalizationResult, Result,
};

mod batches;
mod commands;
mod cursors;
mod identity;
mod ids;

#[cfg(test)]
pub(crate) use batches::import_normalized_provider_captures_in_batches;
pub(crate) use batches::{resolve_pending_provider_edges_batched, ProviderImportTransaction};
pub(crate) use commands::{
    provider_command_run_from_event, validate_provider_event_for_import, ProviderCommandRunInput,
};
#[cfg(test)]
pub(crate) use cursors::provider_source_cursor_stream;
pub(crate) use cursors::{
    persist_provider_sync_cursor, provider_cursor_stream, provider_source_cursor_range,
    provider_sync_cursor,
};
pub(crate) use identity::{
    pi_existing_event_identity_by_entry_id, provider_event_exists, provider_event_import_identity,
    provider_file_touch_event_id, provider_file_touch_import_id, provider_session_exists_cached,
    ProviderEventImportIdentity,
};
pub(crate) use ids::{
    provider_edge_uuid, provider_scoped_source_identity_key, provider_scoped_source_uuid,
    provider_session_uuid, provider_source_edge_uuid, provider_source_identity,
    provider_source_root, provider_source_session_uuid, provider_sync_metadata, timestamps,
};

#[cfg(test)]
pub(crate) use identity::provider_source_event_import_identity;
#[cfg(test)]
pub(crate) use ids::provider_source_root_identity;
#[cfg(test)]
pub(crate) use ids::{
    provider_event_seq, provider_event_uuid, provider_file_touch_uuid, provider_source_event_seq,
    provider_source_event_uuid, provider_source_uuid,
};

pub(crate) struct NativeJsonlTreeImport<'a> {
    pub(crate) path: &'a Path,
    pub(crate) machine_id: String,
    pub(crate) source_path: Option<PathBuf>,
    pub(crate) source_root: Option<PathBuf>,
    pub(crate) imported_at: DateTime<Utc>,
    pub(crate) history_record_id: Option<Uuid>,
}

pub(crate) fn import_native_jsonl_tree<A: ProviderCaptureAdapter>(
    store: &mut Store,
    request: NativeJsonlTreeImport<'_>,
    adapter: A,
) -> Result<ProviderImportSummary> {
    let source_path = request
        .source_path
        .unwrap_or_else(|| request.path.to_path_buf());
    let normalization = adapter.normalize_path(
        request.path,
        &ProviderAdapterContext {
            machine_id: request.machine_id,
            source_path: Some(source_path),
            source_root: request.source_root,
            imported_at: request.imported_at,
        },
    )?;
    import_normalized_provider_captures(
        store,
        normalization,
        NormalizedProviderImportOptions {
            history_record_id: request.history_record_id,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: true,
        },
    )
}

pub fn import_normalized_provider_captures(
    store: &mut Store,
    normalization: ProviderNormalizationResult,
    options: NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    batches::import_normalized_provider_captures(store, normalization, options)
}

pub(crate) fn import_provider_capture_lines(
    store: &mut Store,
    options: NormalizedProviderImportOptions,
    summary: ProviderImportSummary,
    captures: Vec<(usize, ProviderCaptureEnvelope)>,
    files_touched: Vec<(usize, ProviderFileTouchedEnvelope)>,
) -> Result<ProviderImportSummary> {
    batches::import_provider_capture_lines(store, options, summary, captures, files_touched)
}

fn filter_provider_capture_lines_without_real_session_messages(
    summary: &mut ProviderImportSummary,
    captures: &mut Vec<(usize, ProviderCaptureEnvelope)>,
    files_touched: &mut Vec<(usize, ProviderFileTouchedEnvelope)>,
) {
    let native_session_keys = captures
        .iter()
        .filter_map(|(_, capture)| provider_capture_policy_session_key(capture))
        .collect::<HashSet<_>>();
    if native_session_keys.is_empty() {
        return;
    }

    let real_session_keys = captures
        .iter()
        .filter_map(|(_, capture)| {
            let key = provider_capture_policy_session_key(capture)?;
            capture
                .event
                .as_ref()
                .is_some_and(provider_event_is_real_conversation_message)
                .then_some(key)
        })
        .collect::<HashSet<_>>();
    let rejected_session_keys = native_session_keys
        .difference(&real_session_keys)
        .cloned()
        .collect::<HashSet<_>>();
    if rejected_session_keys.is_empty() {
        return;
    }

    summary.skipped_sessions += rejected_session_keys.len();
    captures.retain(|(_, capture)| {
        let Some(key) = provider_capture_policy_session_key(capture) else {
            return true;
        };
        if !rejected_session_keys.contains(&key) {
            return true;
        }
        summary.skipped += 1;
        if capture.event.is_some() {
            summary.skipped_events += 1;
        }
        false
    });
    files_touched.retain(|(_, file)| {
        let Some(key) = provider_file_touch_policy_session_key(file) else {
            return true;
        };
        if !rejected_session_keys.contains(&key) {
            return true;
        }
        summary.skipped += 1;
        false
    });

    if real_session_keys.is_empty() && summary.failed == 0 {
        summary.failed += 1;
        summary.failures.push(ProviderImportFailure {
            line: 0,
            error: "provider source contained no real conversation message".to_owned(),
        });
    }
}

fn provider_capture_policy_session_key(capture: &ProviderCaptureEnvelope) -> Option<String> {
    provider_policy_session_key(
        capture.provider,
        &capture.source.trust,
        &capture.source.source_format,
        &capture.session.provider_session_id,
        capture.source.raw_source_path.as_deref(),
    )
}

fn provider_file_touch_policy_session_key(file: &ProviderFileTouchedEnvelope) -> Option<String> {
    provider_policy_session_key(
        file.provider,
        &ProviderSourceTrust::ProviderNative,
        &file.source_format,
        &file.provider_session_id,
        file.raw_source_path.as_deref(),
    )
}

fn provider_policy_session_key(
    provider: CaptureProvider,
    trust: &ProviderSourceTrust,
    source_format: &str,
    provider_session_id: &str,
    raw_source_path: Option<&str>,
) -> Option<String> {
    if provider == CaptureProvider::Custom
        || !matches!(
            trust,
            ProviderSourceTrust::ProviderNative | ProviderSourceTrust::ProviderExport
        )
    {
        return None;
    }
    Some(format!(
        "{}\0{}\0{}\0{}",
        provider.as_str(),
        source_format,
        provider_session_id,
        raw_source_path.unwrap_or_default()
    ))
}

fn provider_capture_lines_have_real_message(captures: &[(usize, ProviderCaptureEnvelope)]) -> bool {
    captures
        .iter()
        .filter(|(_, capture)| capture.provider != CaptureProvider::Custom)
        .all(|(_, capture)| {
            !matches!(
                capture.source.trust,
                ProviderSourceTrust::ProviderNative | ProviderSourceTrust::ProviderExport
            )
        })
        || captures.iter().any(|(_, capture)| {
            capture.provider != CaptureProvider::Custom
                && matches!(
                    capture.source.trust,
                    ProviderSourceTrust::ProviderNative | ProviderSourceTrust::ProviderExport
                )
                && capture
                    .event
                    .as_ref()
                    .is_some_and(provider_event_is_real_conversation_message)
        })
}

fn provider_event_is_real_conversation_message(event: &ProviderEventEnvelope) -> bool {
    event.event_type == EventType::Message
        && matches!(
            event.role,
            Some(EventRole::User | EventRole::Assistant | EventRole::System)
        )
        && provider_event_payload_has_text(&event.payload)
}

fn provider_event_payload_has_text(payload: &Value) -> bool {
    payload
        .get("text")
        .and_then(Value::as_str)
        .or_else(|| {
            payload
                .get("body")
                .and_then(|body| body.get("text"))
                .and_then(Value::as_str)
        })
        .is_some_and(|text| !text.trim().is_empty())
}

pub(crate) fn import_provider_file_touched_line(
    store: &mut Store,
    file: &ProviderFileTouchedEnvelope,
    options: &NormalizedProviderImportOptions,
) -> Result<()> {
    let source_id = provider_scoped_source_uuid(
        file.provider,
        &file.provider_session_id,
        &file.source_format,
        file.raw_source_path.as_deref(),
    );
    let source_root =
        provider_source_root(file.source_root.as_deref(), file.raw_source_path.as_deref());
    let source_identity = provider_source_identity(
        file.provider,
        &file.source_format,
        file.source_root.as_deref(),
        file.raw_source_path.as_deref(),
        None,
        &file.metadata,
    );
    let inferred_session_id = provider_import_session_uuid(
        store,
        file.provider,
        &file.provider_session_id,
        source_id,
        source_identity.as_deref(),
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
    pub(crate) processed_sessions: BTreeSet<Uuid>,
    pub(crate) imported_edges: BTreeSet<Uuid>,
    pub(crate) processed_edges: BTreeSet<Uuid>,
    pub(crate) session_exists: BTreeMap<Uuid, bool>,
    pub(crate) pi_event_identities_by_entry_id:
        BTreeMap<Uuid, BTreeMap<String, ProviderEventImportIdentity>>,
    pub(crate) pending_edges: BTreeMap<Uuid, PendingProviderEdge>,
}

#[derive(Clone)]
pub(crate) struct PendingProviderEdge {
    pub(crate) provider_session_id: String,
    pub(crate) parent_provider_session_id: Option<String>,
    pub(crate) session_id: Uuid,
    pub(crate) parent_session_id: Uuid,
    pub(crate) root_session_id: Option<Uuid>,
    pub(crate) source_id: Uuid,
    pub(crate) source_format: String,
    pub(crate) imported_at: DateTime<Utc>,
    pub(crate) fidelity: Fidelity,
    pub(crate) line_number: usize,
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

fn legacy_session_matches_source(
    store: &Store,
    session: &Session,
    source_id: Uuid,
    source_identity: &str,
) -> Result<bool> {
    let Some(existing_source_id) = session.capture_source_id else {
        return Ok(false);
    };
    if existing_source_id == source_id {
        return Ok(true);
    }
    match store.get_capture_source(existing_source_id) {
        Ok(source) => {
            if source.descriptor.source_identity.as_deref() == Some(source_identity) {
                return Ok(true);
            }
            let existing_source_format = source
                .descriptor
                .source_format
                .as_deref()
                .or_else(|| source.sync.metadata["source_format"].as_str());
            let Some(existing_source_format) = existing_source_format else {
                return Ok(false);
            };
            let source_metadata = source
                .sync
                .metadata
                .get("source_metadata")
                .unwrap_or(&source.sync.metadata);
            let source_idempotency_key = source.sync.metadata["source_idempotency_key"].as_str();
            Ok(provider_source_identity(
                source.descriptor.provider,
                existing_source_format,
                source.descriptor.source_root.as_deref(),
                source.descriptor.raw_source_path.as_deref(),
                source_idempotency_key,
                source_metadata,
            )
            .as_deref()
                == Some(source_identity))
        }
        Err(StoreError::NotFound(_)) => Ok(false),
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

pub(crate) fn import_provider_capture_line(
    store: &mut Store,
    capture: &ProviderCaptureEnvelope,
    options: &NormalizedProviderImportOptions,
    line_number: usize,
    caches: &mut ProviderImportCaches,
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
    let source_id = stable_capture_uuid(&source_identity_key, "source");
    let source_root = provider_source_root(
        source.source_root.as_deref(),
        source.raw_source_path.as_deref(),
    );
    let source_identity = provider_source_identity(
        provider,
        &source.source_format,
        source.source_root.as_deref(),
        source.raw_source_path.as_deref(),
        source.idempotency_key.as_deref(),
        &source.metadata,
    );
    let session_id = provider_import_session_uuid(
        store,
        provider,
        &session.provider_session_id,
        source_id,
        source_identity.as_deref(),
    )?;
    let source_cursor = provider_source_cursor_range(capture);
    let requested_parent_session_id = session
        .parent_provider_session_id
        .as_ref()
        .map(|id| {
            provider_import_session_uuid(store, provider, id, source_id, source_identity.as_deref())
        })
        .transpose()?;
    let parent_session_id = match requested_parent_session_id {
        Some(parent_id)
            if provider_session_exists_cached(store, parent_id, &mut caches.session_exists)? =>
        {
            Some(parent_id)
        }
        _ => None,
    };
    let requested_root_session_id = session
        .root_provider_session_id
        .as_ref()
        .map(|id| {
            provider_import_session_uuid(store, provider, id, source_id, source_identity.as_deref())
        })
        .transpose()?
        .or_else(|| requested_parent_session_id.map(|_| session_id));
    let root_session_id = match requested_root_session_id {
        Some(root_id)
            if root_id == session_id
                || provider_session_exists_cached(store, root_id, &mut caches.session_exists)? =>
        {
            Some(root_id)
        }
        _ => None,
    };
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

    let process_session = caches.processed_sessions.insert(session_id);
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
    if process_session {
        store.upsert_session(&normalized_session)?;
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
            source_identity.as_deref(),
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
    } else if requested_parent_session_id.is_some() {
        let edge_id = provider_import_edge_uuid(
            provider,
            &session.provider_session_id,
            source_identity.as_deref(),
            session_id,
            "parent_child",
        );
        if let Some(parent_session_id) = requested_parent_session_id {
            caches
                .pending_edges
                .entry(edge_id)
                .or_insert_with(|| PendingProviderEdge {
                    provider_session_id: session.provider_session_id.clone(),
                    parent_provider_session_id: session.parent_provider_session_id.clone(),
                    session_id,
                    parent_session_id,
                    root_session_id: requested_root_session_id,
                    source_id,
                    source_format: source.source_format.clone(),
                    imported_at,
                    fidelity: session.fidelity,
                    line_number,
                });
        }
    }

    if let Some(event) = &capture.event {
        let payload = event.payload.clone();
        let event_metadata = event.metadata.clone();
        let event_hash = event
            .provider_event_hash
            .clone()
            .unwrap_or(compute_payload_hash(&payload)?);
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
        let event_identity = match pi_existing_event_identity_by_entry_id(
            store,
            provider,
            session_id,
            pi_entry_id,
            caches,
        )? {
            Some(identity) => identity,
            None => provider_event_import_identity(
                store,
                provider,
                &session.provider_session_id,
                source_id,
                provider_event_identity_index,
                event.provider_event_index,
                &event_hash,
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
                "body": payload,
            }),
            payload_blob_id: None,
            dedupe_key: Some(event_identity.dedupe_key.clone()),
            sync: provider_sync_metadata(
                event.fidelity,
                json!({
                    "provider_session_id": session.provider_session_id,
                    "provider_event_index": event.provider_event_index,
                    "provider_event_hash": event_hash,
                    "cursor": event.cursor,
                    "source_format": source.source_format,
                    "source_trust": source.trust,
                    "fixture_line": line_number,
                    "imported_at": imported_at,
                    "event_idempotency_key": event.idempotency_key,
                    "metadata": event_metadata,
                }),
            ),
        };
        let was_present = if options.fast_event_inserts {
            if let Some(run) = &command_run {
                store.insert_run_if_absent(run)?;
            }
            !store.insert_event_if_absent(&normalized_event)?
        } else {
            let was_present = provider_event_exists(store, &event_identity.dedupe_key)?;
            if let Some(run) = &command_run {
                store.upsert_run(run)?;
            }
            match store.upsert_event(&normalized_event) {
                Ok(_) => {}
                Err(StoreError::Sql(rusqlite::Error::QueryReturnedNoRows)) => {}
                Err(StoreError::ProviderEventConflict { .. }) => {
                    summary.skipped_events += 1;
                    summary.skipped += 1;
                    summary.accepted_content_records += 1;
                    return Ok(summary);
                }
                Err(err) => return Err(CaptureError::Store(err)),
            }
            was_present
        };
        if was_present {
            summary.skipped_events += 1;
            summary.skipped += 1;
        } else {
            summary.imported_events += 1;
            summary.imported += 1;
        }
    }

    if capture.event.is_some() {
        summary.accepted_content_records += 1;
    }

    Ok(summary)
}

fn resolve_pending_provider_edge(
    store: &mut Store,
    summary: &mut ProviderImportSummary,
    caches: &mut ProviderImportCaches,
    edge_id: Uuid,
    edge: PendingProviderEdge,
) -> Result<()> {
    if caches.processed_edges.contains(&edge_id) {
        update_session_parent_if_needed(store, &edge, caches)?;
        return Ok(());
    }
    if !provider_session_exists_cached(store, edge.parent_session_id, &mut caches.session_exists)? {
        summary.skipped_edges += 1;
        summary.skipped += 1;
        return Ok(());
    }
    let root_session_id = resolve_pending_root_session_id(store, &edge, caches)?;
    update_session_parent(store, &edge, root_session_id)?;
    caches.session_exists.insert(edge.session_id, true);

    let was_present = store.session_edge_exists(edge_id)?;
    let session_edge = SessionEdge {
        id: edge_id,
        from_session_id: edge.parent_session_id,
        to_session_id: edge.session_id,
        edge_type: SessionEdgeType::ParentChild,
        confidence: Confidence::Explicit,
        source_id: Some(edge.source_id),
        timestamps: timestamps(edge.imported_at),
        sync: provider_sync_metadata(
            edge.fidelity,
            json!({
                "provider_session_id": edge.provider_session_id,
                "parent_provider_session_id": edge.parent_provider_session_id,
                "source_format": edge.source_format,
                "fixture_line": edge.line_number,
                "imported_at": edge.imported_at,
                "deferred_edge_resolution": true,
            }),
        ),
    };
    store.upsert_session_edge(&session_edge)?;
    caches.processed_edges.insert(edge_id);
    if !was_present && caches.imported_edges.insert(edge_id) {
        summary.imported_edges += 1;
        summary.imported += 1;
    } else {
        summary.skipped_edges += 1;
        summary.skipped += 1;
    }
    Ok(())
}

pub(crate) fn resolve_pending_root_session_id(
    store: &Store,
    edge: &PendingProviderEdge,
    caches: &mut ProviderImportCaches,
) -> Result<Option<Uuid>> {
    match edge.root_session_id {
        Some(root_id)
            if root_id == edge.session_id
                || provider_session_exists_cached(store, root_id, &mut caches.session_exists)? =>
        {
            Ok(Some(root_id))
        }
        Some(_) | None => Ok(Some(edge.parent_session_id)),
    }
}

pub(crate) fn update_session_parent_if_needed(
    store: &mut Store,
    edge: &PendingProviderEdge,
    caches: &mut ProviderImportCaches,
) -> Result<()> {
    let root_session_id = resolve_pending_root_session_id(store, edge, caches)?;
    update_session_parent(store, edge, root_session_id)
}

pub(crate) fn update_session_parent(
    store: &mut Store,
    edge: &PendingProviderEdge,
    root_session_id: Option<Uuid>,
) -> Result<()> {
    let mut session = store.get_session(edge.session_id)?;
    if session.parent_session_id == Some(edge.parent_session_id)
        && session.root_session_id == root_session_id
    {
        return Ok(());
    }
    session.parent_session_id = Some(edge.parent_session_id);
    session.root_session_id = root_session_id;
    session.timestamps.updated_at = edge.imported_at;
    store.upsert_session(&session)?;
    Ok(())
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
