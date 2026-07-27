use std::{cell::Cell, fs, path::Path};

use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;

use crate::captured_batch::sqlite_logical_rows::{
    SqliteLogicalRowBatchProducer, SqliteLogicalRowsBatchError,
};
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
    Result, NANOCLAW_SOURCE_FORMAT,
};

mod position;
mod project;
mod projection;
mod rows;
mod source;

mod complete_content;
pub(crate) use complete_content::{selected_component_addresses, NanoClawCompleteProject};
pub(crate) use position::decode_nanoclaw_message_locator;

#[cfg(test)]
#[path = "nanoclaw/tests.rs"]
mod tests;

use position::{decode_nanoclaw_position, initial_nanoclaw_position};
use project::{nanoclaw_project_root, NanoClawProjectSnapshot};
use projection::NanoClawCapturedBatchProjector;
use source::NanoClawRowFetcher;

// Revision 2 gives message records a compound project locator that binds the
// central session row, selected message database, and message row. The
// normalized event policy is unchanged.
const NANOCLAW_CAPTURE_REVISION: u32 = 2;
const NANOCLAW_POLICY_REVISION: u32 = 4;
// Snapshot consolidation changes only source-stability ownership. Captured bytes, projection
// policy, source identity, cursor encoding, and source-revision-v1 bytes remain unchanged, so
// neither persisted revision advances.
const NANOCLAW_POSITION_KIND: &str = "nanoclaw-project-keyset-v1";
const NANOCLAW_LOCATOR_KIND: &str = "nanoclaw-project-row-v1";
pub(crate) const NANOCLAW_MESSAGE_LOCATOR_KIND: &str = "nanoclaw-project-message-v1";
const NANOCLAW_SESSION_RECORD_KIND: &str = "nanoclaw-session-v1";
const NANOCLAW_MESSAGE_RECORD_KIND: &str = "nanoclaw-message-v1";

pub(crate) fn import_nanoclaw_project_batched(
    path: &Path,
    store: &mut Store,
    mut context: ProviderAdapterContext,
    import_options: NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    let project_root = nanoclaw_project_root(path)?;
    let canonical_root = fs::canonicalize(&project_root)?;
    if context.source_path.is_none() {
        context.source_path = Some(canonical_root.clone());
    }
    let raw_source_path = context
        .source_path
        .as_deref()
        .unwrap_or(&canonical_root)
        .display()
        .to_string();
    let central_path = canonical_root.join("data").join("v2.db");
    let snapshot = NanoClawProjectSnapshot::read(&canonical_root, &central_path)?;
    let conn = open_provider_sqlite_readonly(&central_path)?;
    if !snapshot.revalidate()? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let schema_fingerprint = sqlite_schema_fingerprint(&conn)?;
    let cursor_path = provider_path_identity(&canonical_root)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::NanoClaw,
        NANOCLAW_SOURCE_FORMAT,
        &cursor_path,
    );
    let source = SourceObservation::new(
        CaptureProvider::NanoClaw,
        NANOCLAW_SOURCE_FORMAT,
        format!("nanoclaw-project:{cursor_path}"),
        snapshot.source_revision(user_version, &schema_fingerprint),
        cursor_stream,
        NANOCLAW_CAPTURE_REVISION,
        NANOCLAW_POLICY_REVISION,
        import_options.inventory_observation_token.as_deref(),
    )
    .map_err(nanoclaw_captured_error)?;
    let stream = captured_batch_cursor_stream(&source);
    let expected_store_cursor = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let initial_position = initial_nanoclaw_position()?;
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
                decode_nanoclaw_position(certified.native_position())?;
                start_position = certified.native_position().clone();
            }
            Some(_) => cursor_mode = CapturedBatchCursorMode::ResetChangedSource,
            None => cursor_mode = CapturedBatchCursorMode::ReplaceLegacyCursor,
        }
    }
    let admission = CapturedSourceAdmission::conversation_for_context(&source, &context)?;
    let source_exhausted = Cell::new(false);
    let producer_source_exhausted = &source_exhausted;
    let mut fetcher = NanoClawRowFetcher::new(&conn, &snapshot)?;
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
    let mut projector = NanoClawCapturedBatchProjector::new(
        context.clone(),
        raw_source_path,
        central_path.display().to_string(),
        user_version,
        schema_fingerprint,
    );
    let summary = drain_captured_batches(
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
            let batch = with_sqlite_read_snapshot(&conn, || {
                active_producer
                    .next_batch()
                    .map_err(nanoclaw_sqlite_batch_error)
            })?;
            if source_exhausted.get() {
                producer.take();
            }
            Ok(batch)
        },
        || snapshot.revalidate_before_commit(),
    )?;
    Ok(summary)
}

pub(super) fn nanoclaw_captured_error(error: impl std::fmt::Display) -> CaptureError {
    CaptureError::InvalidPayload(error.to_string())
}

fn nanoclaw_sqlite_batch_error(error: SqliteLogicalRowsBatchError<CaptureError>) -> CaptureError {
    match error {
        SqliteLogicalRowsBatchError::Callback(error) => error,
        error => CaptureError::InvalidPayload(error.to_string()),
    }
}
