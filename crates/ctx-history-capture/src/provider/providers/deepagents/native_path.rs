use std::{
    fs,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    compute_payload_hash, AgentType, CaptureProvider, CaptureSource, CaptureSourceDescriptor,
    CaptureSourceKind, ContentRef, Event, EventRole, EventType, Fidelity, ProviderSourceTrust,
    Session, SessionStatus, SyncCursor,
};
use ctx_history_store::{
    decode_native_path_committed_cursor, EventSearchBulkGuard, NativePathCursorSetClassification,
    NativePathCursorTransition, NativePathGroupAccounting, NativePathRetainedSourceEntities,
    NativePathSourceEntityFrontier, NativePathSourceEntityKind, NativePathSourceGenerationKey,
    ProviderEventHashAuthority, ProviderSourceLocatorObservation, ProviderSourceRouteRetirement,
    ProviderSourceRouteRetirementReason, Store, StoreError, NATIVE_PATH_MAX_RETAINED_PAGE_BYTES,
};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    complete_content::{
        attach_verified_content_locator, verified_content_profile, CompleteContentSourceFamily,
        VerifiedContentLocatorV1, VerifiedContentLocatorsV1, VerifiedContentRole,
        VERIFIED_CONTENT_LOCATORS_METADATA_KEY,
    },
    provider::{
        importer::{
            compact_provider_result_payload,
            provider_event_import_identity_with_exact_legacy_source, provider_import_session_uuid,
            provider_path_identity, provider_scoped_source_identity_key,
            provider_scoped_source_uuid, provider_session_uuid,
            provider_source_cursor_stream_for_path, provider_source_event_import_identity,
            provider_source_identity, provider_sync_metadata, timestamps, CertifiedProviderCursor,
            ProviderEventImportIdentity,
        },
        native_ingestion::{
            process_pro_replay_only, NativePageAccounting, NativeProOutputPage,
            NativeProReplayPage, NativeSafeFrontier, NativeSourceIdentity,
            NATIVE_INGESTION_PAGE_MAX_UNITS,
        },
        normalization::{
            provider_capped_json, provider_policy_body, provider_policy_event_text,
            provider_result_identifier_evidence, provider_result_outcome_evidence,
        },
        sqlite::{
            open_provider_sqlite_readonly, sqlite_schema_fingerprint, with_sqlite_read_snapshot,
            ProviderSqliteSourceSnapshot,
        },
    },
    CaptureError, CaptureWorkLimit, OutputAssociations, OutputNativeCoordinate,
    OutputObservationKind, OutputOutcome, OutputOutcomeMetadata, OutputSourceIdentity,
    OutputSourceLocator, ProOutputObservation, ProOutputProgress, ProOutputSink,
    ProOutputSinkError, ProOutputSourceDisposition, ProviderAdapterContext, ProviderImportFailure,
    ProviderImportOptions, ProviderImportSummary, ProviderImportTerminalOutcome,
    ProviderImportWorkResult, Result, DEEPAGENTS_SQLITE_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS,
};

use super::{
    complete_content::{deepagents_write_record_digest, DeepAgentsContentAddress},
    message::{deepagents_messages_from_blob, DeepAgentsMessage, DeepAgentsMessageRejection},
    source::{
        deepagents_checkpoint_time, deepagents_hydrate_write, deepagents_next_thread_candidate,
        deepagents_next_write_candidate, deepagents_source_snapshot, deepagents_thread_summary,
        deepagents_validate_schema, deepagents_write_candidate_at, DeepAgentsThread,
        DeepAgentsThreadSummary, DeepAgentsWriteCandidate, DeepAgentsWriteKey,
    },
    DEEPAGENTS_CONTENT_LOCATOR_KIND,
};

mod core;
mod lifecycle;
mod model;
mod output;
mod projection;
mod publication;
pub(crate) mod source_backed;

use core::*;
use lifecycle::*;
use model::*;
use output::*;
use projection::*;
use publication::*;

#[derive(Debug, Clone)]
pub(super) struct DeepAgentsParsedMessage {
    pub(super) offset: usize,
    pub(super) provider_event_index: u64,
    pub(super) message: DeepAgentsMessage,
}

