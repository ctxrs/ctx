use std::path::Path;

use ctx_history_store::Store;

use crate::native_source::{NativeLocator, NativeSqliteValue};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderImportOptions, ProviderImportSummary, Result,
};

mod complete_content;
mod content_locator;
mod native_path;
mod normalization;
mod schema;

pub(crate) use schema::{
    OpenCodeCapturedShape, OpenCodeSqliteDialect, KILO_SQLITE_DIALECT, MIMOCODE_SQLITE_DIALECT,
    OPENCODE_SQLITE_DIALECT,
};

pub(crate) fn opencode_result_record(
    conn: &rusqlite::Connection,
    shape_tag: u8,
    rowid: i64,
) -> Result<Option<crate::complete_content::sqlite::SqliteResultRecord>> {
    let shape = OpenCodeCapturedShape::from_tag(shape_tag)?;
    let Some(values) = content_locator::opencode_values_at_rowid(conn, shape, rowid)? else {
        return Ok(None);
    };
    let text = |index: usize| match values.get(index) {
        Some(NativeSqliteValue::Text(value)) => Ok(value.as_str()),
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
) -> Result<Vec<NativeSqliteValue>> {
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
        NativeSqliteValue::Integer(0),
        NativeSqliteValue::Integer(i64::from(relationship_valid)),
        NativeSqliteValue::Text(message_id),
        NativeSqliteValue::Text(session_id),
        NativeSqliteValue::Text(entry_type),
        NativeSqliteValue::Integer(seq_present),
        NativeSqliteValue::Integer(seq),
        NativeSqliteValue::Integer(created),
        NativeSqliteValue::Integer(updated),
        NativeSqliteValue::Text(message_data),
        NativeSqliteValue::Text(part_data),
        NativeSqliteValue::Text(part_id),
        NativeSqliteValue::Text(part_type),
        NativeSqliteValue::Text(source_table),
    ])
}

pub(crate) fn opencode_complete_message(
    values: &[NativeSqliteValue],
    dialect: &OpenCodeSqliteDialect,
) -> Result<(String, String, String)> {
    complete_content::opencode_complete_message(values, dialect)
}

pub(crate) fn decode_opencode_message_locator(
    locator: &NativeLocator,
) -> Result<(OpenCodeCapturedShape, i64)> {
    content_locator::decode_opencode_message_locator(locator)
}

pub(crate) fn import_opencode_nativepath(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    import_options: ProviderImportOptions,
    dialect: &OpenCodeSqliteDialect,
) -> Result<ProviderImportSummary> {
    native_path::vertical::import_opencode_nativepath(path, store, context, import_options, dialect)
}
