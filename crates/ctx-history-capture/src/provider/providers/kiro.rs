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
use crate::provider::sqlite::{
    sqlite_schema_fingerprint, with_sqlite_read_snapshot, ProviderSqliteSourceSnapshot,
};
use crate::{
    CaptureError, NormalizedProviderImportOptions, ProviderAdapterContext, ProviderImportSummary,
    Result, KIRO_SQLITE_SOURCE_FORMAT,
};

mod event;
mod history;
mod sqlite;

pub(crate) use history::{
    decode_kiro_conversation_for_complete, kiro_history_events, kiro_provider_session_id,
    kiro_session_started_at,
};
use sqlite::{
    initial_kiro_position, kiro_captured_error, kiro_conversation_tables, kiro_sqlite_batch_error,
    validate_kiro_position, KiroCapturedBatchProjector, KiroRowFetcher,
};
const KIRO_CAPTURE_REVISION: u32 = 1;
const KIRO_POLICY_REVISION: u32 = 4;

fn kiro_source_snapshot(path: &Path) -> Result<ProviderSqliteSourceSnapshot> {
    ProviderSqliteSourceSnapshot::read(
        path,
        "Kiro SQLite source must be a regular non-symlink file",
        "Kiro SQLite sidecar must be a regular non-symlink file",
    )
}

fn kiro_source_revision(
    snapshot: &ProviderSqliteSourceSnapshot,
    user_version: i64,
    schema_fingerprint: &str,
) -> String {
    format!(
        "kiro-sqlite-snapshot-v1:capture={KIRO_CAPTURE_REVISION};policy={KIRO_POLICY_REVISION};user_version={user_version};schema={schema_fingerprint};{}",
        snapshot.revision_component(),
    )
}

pub(crate) fn import_kiro_sqlite_batched(
    path: &Path,
    store: &mut Store,
    mut context: ProviderAdapterContext,
    import_options: NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    context.source_path = Some(path.to_path_buf());
    let canonical_path = fs::canonicalize(path)?;
    let snapshot = kiro_source_snapshot(path)?;
    let cursor_path = provider_path_identity(&canonical_path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::KiroCli,
        KIRO_SQLITE_SOURCE_FORMAT,
        &cursor_path,
    );
    let conn = open_provider_sqlite_readonly(path)?;
    if !snapshot.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let schema_fingerprint = sqlite_schema_fingerprint(&conn)?;
    let tables = kiro_conversation_tables(&conn)?;
    let source = SourceObservation::new(
        CaptureProvider::KiroCli,
        KIRO_SQLITE_SOURCE_FORMAT,
        format!("kiro-sqlite:{cursor_path}"),
        kiro_source_revision(&snapshot, user_version, &schema_fingerprint),
        cursor_stream,
        KIRO_CAPTURE_REVISION,
        KIRO_POLICY_REVISION,
        import_options.inventory_observation_token.as_deref(),
    )
    .map_err(kiro_captured_error)?;
    let stream = captured_batch_cursor_stream(&source);
    let expected_store_cursor = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let initial_position = initial_kiro_position()?;
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
                validate_kiro_position(certified.native_position())?;
                start_position = certified.native_position().clone();
            }
            Some(_) => cursor_mode = CapturedBatchCursorMode::ResetChangedSource,
            None => cursor_mode = CapturedBatchCursorMode::ReplaceLegacyCursor,
        }
    }
    let admission = CapturedSourceAdmission::conversation_for_context(&source, &context)?;
    let source_exhausted = Cell::new(false);
    let producer_source_exhausted = &source_exhausted;
    let mut fetcher = KiroRowFetcher::new(&conn, tables)?;
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
    let mut projector = KiroCapturedBatchProjector::new(
        context.clone(),
        path.to_path_buf(),
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
                active_producer
                    .next_batch()
                    .map_err(kiro_sqlite_batch_error)
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
