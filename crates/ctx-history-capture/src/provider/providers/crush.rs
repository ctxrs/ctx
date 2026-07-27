use std::{cell::Cell, fs, path::Path};

use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;

use crate::captured_batch::sqlite_logical_rows::SqliteLogicalRowBatchProducer;
use crate::captured_batch::SourceObservation;
use crate::provider::importer::{
    captured_batch_cursor_stream, drain_captured_batches, provider_path_identity,
    provider_source_cursor_stream_for_path, CapturedBatchCursorMode, CapturedSourceAdmission,
    CertifiedProviderCursor,
};
use crate::provider::sqlite::open_provider_sqlite_readonly;
use crate::provider::sqlite::{sqlite_schema_fingerprint, with_sqlite_read_snapshot};
use crate::{
    CaptureError, NormalizedProviderImportOptions, ProviderAdapterContext, ProviderImportSummary,
    Result, CRUSH_SQLITE_SOURCE_FORMAT,
};

use capture::{
    captured_error, decode_position, initial_position, sqlite_batch_error, CrushRowFetcher,
};
use projection::CrushCapturedBatchProjector;
use source::{
    message_columns, optional_file_columns, optional_read_file_columns, session_columns,
    source_revision, source_snapshot,
};

mod capture;
mod projection;
mod source;

#[cfg(test)]
mod tests;

pub(super) const CRUSH_CAPTURE_REVISION: u32 = 3;
pub(super) const CRUSH_POLICY_REVISION: u32 = 4;
pub(super) const CRUSH_SESSION_RECORD_KIND: &str = "crush-session-v1";
pub(super) const CRUSH_MESSAGE_CHILD_RECORD_KIND: &str = "crush-message-child-v1";
pub(super) const CRUSH_FILE_RECORD_KIND: &str = "crush-file-v1";
pub(super) const CRUSH_READ_FILE_RECORD_KIND: &str = "crush-read-file-v1";

pub(crate) fn import_crush_sqlite_batched(
    path: &Path,
    store: &mut Store,
    mut context: ProviderAdapterContext,
    import_options: NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    context.source_path = Some(path.to_path_buf());
    let canonical_path = fs::canonicalize(path)?;
    let snapshot = source_snapshot(path)?;
    let cursor_path = provider_path_identity(&canonical_path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Crush,
        CRUSH_SQLITE_SOURCE_FORMAT,
        &cursor_path,
    );
    let conn = open_provider_sqlite_readonly(path)?;
    if !snapshot.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let schema_fingerprint = sqlite_schema_fingerprint(&conn)?;
    let session_columns = session_columns(&conn)?;
    let message_columns = message_columns(&conn)?;
    let file_columns = optional_file_columns(&conn)?;
    let read_file_columns = optional_read_file_columns(&conn)?;
    let source = SourceObservation::new(
        CaptureProvider::Crush,
        CRUSH_SQLITE_SOURCE_FORMAT,
        format!("crush-sqlite:{cursor_path}"),
        source_revision(&snapshot, &schema_fingerprint),
        cursor_stream,
        CRUSH_CAPTURE_REVISION,
        CRUSH_POLICY_REVISION,
        import_options.inventory_observation_token.as_deref(),
    )
    .map_err(captured_error)?;
    let stream = captured_batch_cursor_stream(&source);
    let expected_store_cursor = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let initial_position = initial_position()?;
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
                decode_position(certified.native_position())?;
                start_position = certified.native_position().clone();
            }
            Some(_) => cursor_mode = CapturedBatchCursorMode::ResetChangedSource,
            None => cursor_mode = CapturedBatchCursorMode::ReplaceLegacyCursor,
        }
    }
    let admission = CapturedSourceAdmission::conversation_for_context(&source, &context)?;
    let source_exhausted = Cell::new(false);
    let producer_source_exhausted = &source_exhausted;
    let mut fetcher = CrushRowFetcher::new(
        &conn,
        &session_columns,
        &message_columns,
        file_columns.as_ref(),
        read_file_columns.as_ref(),
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
    let mut projector = CrushCapturedBatchProjector::new(
        context.clone(),
        path.display().to_string(),
        user_version,
        schema_fingerprint,
    );
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
                active_producer.next_batch().map_err(sqlite_batch_error)
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
