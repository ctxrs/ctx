use std::{fs, path::Path};

#[cfg(test)]
use ctx_history_core::{new_id, EntityTimestamps, SyncCursor};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, Event, Fidelity, ProviderEventEnvelope, SessionStatus,
};
use ctx_history_store::{Store, StoreError};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
#[cfg(test)]
use sha2::{Digest, Sha256};

use crate::captured_batch::ProviderRecordKind;
use crate::common::io::ensure_provider_path_parents_are_not_symlinks;
use crate::provider::file_touches::PROVIDER_FILE_TOUCH_LIMIT_REJECTION;
use crate::provider::importer::{
    captured_batch_cursor_stream, drain_captured_batches, provider_scoped_source_uuid,
    CapturedBatchCursorMode, CapturedSourceAdmission, CertifiedProviderCursor,
};
#[cfg(test)]
use crate::provider::importer::{
    provider_path_identity, provider_source_cursor_stream_for_path, BoundedParserCheckpoint,
};
use crate::{
    fnv1a64, CaptureError, NormalizedProviderImportOptions, ProviderAdapterContext,
    ProviderImportFailure, ProviderImportSummary, Result, OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
};

mod event;
mod projection;
mod source;

#[cfg(test)]
mod tests;

pub(crate) use event::{decode_openhands_event, decode_openhands_event_value};
#[cfg(test)]
use projection::with_openhands_post_touch_failure;
use projection::{openhands_provider_event_with_identity, OpenHandsCapturedBatchProjector};
#[cfg(test)]
pub(crate) use source::count_openhands_source_file_opens;
#[cfg(test)]
use source::OpenHandsFrozenFile;
use source::{
    capture_openhands_event_batch, decode_openhands_position, openhands_captured_error,
    openhands_checked_path_text, openhands_conversation_id_from_path, openhands_json_path_is_event,
    openhands_line_number, openhands_missing_event_files, openhands_position,
    visit_openhands_event_paths, OpenHandsEventSource,
};

const OPENHANDS_CAPTURE_REVISION: u32 = 2;
const OPENHANDS_POLICY_REVISION: u32 = 5;
const OPENHANDS_RECORD_KIND: &str = "openhands-event-json-v1";
const OPENHANDS_POSITION_KIND: &str = "openhands-event-file-v1";
const OPENHANDS_LOCATOR_KIND: &str = "openhands-event-path-v1";
const OPENHANDS_INVENTORY_PAGE_RECORDS: usize = 64;
const OPENHANDS_INVENTORY_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(5);
const OPENHANDS_MAX_PATH_BYTES: usize = 7 * 1024;
const OPENHANDS_MAX_DERIVED_TEXT_BYTES: usize = 16 * 1024;
const OPENHANDS_MAX_FAILURE_BYTES: usize = 4 * 1024;
const OPENHANDS_CAPTURED_BATCH_PROJECTION_MARKER: &str = "openhands-captured-batch-v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct OpenHandsEventIdentity {
    provider_event_index: u64,
    provider_event_identity_index: u64,
    canonical_path_hash: u64,
    legacy_provider_event_index_candidate: Option<u64>,
}

impl OpenHandsEventIdentity {
    fn for_path(path: &Path, canonical_path_identity: &str) -> Self {
        let canonical_path_hash = fnv1a64(canonical_path_identity.as_bytes());
        // The legacy normalizer assigned timestamp-sorted corpus ordinals. A
        // file-local import cannot prove that mapping. The public index also
        // uses the path hash because the importer probes it as a legacy alias.
        Self {
            provider_event_index: canonical_path_hash,
            provider_event_identity_index: canonical_path_hash,
            canonical_path_hash,
            legacy_provider_event_index_candidate: openhands_legacy_filename_index_candidate(path),
        }
    }
}

