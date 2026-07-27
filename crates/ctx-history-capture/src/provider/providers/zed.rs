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
    Result, ZED_THREADS_SQLITE_SOURCE_FORMAT,
};

mod event;
mod projection;
mod source;
mod thread;

pub(crate) use event::decode_zed_thread_events;
pub(crate) use event::zed_result_content;
use projection::ZedCapturedBatchProjector;
use source::{
    initial_zed_position, zed_captured_error, zed_source_revision, zed_source_snapshot,
    zed_sqlite_batch_error, zed_thread_columns, ZedRowFetcher,
};
pub(crate) use thread::decode_zed_thread_for_complete;

// Revision 4 makes the provider-local event decoder authoritative for both capture and
// complete-content recovery, and rejects payloads outside its explicit structural bounds.
// Policy 5 adds compact result references and verified result-body locators.
const ZED_CAPTURE_REVISION: u32 = 4;
const ZED_POLICY_REVISION: u32 = 5;
const ZED_POSITION_KIND: &str = "zed-thread-native-keyset-v2";
const ZED_LOCATOR_KIND: &str = "zed-thread-row-v1";
const ZED_RECORD_KIND: &str = "zed-thread-v1";
const ZED_MALFORMED_RECORD_KIND: &str = "zed-thread-malformed-v1";
const ZED_POSITION_BYTES: usize = 1 + 1 + 8 + 8;
const ZED_SQLITE_VALUE_OVERHEAD_BYTES: u64 = 64 * 10;
pub(crate) const ZED_RESULT_CONTENT_PROFILE: &str = "zed.result-body.v1";

pub(crate) fn import_zed_threads_sqlite_batched(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    import_options: NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let canonical_path = fs::canonicalize(path)?;
    let snapshot = zed_source_snapshot(path)?;
    let cursor_path = provider_path_identity(&canonical_path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Zed,
        ZED_THREADS_SQLITE_SOURCE_FORMAT,
        &cursor_path,
    );
    let conn = open_provider_sqlite_readonly(path)?;
    if !snapshot.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let schema_fingerprint = sqlite_schema_fingerprint(&conn)?;
    let columns = zed_thread_columns(&conn)?;
    let source = SourceObservation::new(
        CaptureProvider::Zed,
        ZED_THREADS_SQLITE_SOURCE_FORMAT,
        format!("zed-sqlite:{cursor_path}"),
        zed_source_revision(&snapshot, user_version, &schema_fingerprint),
        cursor_stream,
        ZED_CAPTURE_REVISION,
        ZED_POLICY_REVISION,
        import_options.inventory_observation_token.as_deref(),
    )
    .map_err(zed_captured_error)?;
    let stream = captured_batch_cursor_stream(&source);
    let expected_store_cursor = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let initial_position = initial_zed_position()?;
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
                source::decode_zed_position(certified.native_position())?;
                start_position = certified.native_position().clone();
            }
            Some(_) => cursor_mode = CapturedBatchCursorMode::ResetChangedSource,
            None => cursor_mode = CapturedBatchCursorMode::ReplaceLegacyCursor,
        }
    }
    let admission = CapturedSourceAdmission::conversation_for_context(&source, &context)?;
    let source_exhausted = Cell::new(false);
    let producer_source_exhausted = &source_exhausted;
    let mut fetcher = ZedRowFetcher::new(
        &conn,
        &columns,
        ProviderRecordKind::new(ZED_RECORD_KIND).map_err(zed_captured_error)?,
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
    let mut projector = ZedCapturedBatchProjector {
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
                active_producer.next_batch().map_err(zed_sqlite_batch_error)
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

#[cfg(test)]
#[path = "zed/tests.rs"]
mod tests;
