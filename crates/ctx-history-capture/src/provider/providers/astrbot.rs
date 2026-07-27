mod codec;
mod preferences;
mod producer;
mod projector;
mod relationships;
mod source;

#[cfg(test)]
mod tests;

use std::{cell::Cell, fs, path::Path};

use ctx_history_core::{CaptureProvider, ProviderEventEnvelope};
use ctx_history_store::Store;
use rusqlite::Connection;

use crate::captured_batch::sqlite_logical_rows::SqliteLogicalRowBatchProducer;
use crate::captured_batch::{CapturedSqliteValue, NativeLocator, SourceObservation};
use crate::provider::importer::{
    captured_batch_cursor_stream, drain_captured_batches, provider_path_identity,
    provider_source_cursor_stream_for_path, BoundedParserCheckpoint, CapturedBatchCursorMode,
    CapturedSourceAdmission, CertifiedProviderCursor,
};
use crate::provider::sqlite::open_provider_sqlite_readonly;
use crate::provider::sqlite::{sqlite_schema_fingerprint, with_sqlite_read_snapshot};
use crate::{
    CaptureError, NormalizedProviderImportOptions, ProviderAdapterContext, ProviderImportSummary,
    Result, ASTRBOT_SQLITE_SOURCE_FORMAT,
};

use self::codec::{
    astrbot_captured_error, astrbot_sqlite_batch_error, decode_astrbot_position,
    initial_astrbot_position, AstrBotParserCheckpoint,
};
use self::preferences::astrbot_selected_conversation_bounded;
use self::producer::AstrBotRowFetcher;
use self::projector::AstrBotCapturedBatchProjector;
use self::relationships::{
    astrbot_prepare_relationship_projection, astrbot_relationship_projection_needed,
};
use self::source::{
    astrbot_conversation_columns, astrbot_source_revision, astrbot_source_snapshot, AstrBotSql,
};

const ASTRBOT_CAPTURE_REVISION: u32 = 2;
const ASTRBOT_POLICY_REVISION: u32 = 5;
const ASTRBOT_COMPLETE_MESSAGE_LOCATOR_KIND: &str = "astrbot-conversation-message-v1";

pub(crate) fn astrbot_complete_message_locator(
    physical_rowid: i64,
    item_index: usize,
) -> Result<NativeLocator> {
    let item_index = u32::try_from(item_index).map_err(|_| {
        CaptureError::InvalidPayload("AstrBot message index exceeds u32".to_owned())
    })?;
    let mut value = Vec::with_capacity(12);
    value.extend_from_slice(&codec::astrbot_ordered_i64(physical_rowid).to_be_bytes());
    value.extend_from_slice(&item_index.to_be_bytes());
    NativeLocator::new(ASTRBOT_COMPLETE_MESSAGE_LOCATOR_KIND, value)
        .map_err(codec::astrbot_captured_error)
}

pub(crate) fn astrbot_complete_conversation_values(
    conn: &Connection,
    physical_rowid: i64,
) -> Result<Option<Vec<CapturedSqliteValue>>> {
    let sql = AstrBotSql::new(conn)?;
    match producer::astrbot_hydrate_conversation(conn, &sql.conversation_hydration, physical_rowid)
    {
        Ok(row) => Ok(Some(codec::astrbot_conversation_values(row))),
        Err(CaptureError::Sqlite(rusqlite::Error::QueryReturnedNoRows)) => Ok(None),
        Err(error) => Err(error),
    }
}

pub(crate) fn astrbot_complete_conversation_message(
    values: &[CapturedSqliteValue],
    item_index: u32,
) -> Result<Option<(ProviderEventEnvelope, String, String)>> {
    projector::astrbot_complete_conversation_message(values, item_index)
}

pub(crate) fn import_astrbot_sqlite_batched(
    path: &Path,
    store: &mut Store,
    mut context: ProviderAdapterContext,
    import_options: NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    if context.source_path.is_none() {
        context.source_path = Some(path.to_path_buf());
    }
    let canonical_path = fs::canonicalize(path)?;
    let snapshot = astrbot_source_snapshot(path)?;
    let cursor_path = provider_path_identity(&canonical_path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::AstrBot,
        ASTRBOT_SQLITE_SOURCE_FORMAT,
        &cursor_path,
    );
    let conn = open_provider_sqlite_readonly(path)?;
    if !snapshot.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let _ = astrbot_conversation_columns(&conn)?;
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let schema_fingerprint = sqlite_schema_fingerprint(&conn)?;
    let selected_conversation = astrbot_selected_conversation_bounded(&conn)?;
    let source = SourceObservation::new(
        CaptureProvider::AstrBot,
        ASTRBOT_SQLITE_SOURCE_FORMAT,
        format!("astrbot-sqlite:{cursor_path}"),
        astrbot_source_revision(&snapshot, user_version, &schema_fingerprint),
        cursor_stream,
        ASTRBOT_CAPTURE_REVISION,
        ASTRBOT_POLICY_REVISION,
        import_options.inventory_observation_token.as_deref(),
    )
    .map_err(astrbot_captured_error)?;
    let stream = captured_batch_cursor_stream(&source);
    let expected_store_cursor = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let initial_position = initial_astrbot_position()?;
    let mut start_position = initial_position.clone();
    let mut parser_checkpoint = AstrBotParserCheckpoint::empty();
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
                parser_checkpoint = certified.parser_checkpoint().deserialize()?;
                parser_checkpoint.validate()?;
                decode_astrbot_position(certified.native_position())?;
                start_position = certified.native_position().clone();
            }
            Some(_) => cursor_mode = CapturedBatchCursorMode::ResetChangedSource,
            None => cursor_mode = CapturedBatchCursorMode::ReplaceLegacyCursor,
        }
    }
    let sql = AstrBotSql::new(&conn)?;
    if !parser_checkpoint.source_shape_validated {
        // AstrBotSql::new validated the bounded schema shape. Legacy ordering is
        // checked one row at a time at the producer frontier so a distant bad row
        // cannot force an eager source scan before the first batch.
        parser_checkpoint.source_shape_validated = true;
        BoundedParserCheckpoint::from_serializable(&parser_checkpoint)?;
    }
    if astrbot_relationship_projection_needed(&conn, &sql, &start_position)? {
        if !snapshot.revalidate(path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        astrbot_prepare_relationship_projection(&conn, &sql)?;
        if !snapshot.revalidate(path)? {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
    }
    let admission = CapturedSourceAdmission::conversation_for_context(&source, &context)?;
    let source_exhausted = Cell::new(false);
    let producer_source_exhausted = &source_exhausted;
    let mut fetcher = AstrBotRowFetcher::new(&conn, sql, parser_checkpoint)?;
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
    let mut projector = AstrBotCapturedBatchProjector {
        context: context.clone(),
        raw_source_path: path.display().to_string(),
        user_version,
        schema_fingerprint,
        selected_conversation,
        parser_checkpoint,
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
                    .map_err(astrbot_sqlite_batch_error)
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