fn openhands_legacy_filename_index_candidate(path: &Path) -> Option<u64> {
    let stem = path.file_stem()?.to_str()?;
    let (ordinal, suffix) = stem.split_once('-')?;
    if ordinal.is_empty() || suffix.is_empty() || !ordinal.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    ordinal
        .parse::<u64>()
        .ok()
        .and_then(|ordinal| ordinal.checked_sub(1))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenHandsParserCheckpoint {
    next_position: u64,
    accepted_events: u64,
    accepted_file_touches: u64,
    rejection: Option<ProviderImportFailure>,
}

#[derive(Clone, Debug, PartialEq)]
enum OpenHandsProjectionMode {
    Full,
    ExistingStableNoop,
    ExistingStableUpgrade,
    ExistingStableRepair,
    LegacyUpgrade {
        provider_event_index: u64,
        occurred_at: DateTime<Utc>,
        session: OpenHandsLegacySessionSnapshot,
    },
}

#[derive(Clone, Debug, PartialEq)]
struct OpenHandsLegacySessionSnapshot {
    external_agent_id: Option<String>,
    agent_type: AgentType,
    role_hint: Option<String>,
    is_primary: bool,
    status: SessionStatus,
    fidelity: Fidelity,
    metadata: Value,
}

fn openhands_checkpoint_matches_position(
    checkpoint: &OpenHandsParserCheckpoint,
    position: u64,
    event_path: &Path,
) -> bool {
    if checkpoint.next_position != position {
        return false;
    }
    let Ok(touch_limit) =
        u64::try_from(crate::provider::file_touches::MAX_PROVIDER_FILE_TOUCHES_PER_EVENT)
    else {
        return false;
    };
    let rejection_is_bounded = |failure: &ProviderImportFailure| {
        failure.line == openhands_line_number(event_path)
            && !failure.error.is_empty()
            && failure.error.len() <= OPENHANDS_MAX_FAILURE_BYTES
    };

    match (
        position,
        checkpoint.accepted_events,
        checkpoint.accepted_file_touches,
        checkpoint.rejection.as_ref(),
    ) {
        (0, 0, 0, None) => true,
        (1, 0, 0, Some(failure)) => rejection_is_bounded(failure),
        (1, 1, accepted_file_touches, None) => accepted_file_touches <= touch_limit,
        (1, 1, accepted_file_touches, Some(failure)) => {
            accepted_file_touches == touch_limit
                && rejection_is_bounded(failure)
                && failure.error == PROVIDER_FILE_TOUCH_LIMIT_REJECTION
        }
        _ => false,
    }
}

pub(crate) fn import_openhands_file_events_batched(
    path: &Path,
    store: &mut Store,
    mut context: ProviderAdapterContext,
    import_options: NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let metadata = fs::symlink_metadata(path)?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "symlinked provider transcript roots are rejected",
        });
    }
    ensure_provider_path_parents_are_not_symlinks(path)?;
    if context.source_path.is_none() {
        context.source_path = Some(path.to_path_buf());
    }
    if file_type.is_file() {
        if !openhands_json_path_is_event(path) {
            return Err(openhands_missing_event_files(path));
        }
        return import_openhands_event_file_batched(path, store, &context, &import_options);
    }
    if !file_type.is_dir() {
        return Err(openhands_missing_event_files(path));
    }

    let mut merged = ProviderImportSummary::default();
    let mut source_count = 0_u64;
    visit_openhands_event_paths(path, &mut |event_path| {
        source_count = source_count
            .checked_add(1)
            .ok_or(CaptureError::SystemInvariant(
                "OpenHands event-file count overflowed",
            ))?;
        merged.merge(import_openhands_event_file_batched(
            event_path,
            store,
            &context,
            &import_options,
        )?);
        Ok(())
    })?;
    if source_count == 0 {
        return Err(openhands_missing_event_files(path));
    }
    Ok(merged)
}

