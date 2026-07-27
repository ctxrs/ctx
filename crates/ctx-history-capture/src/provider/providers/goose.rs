use std::{cell::Cell, fs, path::Path};

use chrono::{DateTime, Utc};
use ctx_history_core::CaptureProvider;
use ctx_history_store::Store;

use crate::captured_batch::sqlite_logical_rows::SqliteLogicalRowBatchProducer;
use crate::provider::importer::{
    captured_batch_cursor_stream, drain_captured_batches, provider_path_identity,
    provider_source_cursor_stream_for_path, CapturedBatchCursorMode, CapturedSourceAdmission,
    CertifiedProviderCursor,
};
use crate::provider::sqlite::open_provider_sqlite_readonly;
use crate::provider::sqlite::{sqlite_schema_fingerprint, with_sqlite_read_snapshot};
use crate::{
    CaptureError, NormalizedProviderImportOptions, ProviderAdapterContext, ProviderImportSummary,
    Result, GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
};

mod normalization;
mod position;
mod projection;
mod schema;
mod source;
mod stream;

// Narrow provider API consumed by the source resolver after locator attachment.
#[allow(unused_imports)]
pub(crate) use normalization::goose_normalized_result_content;

use position::{decode_goose_position, initial_goose_position};
use projection::GooseCapturedBatchProjector;
use schema::goose_schema_version;
use source::{goose_source_observation, goose_source_snapshot};
use stream::{goose_sqlite_batch_error, GooseRowFetcher};

const GOOSE_CAPTURE_REVISION: u32 = 3;
const GOOSE_POLICY_REVISION: u32 = 5;

pub(crate) fn goose_result_record(
    conn: &rusqlite::Connection,
    rowid: i64,
) -> Result<Option<crate::complete_content::sqlite::SqliteResultRecord>> {
    let Some(values) = stream::goose_message_values_at_rowid(conn, rowid)? else {
        return Ok(None);
    };
    let (_, message) = schema::decode_goose_message_record(&values)?;
    let raw_content =
        serde_json::from_str::<serde_json::Value>(&message.content_json).map_err(|error| {
            CaptureError::InvalidPayload(format!(
                "Goose result content is no longer valid JSON: {error}"
            ))
        })?;
    let content =
        normalization::goose_normalized_result_content(&raw_content).ok_or_else(|| {
            CaptureError::InvalidPayload("Goose row is no longer a supported result".to_owned())
        })?;
    Ok(Some(crate::complete_content::sqlite::SqliteResultRecord {
        values,
        native_record_id: normalization::goose_message_identity(&message),
        content,
    }))
}

pub(crate) fn load_goose_message_values_schema(conn: &rusqlite::Connection) -> Result<()> {
    schema::goose_session_columns(conn)?;
    schema::goose_message_columns(conn)?;
    Ok(())
}

pub(crate) fn load_goose_message_values(
    conn: &rusqlite::Connection,
    rowid: i64,
) -> Result<Vec<crate::captured_batch::CapturedSqliteValue>> {
    let message_columns = schema::goose_message_columns(conn)?;
    let expressions = schema::goose_message_expressions(&message_columns, "m");
    let select = expressions.hydration.join(", ");
    let parent_rowid = conn.query_row(
        "select s.rowid from messages m left join sessions s on s.id = m.session_id \
             where m.rowid = ?1",
        [rowid],
        |row| row.get::<_, Option<i64>>(0),
    )?;
    let mut values = vec![parent_rowid.map_or(
        crate::captured_batch::CapturedSqliteValue::Null,
        crate::captured_batch::CapturedSqliteValue::Integer,
    )];
    values.extend(conn.query_row(
        &format!("select {select} from messages m where m.rowid = ?1"),
        [rowid],
        schema::goose_message_only_values,
    )?);
    Ok(values)
}

pub(crate) fn goose_complete_message(
    values: &[crate::captured_batch::CapturedSqliteValue],
) -> Result<(String, String, String)> {
    let (parent_rowid, message) = schema::decode_goose_message_record(values)?;
    if parent_rowid.is_none() {
        return Err(CaptureError::InvalidPayload(
            "Goose message parent is missing".into(),
        ));
    }
    let content: serde_json::Value = serde_json::from_str(&message.content_json)?;
    let text = normalization::goose_complete_content_text(&content)
        .unwrap_or_else(|| format!("Goose {} message", message.role));
    let identity = message
        .message_id
        .clone()
        .unwrap_or_else(|| format!("row-{}", message.id));
    Ok((message.session_id, identity, text))
}

pub(crate) fn import_goose_sessions_sqlite_batched(
    path: &Path,
    store: &mut Store,
    mut context: ProviderAdapterContext,
    import_options: NormalizedProviderImportOptions,
) -> Result<ProviderImportSummary> {
    context.source_path = Some(path.to_path_buf());
    let canonical_path = fs::canonicalize(path)?;
    let snapshot = goose_source_snapshot(path)?;
    let cursor_path = provider_path_identity(&canonical_path)?;
    let cursor_stream = provider_source_cursor_stream_for_path(
        CaptureProvider::Goose,
        GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
        &cursor_path,
    );
    let conn = open_provider_sqlite_readonly(path)?;
    if !snapshot.revalidate(path)? {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let schema_fingerprint = sqlite_schema_fingerprint(&conn)?;
    let schema_version = goose_schema_version(&conn)?;
    let source = goose_source_observation(
        &snapshot,
        &cursor_path,
        cursor_stream,
        user_version,
        schema_version,
        &schema_fingerprint,
        import_options.inventory_observation_token.as_deref(),
    )?;
    let stream = captured_batch_cursor_stream(&source);
    let expected_store_cursor = store.get_sync_cursor(None, &context.machine_id, &stream)?;
    let initial_position = initial_goose_position()?;
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
                decode_goose_position(certified.native_position())?;
                start_position = certified.native_position().clone();
            }
            Some(_) => cursor_mode = CapturedBatchCursorMode::ResetChangedSource,
            None => cursor_mode = CapturedBatchCursorMode::ReplaceLegacyCursor,
        }
    }
    let admission = CapturedSourceAdmission::conversation_for_context(&source, &context)?;
    let source_exhausted = Cell::new(false);
    let producer_source_exhausted = &source_exhausted;
    let mut fetcher = GooseRowFetcher::new(&conn)?;
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
    let mut projector = GooseCapturedBatchProjector::new(
        context.clone(),
        path.display().to_string(),
        user_version,
        schema_version,
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
                    .map_err(goose_sqlite_batch_error)
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

pub(crate) fn goose_timestamp(raw: Option<&str>, fallback: DateTime<Utc>) -> DateTime<Utc> {
    normalization::goose_timestamp(raw, fallback)
}

#[cfg(test)]
mod tests;
