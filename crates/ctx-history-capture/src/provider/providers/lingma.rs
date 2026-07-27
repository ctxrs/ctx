use std::{fs, path::Path};

use ctx_history_core::{CaptureProvider, ProviderEventEnvelope};
use ctx_history_store::Store;
use rusqlite::Connection;

use crate::captured_batch::{CapturedSqliteValue, SourceObservation};
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
    Result, LINGMA_SQLITE_SOURCE_FORMAT,
};

mod projector;
mod sqlite;
mod text;

#[cfg(test)]
use projector::lingma_values_rowid;
use projector::LingmaCapturedBatchProjector;
use sqlite::{decode_lingma_position, initial_lingma_position, LingmaBatchProducer, LingmaSchema};
#[cfg(test)]
use sqlite::{
    encode_lingma_position, lingma_candidate_sql, lingma_locator,
    lingma_retained_text_byte_bound_sql, LingmaKeyset,
};
#[cfg(test)]
use text::{decode_lingma_sqlite_text, LingmaSqliteEncoding};

const LINGMA_CAPTURE_REVISION: u32 = 5;
const LINGMA_POLICY_REVISION: u32 = 6;
const LINGMA_POSITION_KIND: &str = "lingma-chat-record-rowid-v5";
const LINGMA_LOCATOR_KIND: &str = "lingma-chat-record-v1";
const LINGMA_RECORD_KIND: &str = "lingma-chat-record-row-local-v2";
const LINGMA_MALFORMED_RECORD_KIND: &str = "lingma-malformed-text-row-v1";
const LINGMA_SKIPPED_RECORD_KIND: &str = "lingma-skipped-row-v1";

fn lingma_source_snapshot(path: &Path) -> Result<ProviderSqliteSourceSnapshot> {
    ProviderSqliteSourceSnapshot::read(
        path,
        "Lingma SQLite source must be a regular non-symlink file",
        "Lingma SQLite sidecar must be a regular non-symlink file",
    )
}

fn lingma_source_revision(
    snapshot: &ProviderSqliteSourceSnapshot,
    user_version: i64,
    schema_fingerprint: &str,
) -> String {
    format!(
        "lingma-sqlite-snapshot-v1:capture={LINGMA_CAPTURE_REVISION};policy={LINGMA_POLICY_REVISION};user_version={user_version};schema={schema_fingerprint};{}",
        snapshot.revision_component(),
    )
}

pub(crate) fn import_lingma_sqlite_batched(
    path: &Path,
    store: &mut Store,
    mut context: ProviderAdapterContext,
    import_options: NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    if context.source_path.is_none() {
        context.source_path = Some(path.to_path_buf());
    }
    let raw_source_path = context
        .source_path
        .as_deref()
        .unwrap_or(path)
        .display()
        .to_string();
    let canonical_path = fs::canonicalize(path)?;
    let snapshot = lingma_source_snapshot(path)?;
    let cursor_path = provider_path_identity(&canonical_path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Lingma,
        LINGMA_SQLITE_SOURCE_FORMAT,
        &cursor_path,
    );
    let conn = open_provider_sqlite_readonly(path)?;
    if !snapshot.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let schema = LingmaSchema::detect(&conn)?;
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let schema_fingerprint = sqlite_schema_fingerprint(&conn)?;
    let source = SourceObservation::new(
        CaptureProvider::Lingma,
        LINGMA_SQLITE_SOURCE_FORMAT,
        format!("lingma-sqlite:{cursor_path}"),
        lingma_source_revision(&snapshot, user_version, &schema_fingerprint),
        cursor_stream,
        LINGMA_CAPTURE_REVISION,
        LINGMA_POLICY_REVISION,
        import_options.inventory_observation_token.as_deref(),
    )
    .map_err(lingma_captured_error)?;
    let stream = captured_batch_cursor_stream(&source);
    let expected_store_cursor = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let initial_position = initial_lingma_position()?;
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
                decode_lingma_position(certified.native_position())?;
                start_position = certified.native_position().clone();
            }
            Some(_) => cursor_mode = CapturedBatchCursorMode::ResetChangedSource,
            None => cursor_mode = CapturedBatchCursorMode::ReplaceLegacyCursor,
        }
    }
    let admission = CapturedSourceAdmission::conversation_for_context(&source, &context)?;
    let mut producer = Some(LingmaBatchProducer::new(
        &conn,
        source,
        start_position,
        schema,
    )?);
    let mut projector = LingmaCapturedBatchProjector::new(
        context.clone(),
        raw_source_path,
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
            let batch = with_sqlite_read_snapshot(&conn, || active_producer.next_batch())?;
            if !snapshot.revalidate(path)? {
                return Err(CaptureError::SourceChangedDuringCapture);
            }
            if batch.is_none() {
                producer.take();
            }
            Ok(batch)
        },
        || snapshot.revalidate(path),
    )
}

fn lingma_captured_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

/// Reads the exact logical row shape used by capture for verified-content reopening.
pub(crate) fn lingma_complete_values(
    conn: &Connection,
    rowid: i64,
) -> Result<Option<Vec<CapturedSqliteValue>>> {
    sqlite::lingma_complete_values(conn, rowid)
}

/// Replays the authoritative user-prompt normalization for one captured row.
pub(crate) fn lingma_complete_user_message(
    values: &[CapturedSqliteValue],
) -> Result<(ProviderEventEnvelope, String)> {
    projector::lingma_complete_user_message(values)
}

#[cfg(test)]
#[path = "lingma/tests.rs"]
mod captured_batch_tests;
