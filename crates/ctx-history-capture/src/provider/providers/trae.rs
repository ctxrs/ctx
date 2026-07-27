use std::{fs, path::Path};

use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, ProviderEventEnvelope};
use ctx_history_store::Store;
use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::captured_batch::sqlite_logical_rows::SqliteLogicalRowBatchProducer;
use crate::captured_batch::{CapturedBatch, NativeLocator, SourceObservation};
use crate::provider::importer::{
    captured_batch_cursor_stream, drain_captured_batches, provider_path_identity,
    provider_source_cursor_stream_for_path, CapturedBatchCursorMode, CapturedSourceAdmission,
    CertifiedProviderCursor,
};
use crate::provider::sqlite::open_provider_sqlite_readonly;
use crate::provider::sqlite::{
    sqlite_schema_fingerprint, with_sqlite_read_snapshot, ProviderSqliteSourceSnapshot,
};
use crate::{
    CaptureError, NormalizedProviderImportOptions, ProviderAdapterContext, ProviderImportSummary,
    Result,
};

mod event;
mod json_stream;
mod projection;
mod sqlite;
mod workspace;

use projection::TraeCapturedBatchProjector;
use sqlite::{
    decode_trae_position, initial_trae_position, trae_captured_error, trae_source_revision,
    trae_source_snapshot, trae_sqlite_batch_error, trae_validate_schema, TraeRowFetcher,
};
use workspace::{collect_trae_state_vscdb_paths, trae_workspace_folder, trae_workspace_id};

pub(crate) const TRAE_STATE_VSCDB_SOURCE_FORMAT: &str = "trae_state_vscdb";
pub(crate) const TRAE_CN_INPUT_HISTORY_KEY: &str = "icube-ai-agent-storage-input-history";
pub(crate) const TRAE_CHAT_KEYS: &[&str] = &[
    "memento/icube-ai-agent-storage",
    TRAE_CN_INPUT_HISTORY_KEY,
    "chat.ChatSessionStore.index",
    "ChatStore",
    "memento/icube-ai-chat-storage-7467774676505887760",
    "memento/icube-ai-ng-chat-storage-7467774676505887760",
];

const TRAE_CAPTURE_REVISION: u32 = 3;
const TRAE_POLICY_REVISION: u32 = 5;
const TRAE_POSITION_KIND: &str = "trae-itemtable-row-keyset-v2";
const TRAE_CHAT_ROW_LOCATOR_KIND: &str = "trae-itemtable-chat-row-v1";
const TRAE_FRONTIER_LOCATOR_KIND: &str = "trae-itemtable-frontier-v1";
const TRAE_CHAT_ROW_RECORD_KIND: &str = "trae-chat-row-rejection-v1";
const TRAE_INVALID_VALUE_RECORD_KIND: &str = "trae-invalid-itemtable-value-v1";
const TRAE_CHAT_VALUE_RECORD_KIND: &str = "trae-chat-value-v1";
const TRAE_FRONTIER_RECORD_KIND: &str = "trae-frontier-v1";
const TRAE_POSITION_BYTES: usize = 1 + 2 + 4 + 4 + 8;
const TRAE_SQLITE_VALUE_OVERHEAD_BYTES: u64 = 16 * 64;
const TRAE_COMPLETE_MESSAGE_LOCATOR_KIND: &str = "trae-itemtable-message-v1";

pub(crate) fn trae_complete_message_locator(
    key_index: u16,
    session_index: usize,
    message_index: usize,
) -> Result<NativeLocator> {
    let session_index = u32::try_from(session_index)
        .map_err(|_| CaptureError::InvalidPayload("Trae session index exceeds u32".to_owned()))?;
    let message_index = u32::try_from(message_index)
        .map_err(|_| CaptureError::InvalidPayload("Trae message index exceeds u32".to_owned()))?;
    let mut bytes = Vec::with_capacity(10);
    bytes.extend_from_slice(&key_index.to_be_bytes());
    bytes.extend_from_slice(&session_index.to_be_bytes());
    bytes.extend_from_slice(&message_index.to_be_bytes());
    NativeLocator::new(TRAE_COMPLETE_MESSAGE_LOCATOR_KIND, bytes).map_err(trae_captured_error)
}

