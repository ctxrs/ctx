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
mod complete_content;
mod normalization;
mod projection;
mod schema;

// Narrow provider API consumed by the source resolver after locator attachment.
#[allow(unused_imports)]
pub(crate) use normalization::opencode_normalized_result_content;

use capture::{
    initial_opencode_position, opencode_captured_error, opencode_sqlite_batch_error,
    validate_opencode_resume_position, OpenCodeRowFetcher, OPENCODE_RECORD_KIND,
};
use projection::{OpenCodeCapturedBatchProjector, OpenCodeProjectionSource};
use schema::opencode_captured_shape;
pub(crate) use schema::{
    OpenCodeCapturedShape, OpenCodeSqliteDialect, KILO_SQLITE_DIALECT, MIMOCODE_SQLITE_DIALECT,
    OPENCODE_SQLITE_DIALECT,
};

const OPENCODE_CAPTURE_REVISION: u32 = 6;
const OPENCODE_POLICY_REVISION: u32 = 7;

pub(crate) fn opencode_result_record(
    conn: &rusqlite::Connection,
    shape_tag: u8,
    rowid: i64,
) -> Result<Option<crate::complete_content::sqlite::SqliteResultRecord>> {
    let shape = OpenCodeCapturedShape::from_tag(shape_tag)?;
    let Some(values) = capture::opencode_values_at_rowid(conn, shape, rowid)? else {
        return Ok(None);
    };
    let text = |index: usize| match values.get(index) {
        Some(crate::captured_batch::CapturedSqliteValue::Text(value)) => Ok(value.as_str()),
        _ => Err(CaptureError::InvalidPayload(
            "OpenCode result logical row has an invalid text value".to_owned(),
        )),
    };
    let message_id = text(2)?;
    let source_table = text(13)?;
    let (native_record_id, entry_type, data) = if source_table == "message+part" {
        let part_id = text(11)?;
        let part_type = text(12)?;
        (
            format!("{message_id}:{part_id}"),
            if matches!(part_type, "tool" | "tool_result" | "result") {
                "tool".to_owned()
            } else {
                part_type.to_owned()
            },
            serde_json::from_str::<serde_json::Value>(text(10)?).map_err(|error| {
                CaptureError::InvalidPayload(format!(
                    "OpenCode result part is no longer valid JSON: {error}"
                ))
            })?,
        )
    } else {
        let data = serde_json::from_str::<serde_json::Value>(text(9)?).map_err(|error| {
            CaptureError::InvalidPayload(format!(
                "OpenCode result message is no longer valid JSON: {error}"
            ))
        })?;
        (
            message_id.to_owned(),
            normalization::opencode_entry_type_from_data(text(4)?, text(9)?),
            data,
        )
    };
    let content = normalization::opencode_normalized_result_content(&entry_type, &data)
        .ok_or_else(|| {
            CaptureError::InvalidPayload("OpenCode row is no longer a supported result".to_owned())
        })?;
    Ok(Some(crate::complete_content::sqlite::SqliteResultRecord {
        values,
        native_record_id,
        content,
    }))
}

pub(crate) fn load_opencode_message_values_schema(
    conn: &rusqlite::Connection,
    dialect: &OpenCodeSqliteDialect,
) -> Result<()> {
    schema::opencode_captured_shape(conn, dialect).map(|_| ())
}

pub(crate) fn load_opencode_message_values(
    conn: &rusqlite::Connection,
    dialect: &OpenCodeSqliteDialect,
    shape: OpenCodeCapturedShape,
    rowid: i64,
) -> Result<Vec<crate::captured_batch::CapturedSqliteValue>> {
    use crate::captured_batch::CapturedSqliteValue;

    if schema::opencode_captured_shape(conn, dialect)? != shape {
        return Err(CaptureError::InvalidPayload(
            "OpenCode locator shape no longer matches the selected provider schema".into(),
        ));
    }
    let sql = schema::OpenCodeRowSql::for_shape(conn, shape)?.hydration_sql(shape);
    let row = conn.query_row(&sql, [rowid], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, i64>(6)?,
            row.get::<_, String>(7)?,
            row.get::<_, String>(8)?,
            row.get::<_, String>(9)?,
            row.get::<_, String>(10)?,
            row.get::<_, String>(11)?,
        ))
    })?;
    let (
        message_id,
        mut session_id,
        entry_type,
        seq_present,
        seq,
        created,
        updated,
        message_data,
        part_data,
        part_id,
        part_type,
        source_table,
    ) = row;
    let relationship_valid = if shape == OpenCodeCapturedShape::MessagePart {
        let parent_session = conn.query_row(
            "select cast(session_id as text) from message where id = ?1 order by rowid limit 1",
            [message_id.as_str()],
            |row| row.get::<_, String>(0),
        )?;
        if session_id.trim().is_empty() {
            session_id = parent_session;
            true
        } else {
            session_id == parent_session
        }
    } else {
        !session_id.trim().is_empty()
    };
    Ok(vec![
        CapturedSqliteValue::Integer(0),
        CapturedSqliteValue::Integer(i64::from(relationship_valid)),
        CapturedSqliteValue::Text(message_id),
        CapturedSqliteValue::Text(session_id),
        CapturedSqliteValue::Text(entry_type),
        CapturedSqliteValue::Integer(seq_present),
        CapturedSqliteValue::Integer(seq),
        CapturedSqliteValue::Integer(created),
        CapturedSqliteValue::Integer(updated),
        CapturedSqliteValue::Text(message_data),
        CapturedSqliteValue::Text(part_data),
        CapturedSqliteValue::Text(part_id),
        CapturedSqliteValue::Text(part_type),
        CapturedSqliteValue::Text(source_table),
    ])
}

pub(crate) fn opencode_complete_message(
    values: &[crate::captured_batch::CapturedSqliteValue],
    dialect: &OpenCodeSqliteDialect,
) -> Result<(String, String, String)> {
    complete_content::opencode_complete_message(values, dialect)
}

pub(crate) fn decode_opencode_message_locator(
    locator: &crate::captured_batch::NativeLocator,
) -> Result<(OpenCodeCapturedShape, i64)> {
    capture::decode_opencode_message_locator(locator)
}

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
