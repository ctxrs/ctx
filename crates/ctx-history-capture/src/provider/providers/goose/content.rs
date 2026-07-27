use rusqlite::{Connection, OptionalExtension};

use ctx_history_core::CaptureProvider;

use crate::{
    native_source::{NativeLocator, NativeSqliteValue},
    CaptureError, Result, GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
};

use super::{normalization, position::goose_message_locator, schema};

pub(super) fn result_record(
    conn: &Connection,
    rowid: i64,
) -> Result<Option<crate::complete_content::sqlite::SqliteResultRecord>> {
    let Some(values) = message_values_at_rowid(conn, rowid)? else {
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

pub(super) fn load_schema(conn: &Connection) -> Result<()> {
    schema::goose_session_columns(conn)?;
    schema::goose_message_columns(conn)?;
    Ok(())
}

pub(super) fn load_message_values(conn: &Connection, rowid: i64) -> Result<Vec<NativeSqliteValue>> {
    message_values_at_rowid(conn, rowid)?.ok_or_else(|| {
        CaptureError::InvalidPayload(format!("Goose message row {rowid} is missing"))
    })
}

pub(super) fn complete_message(values: &[NativeSqliteValue]) -> Result<(String, String, String)> {
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

pub(super) fn attach_message_locator(
    conn: &Connection,
    rowid: i64,
    native_record_id: &str,
    payload: &serde_json::Value,
    metadata: &mut serde_json::Value,
    complete_text: String,
) -> Result<()> {
    let Some(values) = message_values_at_rowid(conn, rowid)? else {
        return Err(CaptureError::InvalidPayload(format!(
            "Goose retained message row {rowid} disappeared from its immutable snapshot"
        )));
    };
    let (kind, value) = goose_message_locator(rowid);
    let locator = NativeLocator::new(kind, value)
        .map_err(|error| CaptureError::InvalidPayload(error.to_string()))?;
    crate::complete_content::sqlite::attach_sqlite_complete_content_locator(
        CaptureProvider::Goose,
        GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
        native_record_id,
        payload,
        metadata,
        &locator,
        &values,
        || complete_text,
    )
}

fn message_values_at_rowid(
    conn: &Connection,
    rowid: i64,
) -> Result<Option<Vec<NativeSqliteValue>>> {
    let columns = schema::goose_message_columns(conn)?;
    let expressions = schema::goose_message_expressions(&columns, "m");
    let select = expressions.hydration.join(", ");
    conn.query_row(
        &format!(
            "select s.rowid, {select} from messages m \
             left join sessions s on s.id = m.session_id where m.rowid = ?1"
        ),
        [rowid],
        |row| {
            let mut values = vec![row
                .get::<_, Option<i64>>(0)?
                .map_or(NativeSqliteValue::Null, NativeSqliteValue::Integer)];
            values.extend(schema::goose_message_values_at(row, 1)?);
            Ok(values)
        },
    )
    .optional()
    .map_err(CaptureError::from)
}