pub(crate) fn trae_complete_value(conn: &Connection, key_index: u16) -> Result<Option<Vec<u8>>> {
    let Some(chat_key) = TRAE_CHAT_KEYS.get(usize::from(key_index)) else {
        return Ok(None);
    };
    conn.query_row(
        "select cast(value as text) from ItemTable where [key] = ?1 and typeof(value) = 'text'",
        [chat_key],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map(|value| value.map(String::into_bytes))
    .map_err(CaptureError::from)
}

pub(crate) fn trae_complete_message(
    bytes: &[u8],
    key_index: u16,
    session_index: u32,
    message_index: u32,
    provider_session_id: &str,
) -> Result<Option<(ProviderEventEnvelope, String)>> {
    let Some(chat_key) = TRAE_CHAT_KEYS.get(usize::from(key_index)) else {
        return Ok(None);
    };
    let selection = json_stream::trae_session_selection(bytes, chat_key)?;
    let Some(selection) = selection else {
        return Ok(None);
    };
    let session_index_usize = usize::try_from(session_index)
        .map_err(|_| CaptureError::InvalidPayload("Trae session index exceeds usize".to_owned()))?;
    let session = match selection {
        json_stream::TraeSessionSelection::CnMessages(messages) => {
            if session_index != 0 {
                return Ok(None);
            }
            json_stream::TraeStreamSession {
                native_session_id: "trae-cn-input-history".to_owned(),
                metadata_preview: json!({
                    "id": "trae-cn-input-history",
                    "title": "Trae CN input history",
                }),
                explicit_started_at: None,
                explicit_ended_at: None,
                explicit_title: Some("Trae CN input history".to_owned()),
                messages,
            }
        }
        json_stream::TraeSessionSelection::Sessions(container) => {
            let mut sessions = json_stream::TraeJsonContainerValues::new(bytes, container)?;
            let mut current = 0_usize;
            let mut selected = None;
            while let Some(range) = sessions.next_range()? {
                if current == session_index_usize {
                    selected = json_stream::trae_stream_session(bytes, range, current)?;
                    break;
                }
                current = current.saturating_add(1);
            }
            let Some(session) = selected else {
                return Ok(None);
            };
            session
        }
    };
    let suffix = format!("/{}", session.native_session_id);
    let Some(workspace_id) = provider_session_id.strip_suffix(&suffix) else {
        return Ok(None);
    };
    if workspace_id.is_empty() {
        return Ok(None);
    }
    let mut messages = json_stream::TraeJsonArrayValues::new(bytes, session.messages)?;
    let mut current = 0_u32;
    while let Some(range) = messages.next_range()? {
        if current == message_index {
            let message: Value = serde_json::from_slice(&bytes[range])?;
            let Some(input) = event::trae_event_from_owned_message(
                provider_session_id,
                workspace_id,
                chat_key,
                message,
                usize::try_from(message_index).unwrap_or(usize::MAX),
                DateTime::<Utc>::UNIX_EPOCH,
                0,
            ) else {
                return Ok(None);
            };
            let text = input.text.clone();
            return Ok(Some((
                event::trae_event(provider_session_id, workspace_id, chat_key, &input),
                text,
            )));
        }
        current = current.saturating_add(1);
    }
    Ok(None)
}

/// Admits either one `state.vscdb` or a workspace-storage directory and
/// orchestrates each workspace as an independently revisioned source.
pub(crate) fn import_trae_history_batched(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    import_options: NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let mut db_paths = collect_trae_state_vscdb_paths(path)?;
    db_paths.sort();
    db_paths.dedup();
    if db_paths.is_empty() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "no Trae state.vscdb files found",
        });
    }
    let source_root = context
        .source_root
        .clone()
        .or_else(|| context.source_path.clone())
        .unwrap_or_else(|| path.to_path_buf());
    let mut merged = ProviderImportSummary::default();
    for (workspace_index, db_path) in db_paths.iter().enumerate() {
        let summary = import_trae_state_vscdb_batched(
            db_path,
            store,
            ProviderAdapterContext {
                machine_id: context.machine_id.clone(),
                source_path: Some(db_path.clone()),
                source_root: Some(source_root.clone()),
                imported_at: context.imported_at,
            },
            workspace_index.saturating_add(1),
            import_options.clone(),
        )?;
        merged.merge_from(summary);
    }
    Ok(merged)
}

