use std::{cell::Cell, fs, path::Path};

use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;

use crate::captured_batch::sqlite_logical_rows::SqliteLogicalRowBatchProducer;
use crate::captured_batch::{ProviderRecordKind, SourceObservation};
use crate::provider::importer::{
    captured_batch_cursor_stream, drain_captured_batches, provider_path_identity,
    provider_source_cursor_stream_for_path, CapturedBatchCursorMode, CapturedSourceAdmission,
    CertifiedProviderCursor,
};
use crate::provider::sqlite::open_provider_sqlite_readonly;
use crate::provider::sqlite::{sqlite_schema_fingerprint, with_sqlite_read_snapshot};
use crate::{
    CaptureError, NormalizedProviderImportOptions, ProviderAdapterContext, ProviderImportSummary,
    Result, FORGECODE_SQLITE_SOURCE_FORMAT,
};

use self::projection::ForgeCodeCapturedBatchProjector;
use self::source::{
    decode_forgecode_position, forgecode_captured_error, forgecode_conversation_columns,
    forgecode_source_revision, forgecode_source_snapshot, forgecode_sqlite_batch_error,
    initial_forgecode_position, ForgeCodeRowFetcher,
};

mod event;
mod normalization;
mod projection;
mod source;
#[cfg(test)]
mod tests;

// Narrow provider API consumed by the source resolver after locator attachment.
#[allow(unused_imports)]
pub(crate) use event::forgecode_normalized_result_content;

const FORGECODE_CAPTURE_REVISION: u32 = 1;
const FORGECODE_POLICY_REVISION: u32 = 5;
const FORGECODE_POSITION_KIND: &str = "forgecode-conversation-rowid-v1";
const FORGECODE_LOCATOR_KIND: &str = "forgecode-conversation-row-v1";
const FORGECODE_RECORD_KIND: &str = "forgecode-conversation-v1";
const FORGECODE_REJECTED_RECORD_KIND: &str = "forgecode-rejected-conversation-v1";
const FORGECODE_POSITION_BYTES: usize = 1 + 8 + 8;
const FORGECODE_SQLITE_VALUE_OVERHEAD_BYTES: u64 = 64 * 8;

#[cfg(test)]
pub(crate) fn forgecode_text_message_text(
    body: &serde_json::Value,
    event_type: ctx_history_core::EventType,
) -> String {
    event::forgecode_text_message_text(body, event_type)
}

pub(crate) fn forgecode_result_record(
    conn: &rusqlite::Connection,
    rowid: i64,
    subrecord: u32,
) -> Result<Option<crate::complete_content::sqlite::SqliteResultRecord>> {
    let Some(values) = source::forgecode_values_at_rowid(conn, rowid)? else {
        return Ok(None);
    };
    let row = source::decode_forgecode_conversation(&values)?;
    let context = row
        .context
        .as_deref()
        .filter(|raw| !raw.trim().is_empty())
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "ForgeCode result conversation context is no longer valid JSON".to_owned(),
            )
        })?;
    let entry = context
        .get("messages")
        .and_then(serde_json::Value::as_array)
        .and_then(|messages| messages.get(subrecord as usize))
        .ok_or_else(|| {
            CaptureError::InvalidPayload(
                "ForgeCode result message coordinate is no longer present".to_owned(),
            )
        })?;
    let parts = event::forgecode_message_parts(entry);
    let content = event::forgecode_normalized_result_content(parts.body).ok_or_else(|| {
        CaptureError::InvalidPayload("ForgeCode row is no longer a supported result".to_owned())
    })?;
    let native_record_id = crate::compute_payload_hash(entry)?;
    Ok(Some(crate::complete_content::sqlite::SqliteResultRecord {
        values,
        native_record_id,
        content,
    }))
}