fn openhands_projection_mode(
    store: &Store,
    canonical_path: &Path,
    conversation_dir: &Path,
    session_id: &str,
    identity: OpenHandsEventIdentity,
    raw_bytes: Option<&[u8]>,
    current_projection_published: bool,
) -> Result<OpenHandsProjectionMode> {
    let Some(decoded) = raw_bytes.and_then(|bytes| {
        let decoded = decode_openhands_event(canonical_path, bytes).ok()?;
        if openhands_conversation_id_from_path(canonical_path).as_deref() != Some(session_id) {
            return None;
        }
        Some(decoded)
    }) else {
        return Ok(OpenHandsProjectionMode::Full);
    };
    let event_hash = decoded.event_id().to_owned();
    let timestamp = decoded.timestamp();

    let canonical_path_text = openhands_checked_path_text(canonical_path)?;
    let stable_source_id = provider_scoped_source_uuid(
        CaptureProvider::OpenHands,
        session_id,
        OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
        Some(&canonical_path_text),
    );
    let stable_dedupe_key = Store::provider_source_event_dedupe_key(
        stable_source_id,
        identity.provider_event_identity_index,
        &event_hash,
    );
    if let Some(stored_event) =
        openhands_stored_event_for_path(store, &stable_dedupe_key, &canonical_path_text)?
    {
        if current_projection_published {
            return Ok(OpenHandsProjectionMode::ExistingStableNoop);
        }
        let incoming_event = openhands_provider_event_with_identity(
            session_id,
            canonical_path,
            &decoded,
            timestamp,
            identity,
            None,
        );
        if !openhands_stored_event_matches_projection(&stored_event, &incoming_event) {
            return Ok(OpenHandsProjectionMode::ExistingStableNoop);
        }
        return if openhands_stored_event_has_captured_batch_marker(store, &stored_event)? {
            Ok(OpenHandsProjectionMode::ExistingStableRepair)
        } else {
            Ok(OpenHandsProjectionMode::ExistingStableUpgrade)
        };
    }

    let Some(provider_event_index) = identity.legacy_provider_event_index_candidate else {
        return Ok(OpenHandsProjectionMode::Full);
    };
    let conversation_dir_text = openhands_checked_path_text(conversation_dir)?;
    let legacy_source_id = provider_scoped_source_uuid(
        CaptureProvider::OpenHands,
        session_id,
        OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
        Some(&conversation_dir_text),
    );
    let legacy_dedupe_key = Store::provider_source_event_dedupe_key(
        legacy_source_id,
        provider_event_index,
        &event_hash,
    );
    if let Some(stored_event) =
        openhands_stored_event_for_path(store, &legacy_dedupe_key, &canonical_path_text)?
    {
        let Some(session) = openhands_legacy_session_snapshot(store, &stored_event)? else {
            return Ok(OpenHandsProjectionMode::Full);
        };
        Ok(OpenHandsProjectionMode::LegacyUpgrade {
            provider_event_index,
            occurred_at: stored_event.occurred_at,
            session,
        })
    } else {
        Ok(OpenHandsProjectionMode::Full)
    }
}

fn openhands_stored_event_has_captured_batch_marker(store: &Store, event: &Event) -> Result<bool> {
    let Some(source_id) = event.capture_source_id else {
        return Ok(false);
    };
    let source = match store.get_capture_source(source_id) {
        Ok(source) => source,
        Err(StoreError::NotFound(_)) => return Ok(false),
        Err(error) => return Err(CaptureError::Store(error)),
    };
    Ok(source
        .sync
        .metadata
        .pointer("/source_metadata/captured_batch_projection")
        .and_then(Value::as_str)
        == Some(OPENHANDS_CAPTURED_BATCH_PROJECTION_MARKER))
}

fn openhands_legacy_session_snapshot(
    store: &Store,
    event: &Event,
) -> Result<Option<OpenHandsLegacySessionSnapshot>> {
    let Some(session_id) = event.session_id else {
        return Ok(None);
    };
    let session = match store.get_session(session_id) {
        Ok(session) => session,
        Err(StoreError::NotFound(_)) => return Ok(None),
        Err(error) => return Err(CaptureError::Store(error)),
    };
    Ok(Some(OpenHandsLegacySessionSnapshot {
        external_agent_id: session.external_agent_id,
        agent_type: session.agent_type,
        role_hint: session.role_hint,
        is_primary: session.is_primary,
        status: session.status,
        fidelity: session.sync.fidelity,
        metadata: session
            .sync
            .metadata
            .get("metadata")
            .cloned()
            .unwrap_or(Value::Null),
    }))
}