pub(super) fn import_deepagents_sqlite_nativepath(
    path: &Path,
    store: &mut Store,
    mut context: ProviderAdapterContext,
    options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    if context.source_path.is_none() {
        context.source_path = Some(path.to_path_buf());
    }
    let database_path = absolute_path(path)?;
    if !database_path.exists() {
        return retire_missing_source(path, &database_path, store, &context, &options);
    }

    let canonical_database_path = fs::canonicalize(&database_path)?;
    let snapshot = deepagents_source_snapshot(&database_path)?;
    let conn = open_provider_sqlite_readonly(&database_path)?;
    if !snapshot.revalidate(&database_path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    deepagents_validate_schema(&conn, &database_path)?;
    let sqlite_user_version =
        conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))?;
    let schema_fingerprint = sqlite_schema_fingerprint(&conn)?;
    let source_revision = source_revision(&snapshot, &schema_fingerprint);
    let configured_source_root = context
        .source_root
        .clone()
        .or_else(|| context.source_path.clone())
        .unwrap_or_else(|| database_path.clone());
    let route_identity = provider_path_identity(&canonical_database_path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::DeepAgents,
        DEEPAGENTS_SQLITE_SOURCE_FORMAT,
        &route_identity,
    );
    let raw_source_path = canonical_database_path.display().to_string();
    let source_root = configured_source_root.display().to_string();
    let proposed_source_identity = provider_source_identity(
        CaptureProvider::DeepAgents,
        DEEPAGENTS_SQLITE_SOURCE_FORMAT,
        Some(&source_root),
        Some(&raw_source_path),
        None,
        &Value::Null,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Deep Agents NativePath source has no canonical identity",
    ))?;
    let stored = store.get_sync_cursor(None, &context.machine_id, &cursor_stream)?;
    let prior = decode_core_cursor_for_migration(stored.as_ref())?;
    let same_generation = prior.as_ref().is_some_and(|cursor| {
        cursor.route_identity == route_identity
            && cursor.source_revision == source_revision
            && cursor.schema_fingerprint == schema_fingerprint
            && !matches!(
                cursor.phase,
                DeepAgentsCorePhase::MissingStage { .. }
                    | DeepAgentsCorePhase::MissingRetire { .. }
                    | DeepAgentsCorePhase::MissingComplete
            )
    });
    let generation = if same_generation {
        prior.as_ref().map_or(0, |cursor| cursor.generation)
    } else {
        prior
            .as_ref()
            .map_or(0, |cursor| cursor.generation.saturating_add(1))
    };
    if generation == u64::MAX
        && prior
            .as_ref()
            .is_some_and(|cursor| cursor.generation == u64::MAX)
    {
        return Err(CaptureError::SystemInvariant(
            "Deep Agents source generation is exhausted",
        ));
    }
    let canonical_source_identity = prior
        .as_ref()
        .map(|cursor| cursor.canonical_source_identity.clone())
        .unwrap_or_else(|| proposed_source_identity.clone());
    let mut authority = DeepAgentsSourceAuthority {
        configured_source_root,
        database_path,
        canonical_database_path,
        route_identity,
        cursor_stream,
        proposed_source_identity,
        canonical_source_identity,
        source_revision,
        schema_fingerprint,
        sqlite_user_version,
    };

    if options.import_profile.is_replay_only() {
        require_complete_matching_core(store, &authority, &context)?;
        let mut summary = ProviderImportSummary::default();
        summary.set_terminal_outcome(ProviderImportTerminalOutcome::CoreCursorCommitted);
        if let Some(sink) = options.import_profile.sink() {
            if replay_outputs(&conn, &snapshot, &authority, &context, sink.as_ref())? {
                record_output_behind(&mut summary);
            }
        }
        return Ok(summary);
    }

    let cursor = prior
        .filter(|_| same_generation)
        .unwrap_or(DeepAgentsNativeCursor {
            version: DEEPAGENTS_NATIVE_CURSOR_VERSION,
            parser_revision: DEEPAGENTS_NATIVE_PARSER_REVISION.to_owned(),
            policy_revision: DEEPAGENTS_NATIVE_POLICY_REVISION.to_owned(),
            route_identity: authority.route_identity.clone(),
            canonical_source_identity: authority.canonical_source_identity.clone(),
            source_revision: authority.source_revision.clone(),
            schema_fingerprint: authority.schema_fingerprint.clone(),
            generation,
            generation_staged: false,
            accepted_sessions: 0,
            accepted_events: 0,
            rejected_records: 0,
            rejections: Vec::new(),
            phase: DeepAgentsCorePhase::Threads { after_rowid: None },
        });
    if cursor.is_complete() {
        let mut summary = ProviderImportSummary::default();
        summary.skipped_sessions = usize::try_from(cursor.accepted_sessions).unwrap_or(usize::MAX);
        summary.skipped_events = usize::try_from(cursor.accepted_events).unwrap_or(usize::MAX);
        summary.skipped = summary
            .skipped_sessions
            .saturating_add(summary.skipped_events);
        summary.failed = usize::try_from(cursor.rejected_records).unwrap_or(usize::MAX);
        summary.failures = cursor.rejections.clone();
        summary.accepted_content_records =
            usize::try_from(cursor.accepted_events).unwrap_or(usize::MAX);
        summary.set_terminal_outcome(ProviderImportTerminalOutcome::CoreCursorCommitted);
        summary.set_work_result(ProviderImportWorkResult::NoOp);
        if let Some(sink) = options.import_profile.sink() {
            if replay_outputs(&conn, &snapshot, &authority, &context, sink.as_ref())? {
                record_output_behind(&mut summary);
            }
        }
        return Ok(summary);
    }

    let mut summary = import_core(
        store,
        &conn,
        &snapshot,
        &mut authority,
        &context,
        &options,
        cursor,
    )?;
    if !summary.work_remaining {
        if let Some(sink) = options.import_profile.sink() {
            if replay_outputs(&conn, &snapshot, &authority, &context, sink.as_ref())? {
                record_output_behind(&mut summary);
            }
        }
    }
    if summary.work_result.is_none() {
        summary.set_work_result(ProviderImportWorkResult::NoOp);
    }
    Ok(summary)
}