pub(crate) fn forgecode_complete_message(
    values: &[crate::captured_batch::CapturedSqliteValue],
    subrecord_index: u32,
) -> Result<(String, String, String)> {
    let row = source::decode_forgecode_conversation(values)?;
    let context = row
        .context
        .as_deref()
        .filter(|raw| !raw.trim().is_empty())
        .ok_or_else(|| {
            CaptureError::InvalidPayload("ForgeCode conversation has no context".into())
        })?;
    let value: serde_json::Value = serde_json::from_str(context)?;
    let entry = value
        .get("messages")
        .and_then(serde_json::Value::as_array)
        .and_then(|messages| messages.get(subrecord_index as usize))
        .ok_or_else(|| {
            CaptureError::InvalidPayload("ForgeCode message subrecord is missing".into())
        })?;
    let parts = event::forgecode_message_parts(entry);
    let text = event::forgecode_message_text(parts, event::forgecode_event_type(parts));
    Ok((
        row.conversation_id,
        crate::compute_payload_hash(entry)?,
        text,
    ))
}

pub(crate) use source::load_forgecode_conversation_values;

pub(crate) fn import_forgecode_sqlite_batched(
    path: &Path,
    store: &mut Store,
    mut context: ProviderAdapterContext,
    import_options: NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    context.source_path = Some(path.to_path_buf());
    let canonical_path = fs::canonicalize(path)?;
    let snapshot = forgecode_source_snapshot(path)?;
    let cursor_path = provider_path_identity(&canonical_path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::ForgeCode,
        FORGECODE_SQLITE_SOURCE_FORMAT,
        &cursor_path,
    );
    let conn = open_provider_sqlite_readonly(path)?;
    if !snapshot.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let schema_fingerprint = sqlite_schema_fingerprint(&conn)?;
    let columns = forgecode_conversation_columns(&conn)?;
    let source = SourceObservation::new(
        CaptureProvider::ForgeCode,
        FORGECODE_SQLITE_SOURCE_FORMAT,
        format!("forgecode-sqlite:{cursor_path}"),
        forgecode_source_revision(&snapshot, &schema_fingerprint),
        cursor_stream,
        FORGECODE_CAPTURE_REVISION,
        FORGECODE_POLICY_REVISION,
        import_options.inventory_observation_token.as_deref(),
    )
    .map_err(forgecode_captured_error)?;
    let stream = captured_batch_cursor_stream(&source);
    let expected_store_cursor = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let initial_position = initial_forgecode_position()?;
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
                decode_forgecode_position(certified.native_position())?;
                start_position = certified.native_position().clone();
            }
            Some(_) => cursor_mode = CapturedBatchCursorMode::ResetChangedSource,
            None => cursor_mode = CapturedBatchCursorMode::ReplaceLegacyCursor,
        }
    }
    let admission = CapturedSourceAdmission::conversation_for_context(&source, &context)?;
    let source_exhausted = Cell::new(false);
    let producer_source_exhausted = &source_exhausted;
    let mut fetcher = ForgeCodeRowFetcher::new(
        &conn,
        &columns,
        ProviderRecordKind::new(FORGECODE_RECORD_KIND).map_err(forgecode_captured_error)?,
    )?;
    let mut producer = Some(SqliteLogicalRowBatchProducer::new(
        source,
        start_position,
        move |position| {
            let row = fetcher.fetch(position)?;
            if row.is_none() {
                producer_source_exhausted.set(true);
            }
            Ok(row)
        },
    ));
    let mut projector = ForgeCodeCapturedBatchProjector {
        context: context.clone(),
        raw_source_path: path.display().to_string(),
        user_version,
        schema_fingerprint,
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
            if !snapshot.revalidate(path)? {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            let batch = with_sqlite_read_snapshot(&conn, || {
                active_producer
                    .next_batch()
                    .map_err(forgecode_sqlite_batch_error)
            })?;
            if !snapshot.revalidate(path)? {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            if source_exhausted.get() {
                producer.take();
            }
            Ok(batch)
        },
        || snapshot.revalidate(path),
    )
}