pub(crate) fn import_trae_state_vscdb_batched(
    path: &Path,
    store: &mut Store,
    mut context: ProviderAdapterContext,
    workspace_ordinal: usize,
    import_options: NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    context.source_path = Some(path.to_path_buf());
    let canonical_path = fs::canonicalize(path)?;
    let snapshot = trae_source_snapshot(path)?;
    let cursor_path = provider_path_identity(&canonical_path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Trae,
        TRAE_STATE_VSCDB_SOURCE_FORMAT,
        &cursor_path,
    );
    let conn = open_provider_sqlite_readonly(path)?;
    if !snapshot.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    trae_validate_schema(&conn, path)?;
    let schema_fingerprint = sqlite_schema_fingerprint(&conn)?;
    let workspace_id = trae_workspace_id(path);
    let workspace_folder = trae_workspace_folder(path);
    let source = SourceObservation::new(
        CaptureProvider::Trae,
        TRAE_STATE_VSCDB_SOURCE_FORMAT,
        format!("trae-sqlite:{cursor_path}"),
        trae_source_revision(&snapshot, &schema_fingerprint, workspace_ordinal),
        cursor_stream,
        TRAE_CAPTURE_REVISION,
        TRAE_POLICY_REVISION,
        import_options.inventory_observation_token.as_deref(),
    )
    .map_err(trae_captured_error)?;
    let stream = captured_batch_cursor_stream(&source);
    let expected_store_cursor = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let initial_position = initial_trae_position()?;
    let mut start_position = initial_position.clone();
    let mut cursor_mode = CapturedBatchCursorMode::Resume;
    if let Some(stored_cursor) = expected_store_cursor.as_ref() {
        match CertifiedProviderCursor::decode_if_certified(&stored_cursor.cursor)? {
            Some(certified)
                if certified.matches_revisions(
                    source.source_revision(),
                    source.capture_revision(),
                    source.policy_revision(),
                ) =>
            {
                let _: () = certified.parser_checkpoint().deserialize()?;
                decode_trae_position(certified.native_position())?;
                start_position = certified.native_position().clone();
            }
            Some(_) => cursor_mode = CapturedBatchCursorMode::ResetChangedSource,
            None => cursor_mode = CapturedBatchCursorMode::ReplaceLegacyCursor,
        }
    }
    let admission = CapturedSourceAdmission::conversation_for_context(&source, &context)?;
    let mut fetcher = TraeRowFetcher::new(&conn, workspace_ordinal)?;
    let mut producer = Some(SqliteLogicalRowBatchProducer::new(
        source,
        start_position,
        move |position| fetcher.fetch(position),
    ));
    let mut projector = TraeCapturedBatchProjector {
        context: context.clone(),
        workspace_id,
        workspace_folder,
        workspace_ordinal,
        #[cfg(test)]
        projected_chat_values: 0,
    };
    drain_captured_batches(
        store,
        &admission,
        import_options,
        &context.machine_id,
        context.imported_at,
        expected_store_cursor,
        &initial_position,
        cursor_mode,
        &stream,
        &mut projector,
        || {
            let Some(active_producer) = producer.as_mut() else {
                return Ok(None);
            };
            let batch = with_trae_source_revalidation(&snapshot, path, || {
                with_sqlite_read_snapshot(&conn, || {
                    active_producer
                        .next_batch()
                        .map_err(trae_sqlite_batch_error)
                })
            })?;
            if batch.as_ref().is_some_and(CapturedBatch::source_exhausted) || batch.is_none() {
                producer.take();
            }
            Ok(batch)
        },
        || snapshot.revalidate(path),
    )
}

fn with_trae_source_revalidation<T>(
    snapshot: &ProviderSqliteSourceSnapshot,
    path: &Path,
    capture: impl FnOnce() -> Result<T>,
) -> Result<T> {
    if !snapshot.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let value = capture()?;
    if !snapshot.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    Ok(value)
}

#[cfg(test)]
#[path = "trae/sqlite_tests.rs"]
mod sqlite_tests;

#[cfg(test)]
#[path = "trae/projection_tests.rs"]
mod projection_tests;
