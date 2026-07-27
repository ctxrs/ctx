use std::{cell::Cell, fs, path::Path};

#[cfg(test)]
use std::path::PathBuf;

use ctx_history_core::{CaptureProvider, EventType};
use ctx_history_store::Store;

#[cfg(test)]
use chrono::{DateTime, Utc};
#[cfg(test)]
use ctx_history_core::{EventRole, Fidelity, ProviderEventEnvelope};
#[cfg(test)]
use rusqlite::Connection;
#[cfg(test)]
use serde_json::{json, Value};

use crate::captured_batch::sqlite_logical_rows::SqliteLogicalRowBatchProducer;
use crate::captured_batch::SourceObservation;
#[cfg(test)]
use crate::captured_batch::{
    CapturedBatch, NativePosition, CAPTURE_BATCH_MAX_OVERSIZE_RECORD_BYTES,
};
use crate::provider::importer::{
    captured_batch_cursor_stream, drain_captured_batches, provider_path_identity,
    provider_source_cursor_stream_for_path, CapturedBatchCursorMode, CapturedSourceAdmission,
    CertifiedProviderCursor,
};
#[cfg(test)]
use crate::provider::importer::{
    CapturedBatchCursorFinish, CapturedBatchProjector, ProviderProjectionOutput,
    ProviderProjectionResult,
};
use crate::provider::sqlite::open_provider_sqlite_readonly;
use crate::provider::sqlite::{
    sqlite_schema_fingerprint, with_sqlite_read_snapshot, ProviderSqliteSourceSnapshot,
};
#[cfg(test)]
use crate::ProviderNormalizationResult;
use crate::{
    CaptureError, NormalizedProviderImportOptions, ProviderAdapterContext, ProviderImportSummary,
    Result, WARP_SQLITE_SOURCE_FORMAT,
};

mod position;
mod projection;
mod proto;
mod sqlite;

use position::{decode_warp_position, initial_warp_position};
use projection::{WarpCapturedBatchProjector, WarpParserCheckpoint};
use sqlite::{warp_captured_error, warp_sqlite_batch_error, WarpRowFetcher, WarpSqliteSchema};

pub(crate) struct WarpTaskContent {
    pub(crate) event_type: EventType,
    pub(crate) native_record_id: String,
    pub(crate) text: String,
}

/// Pure provider-local reopening boundary shared by capture and SQLite
/// resolution. It never treats Warp's synthetic tool labels as source-backed
/// content.
pub(crate) fn warp_task_content_at(
    task_bytes: &[u8],
    fallback_task_id: &str,
    message_index: usize,
) -> Result<Option<WarpTaskContent>> {
    let task = proto::warp_decode_task(task_bytes)?;
    let task_id = if task.id.is_empty() {
        fallback_task_id
    } else {
        &task.id
    };
    let Some(message) = task.messages.get(message_index) else {
        return Ok(None);
    };
    let Some(text) = message.complete_text.clone() else {
        return Ok(None);
    };
    let native_record_id = if message.id.is_empty() {
        format!("{task_id}:{message_index}")
    } else {
        message.id.clone()
    };
    Ok(Some(WarpTaskContent {
        event_type: message.event_type,
        native_record_id,
        text,
    }))
}

const WARP_CAPTURE_REVISION: u32 = 5;
const WARP_POLICY_REVISION: u32 = 7;

fn warp_source_snapshot(path: &Path) -> Result<ProviderSqliteSourceSnapshot> {
    ProviderSqliteSourceSnapshot::read(
        path,
        "Warp SQLite source must be a regular non-symlink file",
        "Warp SQLite sidecar must be a regular non-symlink file",
    )
}

fn warp_source_revision(
    snapshot: &ProviderSqliteSourceSnapshot,
    schema_fingerprint: &str,
) -> String {
    format!(
        "warp-sqlite-snapshot-v1:capture={WARP_CAPTURE_REVISION};policy={WARP_POLICY_REVISION};schema={schema_fingerprint};{}",
        snapshot.revision_component(),
    )
}

pub(crate) fn import_warp_sqlite_batched(
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
    let snapshot = warp_source_snapshot(path)?;
    let cursor_path = provider_path_identity(&canonical_path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Warp,
        WARP_SQLITE_SOURCE_FORMAT,
        &cursor_path,
    );
    let conn = open_provider_sqlite_readonly(path)?;
    if !snapshot.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let schema_fingerprint = sqlite_schema_fingerprint(&conn)?;
    let schema = WarpSqliteSchema::detect(&conn)?;
    let source = SourceObservation::new(
        CaptureProvider::Warp,
        WARP_SQLITE_SOURCE_FORMAT,
        format!("warp-sqlite:{cursor_path}"),
        warp_source_revision(&snapshot, &schema_fingerprint),
        cursor_stream,
        WARP_CAPTURE_REVISION,
        WARP_POLICY_REVISION,
        import_options.inventory_observation_token.as_deref(),
    )
    .map_err(warp_captured_error)?;
    let stream = captured_batch_cursor_stream(&source);
    let expected_store_cursor = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let initial_position = initial_warp_position()?;
    let mut start_position = initial_position.clone();
    let mut cursor_mode = CapturedBatchCursorMode::Resume;
    let mut checkpoint = WarpParserCheckpoint::default();
    if let Some(stored_cursor) = expected_store_cursor.as_ref() {
        match CertifiedProviderCursor::decode_if_certified(&stored_cursor.cursor)? {
            Some(certified)
                if certified.matches_revisions(
                    source.source_revision(),
                    source.capture_revision(),
                    source.policy_revision(),
                ) =>
            {
                checkpoint = certified.parser_checkpoint().deserialize()?;
                decode_warp_position(certified.native_position())?;
                start_position = certified.native_position().clone();
            }
            Some(_) => cursor_mode = CapturedBatchCursorMode::ResetChangedSource,
            None => cursor_mode = CapturedBatchCursorMode::ReplaceLegacyCursor,
        }
    }
    let admission = CapturedSourceAdmission::conversation_for_context(&source, &context)?;
    let source_exhausted = Cell::new(false);
    let producer_source_exhausted = &source_exhausted;
    let mut fetcher = WarpRowFetcher::from_schema(&conn, &start_position, &schema)?;
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
    let mut projector = WarpCapturedBatchProjector {
        context: context.clone(),
        raw_source_path,
        user_version,
        schema_fingerprint,
        checkpoint,
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
                    .map_err(warp_sqlite_batch_error)
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
#[path = "warp/integration_tests.rs"]
mod captured_batch_tests;