fn openhands_stored_event_for_path(
    store: &Store,
    dedupe_key: &str,
    canonical_path: &str,
) -> Result<Option<Event>> {
    let event_id = match store.event_id_by_dedupe_key(dedupe_key) {
        Ok(event_id) => event_id,
        Err(StoreError::Sql(rusqlite::Error::QueryReturnedNoRows)) => return Ok(None),
        Err(error) => return Err(CaptureError::Store(error)),
    };
    let event = store.get_event(event_id)?;
    if event
        .sync
        .metadata
        .pointer("/metadata/event_path")
        .and_then(Value::as_str)
        != Some(canonical_path)
    {
        return Ok(None);
    }
    Ok(Some(event))
}

fn openhands_stored_event_matches_projection(
    stored: &Event,
    incoming: &ProviderEventEnvelope,
) -> bool {
    stored.event_type == incoming.event_type
        && stored.role == incoming.role
        && stored.occurred_at == incoming.occurred_at
        && stored
            .payload
            .get("provider_event_index")
            .and_then(Value::as_u64)
            == Some(incoming.provider_event_index)
        && stored
            .payload
            .get("provider_event_hash")
            .and_then(Value::as_str)
            == incoming.provider_event_hash.as_deref()
        && stored.payload.get("cursor").and_then(Value::as_str) == incoming.cursor.as_deref()
        && stored.payload.get("artifacts") == Some(&json!([]))
        && stored.payload.get("body") == Some(&incoming.payload)
}

fn import_openhands_event_file_batched(
    path: &Path,
    store: &mut Store,
    context: &ProviderAdapterContext,
    import_options: &NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let OpenHandsEventSource {
        canonical_path,
        canonical_path_text,
        conversation_dir,
        session_id,
        frozen,
        raw_bytes,
        path_identity,
        observation: source,
    } = OpenHandsEventSource::observe(path, import_options.inventory_observation_token.as_deref())?;
    let identity = OpenHandsEventIdentity::for_path(&canonical_path, &path_identity);
    let file_context = ProviderAdapterContext {
        machine_id: context.machine_id.clone(),
        source_path: Some(canonical_path.clone()),
        source_root: context
            .source_root
            .clone()
            .or_else(|| context.source_path.clone()),
        imported_at: context.imported_at,
    };
    let stream = captured_batch_cursor_stream(&source);
    let expected_store_cursor = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let current_projection_published = expected_store_cursor
        .as_ref()
        .map(|cursor| CertifiedProviderCursor::decode_if_certified(&cursor.cursor))
        .transpose()?
        .flatten()
        .is_some_and(|cursor| {
            let position = decode_openhands_position(cursor.native_position()).ok();
            let checkpoint = cursor
                .parser_checkpoint()
                .deserialize::<OpenHandsParserCheckpoint>()
                .ok();
            cursor.matches_revisions(
                source.source_revision(),
                source.capture_revision(),
                source.policy_revision(),
            ) && position == Some(1)
                && checkpoint.as_ref().is_some_and(|checkpoint| {
                    checkpoint.accepted_events == 1
                        && openhands_checkpoint_matches_position(checkpoint, 1, &canonical_path)
                })
        });
    let projection_mode = openhands_projection_mode(
        store,
        &canonical_path,
        &conversation_dir,
        &session_id,
        identity,
        raw_bytes.as_deref(),
        current_projection_published,
    )?;
    let initial_position = openhands_position(0)?;
    let mut cursor_mode = CapturedBatchCursorMode::Resume;
    let mut resumed_projector = None;

    if let Some(stored_cursor) = expected_store_cursor.as_ref() {
        match CertifiedProviderCursor::decode_if_certified(&stored_cursor.cursor)? {
            Some(certified)
                if certified.matches_revisions(
                    source.source_revision(),
                    source.capture_revision(),
                    source.policy_revision(),
                ) =>
            {
                let position = decode_openhands_position(certified.native_position())?;
                let projector = OpenHandsCapturedBatchProjector::resume(
                    file_context.clone(),
                    canonical_path.clone(),
                    conversation_dir.clone(),
                    session_id.clone(),
                    identity,
                    projection_mode.clone(),
                    &certified,
                )?;
                if position == 1 {
                    if !frozen.revalidate(&canonical_path)? {
                        return Err(CaptureError::SourceChangedDuringCapture);
                    }
                    return projector.replay_summary();
                }
                if position != 0 {
                    return Err(CaptureError::InvalidPayload(
                        "OpenHands cursor is past its event-file boundary".to_owned(),
                    ));
                }
                resumed_projector = Some(projector);
            }
            Some(_) => cursor_mode = CapturedBatchCursorMode::ResetChangedSource,
            None => cursor_mode = CapturedBatchCursorMode::ReplaceLegacyCursor,
        }
    }

    let mut projector = resumed_projector.unwrap_or_else(|| {
        OpenHandsCapturedBatchProjector::fresh(
            file_context.clone(),
            canonical_path.clone(),
            conversation_dir,
            session_id,
            identity,
            projection_mode,
        )
    });
    let admission = CapturedSourceAdmission::file_for_context(&source, &file_context)?;
    let record_kind =
        ProviderRecordKind::new(OPENHANDS_RECORD_KIND).map_err(openhands_captured_error)?;
    let mut pending_bytes = Some(raw_bytes);
    drain_captured_batches(
        store,
        &admission,
        import_options.clone(),
        &context.machine_id,
        context.imported_at,
        expected_store_cursor,
        &initial_position,
        cursor_mode,
        source.cursor_stream(),
        &mut projector,
        || {
            let Some(raw_bytes) = pending_bytes.take() else {
                return Ok(None);
            };
            capture_openhands_event_batch(
                &canonical_path,
                &canonical_path_text,
                &frozen,
                raw_bytes,
                source.clone(),
                record_kind.clone(),
            )
            .map(Some)
        },
        || frozen.revalidate(&canonical_path),
    )
}