#[derive(Debug, Clone)]
pub(super) struct DeepAgentsMessageIdentity {
    pub(super) provider_index: u64,
}

pub(super) fn deepagents_message_identity(
    thread_id: &str,
    message_id: &str,
) -> DeepAgentsMessageIdentity {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for component in [
        b"ctx-deepagents-message-v1".as_slice(),
        thread_id.as_bytes(),
        message_id.as_bytes(),
    ] {
        for byte in component {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(PRIME);
        }
        hash ^= 0xff;
        hash = hash.wrapping_mul(PRIME);
    }
    DeepAgentsMessageIdentity {
        provider_index: hash,
    }
}

#[derive(Debug)]
pub(crate) struct DeepAgentsNativeEvent {
    pub(crate) provider_event_index: u64,
    pub(crate) provider_event_hash: Option<String>,
    pub(crate) cursor: String,
    pub(crate) event_type: EventType,
    pub(crate) role: Option<EventRole>,
    pub(crate) occurred_at: DateTime<Utc>,
    pub(crate) payload: Value,
    pub(crate) metadata: Value,
}

pub(super) fn deepagents_native_event(
    key: &DeepAgentsWriteKey,
    parsed: &DeepAgentsParsedMessage,
    occurred_at: DateTime<Utc>,
    provider_event_hash: &str,
    provider_event_identity_index: Option<u64>,
    record_digest: Option<crate::complete_content::CompleteContentBodyDigest>,
) -> DeepAgentsNativeEvent {
    let event_type = if parsed.message.role == EventRole::Tool {
        EventType::ToolOutput
    } else {
        EventType::Message
    };
    let cursor = format!(
        "thread:{}:checkpoint:{}:task:{}:write:{}:message:{}",
        key.thread_id, key.checkpoint_id, key.task_id, key.idx, parsed.offset
    );
    let body = json!({
        "message_type": parsed.message.message_type,
        "message_class": parsed.message.message_class,
        "message_id": parsed.message.message_id,
        "tool_call_id": parsed.message.tool_call_id,
        "status": parsed.message.status,
        "exit_code": parsed.message.exit_code,
        "duration_ms": parsed.message.duration_ms,
        "timed_out": parsed.message.timed_out,
        "is_error": parsed.message.is_error,
        "success": parsed.message.success,
        "checkpoint_id": key.checkpoint_id,
        "task_id": key.task_id,
        "write_idx": key.idx,
        "message_offset": parsed.offset,
    });
    let retained_text = provider_policy_event_text(event_type, &parsed.message.text, &body);
    let retained_body = provider_policy_body(event_type, &body);
    let result_evidence =
        provider_result_identifier_evidence(event_type, &parsed.message.text, &body);
    let result_outcome = provider_result_outcome_evidence(event_type, &body);
    let mut event = DeepAgentsNativeEvent {
        provider_event_index: parsed.provider_event_index,
        provider_event_hash: Some(provider_event_hash.to_owned()),
        cursor,
        event_type,
        role: Some(parsed.message.role),
        occurred_at,
        payload: json!({
            "text": retained_text.text,
            "text_retention": retained_text.retention.as_json(),
            "result_evidence": result_evidence,
            "result_outcome": result_outcome,
            "source_format": DEEPAGENTS_SQLITE_SOURCE_FORMAT,
            "body": provider_capped_json(&retained_body, PROVIDER_MAX_PREVIEW_CHARS),
        }),
        metadata: json!({
            "source": DEEPAGENTS_SQLITE_SOURCE_FORMAT,
            "source_format": DEEPAGENTS_SQLITE_SOURCE_FORMAT,
            "checkpoint_id": key.checkpoint_id,
            "task_id": key.task_id,
            "write_idx": key.idx,
            "message_offset": parsed.offset,
            "message_type": parsed.message.message_type,
            "message_class": parsed.message.message_class,
            "message_id": parsed.message.message_id,
            "provider_event_identity_index": provider_event_identity_index,
            "privacy": "decoded from writes.messages only",
        }),
    };
    if event_type != EventType::ToolOutput {
        attach_native_message_content_locator(
            &mut event,
            key,
            parsed.offset,
            &parsed.message.text,
            record_digest,
        );
    }
    event
}
