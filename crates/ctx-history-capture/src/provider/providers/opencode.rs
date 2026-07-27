use std::{cell::Cell, fs, path::Path};

use ctx_history_store::Store;

use crate::captured_batch::sqlite_logical_rows::SqliteLogicalRowBatchProducer;
use crate::captured_batch::{ProviderRecordKind, SourceObservation};
use crate::provider::importer::{
    captured_batch_cursor_stream, drain_captured_batches, provider_path_identity,
    provider_source_cursor_stream_for_path, CapturedBatchCursorMode, CapturedSourceAdmission,
    CertifiedProviderCursor,
};
use crate::provider::sqlite::{
    open_provider_sqlite_readonly, sqlite_schema_fingerprint, with_sqlite_read_snapshot,
    ProviderSqliteSourceSnapshot,
};
use crate::{
    CaptureError, NormalizedProviderImportOptions, ProviderAdapterContext, ProviderImportSummary,
    Result,
};

mod capture;
mod normalization;
mod projection;
mod schema;

use capture::{
    initial_opencode_position, opencode_captured_error, opencode_sqlite_batch_error,
    validate_opencode_resume_position, OpenCodeRowFetcher, OPENCODE_RECORD_KIND,
};
use projection::{OpenCodeCapturedBatchProjector, OpenCodeProjectionSource};
use schema::{opencode_captured_shape, OpenCodeCapturedShape};
pub(crate) use schema::{
    OpenCodeSqliteDialect, KILO_SQLITE_DIALECT, MIMOCODE_SQLITE_DIALECT, OPENCODE_SQLITE_DIALECT,
};

const OPENCODE_CAPTURE_REVISION: u32 = 6;
const OPENCODE_POLICY_REVISION: u32 = 6;

fn opencode_source_snapshot(path: &Path) -> Result<ProviderSqliteSourceSnapshot> {
    ProviderSqliteSourceSnapshot::read(
        path,
        "OpenCode-family SQLite source component must be a regular non-symlink file",
        "OpenCode-family SQLite sidecar must be a regular non-symlink file",
    )
}

fn opencode_source_revision(
    snapshot: &ProviderSqliteSourceSnapshot,
    dialect: &OpenCodeSqliteDialect,
    shape: OpenCodeCapturedShape,
    schema_fingerprint: &str,
) -> String {
    format!(
        "opencode-sqlite-snapshot-v1:provider={};capture={OPENCODE_CAPTURE_REVISION};policy={OPENCODE_POLICY_REVISION};shape={};schema={schema_fingerprint};{}",
        dialect.provider.as_str(),
        shape.label(),
        snapshot.revision_component(),
    )
}

pub(crate) fn import_opencode_sqlite_batched(
    path: &Path,
    store: &mut Store,
    mut context: ProviderAdapterContext,
    import_options: NormalizedProviderImportOptions,
    dialect: &OpenCodeSqliteDialect,
) -> Result<ProviderImportSummary> {
    context.source_path = Some(path.to_path_buf());
    let canonical_path = fs::canonicalize(path)?;
    let snapshot = opencode_source_snapshot(path)?;
    let cursor_source_path = provider_path_identity(&canonical_path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        dialect.provider,
        dialect.source_format,
        &cursor_source_path,
    );
    let conn = open_provider_sqlite_readonly(path)?;
    if !snapshot.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let schema_fingerprint = sqlite_schema_fingerprint(&conn)?;
    let shape = opencode_captured_shape(&conn, dialect)?;
    let source = SourceObservation::new(
        dialect.provider,
        dialect.source_format,
        format!("opencode-sqlite:{cursor_source_path}"),
        opencode_source_revision(&snapshot, dialect, shape, &schema_fingerprint),
        cursor_stream,
        OPENCODE_CAPTURE_REVISION,
        OPENCODE_POLICY_REVISION,
        import_options.inventory_observation_token.as_deref(),
    )
    .map_err(opencode_captured_error)?;
    let stream = captured_batch_cursor_stream(&source);
    let expected_store_cursor = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let initial_position = initial_opencode_position(shape)?;
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
                validate_opencode_resume_position(certified.native_position(), shape)?;
                start_position = certified.native_position().clone();
            }
            Some(_) => cursor_mode = CapturedBatchCursorMode::ResetChangedSource,
            None => cursor_mode = CapturedBatchCursorMode::ReplaceLegacyCursor,
        }
    }
    let admission = CapturedSourceAdmission::conversation_for_context(&source, &context)?;
    let record_kind =
        ProviderRecordKind::new(OPENCODE_RECORD_KIND).map_err(opencode_captured_error)?;
    let source_exhausted = Cell::new(false);
    let producer_source_exhausted = &source_exhausted;
    let mut fetcher = OpenCodeRowFetcher::new(&conn, shape, record_kind)?;
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
    let mut projector = OpenCodeCapturedBatchProjector::new(
        context.clone(),
        OpenCodeProjectionSource {
            database_path: path.to_path_buf(),
            conn: &conn,
            snapshot: snapshot.clone(),
        },
        dialect,
        user_version,
        schema_fingerprint,
        shape,
    )?;
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
                    .map_err(opencode_sqlite_batch_error)
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
mod captured_batch_tests {
    include!("opencode/captured_batch_tests.rs");
}