fn openhands_bounded_derived_text(value: String, field: &str) -> Result<String> {
    if value.len() > OPENHANDS_MAX_DERIVED_TEXT_BYTES {
        return Err(CaptureError::InvalidPayload(format!(
            "OpenHands {field} exceeds {OPENHANDS_MAX_DERIVED_TEXT_BYTES} bytes"
        )));
    }
    Ok(value)
}

#[cfg(test)]
pub(crate) fn seed_c213_openhands_terminal_cursor(
    store: &Store,
    path: &Path,
    machine_id: &str,
    observed_at: DateTime<Utc>,
) -> Result<()> {
    let canonical_path = fs::canonicalize(path)?;
    let frozen = OpenHandsFrozenFile::read(&canonical_path)?;
    let raw_bytes = source::read_openhands_frozen_bytes(&canonical_path, &frozen)?;
    let content_hash: [u8; 32] = Sha256::digest(&raw_bytes).into();
    let path_identity = provider_path_identity(&canonical_path)?;
    let stream = provider_source_cursor_stream_for_path(
        CaptureProvider::OpenHands,
        OPENHANDS_FILE_EVENTS_SOURCE_FORMAT,
        &path_identity,
    );
    let cursor = CertifiedProviderCursor::new(
        frozen.source_revision(Some(&content_hash)),
        2,
        1,
        openhands_position(1)?,
        BoundedParserCheckpoint::from_serializable(&OpenHandsParserCheckpoint {
            next_position: 1,
            accepted_events: 1,
            accepted_file_touches: 1,
            rejection: None,
        })?,
    )?;
    store.upsert_sync_cursor(&SyncCursor {
        id: new_id(),
        team_id: None,
        device_id: machine_id.to_owned(),
        stream,
        cursor: cursor.encode()?,
        last_synced_at: Some(observed_at),
        timestamps: EntityTimestamps {
            created_at: observed_at,
            updated_at: observed_at,
        },
    })?;
    Ok(())
}
