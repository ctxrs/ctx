use std::collections::BTreeSet;

use rusqlite::Connection;

use crate::captured_batch::CapturedSqliteValue;
use crate::provider::sqlite::{
    ensure_sqlite_table_columns, sqlite_table_columns, sqlite_table_exists,
};
use crate::{CaptureError, Result};

pub(super) const GOOSE_MESSAGE_RECORD_KIND: &str = "goose-message-v3";
pub(super) const GOOSE_SESSION_RECORD_KIND: &str = "goose-session-v3";
pub(super) const GOOSE_MESSAGE_VALUE_COUNT: usize = 10;
pub(super) const GOOSE_SESSION_VALUE_COUNT: usize = 21;

#[derive(Debug, Clone)]
pub(super) struct GooseSessionRow {
    pub(super) id: String,
    pub(super) name: Option<String>,
    pub(super) description: Option<String>,
    pub(super) user_set_name: bool,
    pub(super) session_type: Option<String>,
    pub(super) working_dir: Option<String>,
    pub(super) created_at: Option<String>,
    pub(super) updated_at: Option<String>,
    pub(super) extension_data: Option<String>,
    pub(super) total_tokens: Option<i64>,
    pub(super) input_tokens: Option<i64>,
    pub(super) output_tokens: Option<i64>,
    pub(super) accumulated_total_tokens: Option<i64>,
    pub(super) accumulated_input_tokens: Option<i64>,
    pub(super) accumulated_output_tokens: Option<i64>,
    pub(super) accumulated_cost: Option<f64>,
    pub(super) provider_name: Option<String>,
    pub(super) model_config_json: Option<String>,
    pub(super) goose_mode: Option<String>,
    pub(super) archived_at: Option<String>,
    pub(super) project_id: Option<String>,
}

impl GooseSessionRow {
    pub(super) fn event_reference(session_id: &str) -> Self {
        Self {
            id: session_id.to_owned(),
            name: None,
            description: None,
            user_set_name: false,
            session_type: None,
            working_dir: None,
            created_at: None,
            updated_at: None,
            extension_data: None,
            total_tokens: None,
            input_tokens: None,
            output_tokens: None,
            accumulated_total_tokens: None,
            accumulated_input_tokens: None,
            accumulated_output_tokens: None,
            accumulated_cost: None,
            provider_name: None,
            model_config_json: None,
            goose_mode: None,
            archived_at: None,
            project_id: None,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct GooseMessageRow {
    pub(super) rowid: i64,
    pub(super) id: i64,
    pub(super) message_id: Option<String>,
    pub(super) session_id: String,
    pub(super) role: String,
    pub(super) content_json: String,
    pub(super) created_timestamp: Option<i64>,
    pub(super) timestamp: Option<String>,
    pub(super) tokens: Option<String>,
    pub(super) metadata_json: Option<String>,
}
pub(super) fn goose_session_columns(conn: &Connection) -> Result<BTreeSet<String>> {
    if !sqlite_table_exists(conn, "sessions")? {
        return Err(CaptureError::InvalidPayload(
            "Goose sessions.db is missing required sessions table".into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "sessions")?;
    ensure_sqlite_table_columns(&columns, "Goose sessions table", &["id"])?;
    Ok(columns)
}

pub(super) fn goose_message_columns(conn: &Connection) -> Result<BTreeSet<String>> {
    if !sqlite_table_exists(conn, "messages")? {
        return Err(CaptureError::InvalidPayload(
            "Goose sessions.db is missing required messages table".into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "messages")?;
    ensure_sqlite_table_columns(
        &columns,
        "Goose messages table",
        &["session_id", "role", "content_json"],
    )?;
    Ok(columns)
}

fn goose_qualified_optional_column(
    columns: &BTreeSet<String>,
    alias: &str,
    column: &str,
    fallback: &str,
) -> String {
    if columns.contains(column) {
        format!("{alias}.{column}")
    } else {
        fallback.to_owned()
    }
}

struct GooseSqlFieldExpressions {
    hydration: String,
    retained: String,
}

impl GooseSqlFieldExpressions {
    fn same(expression: String) -> Self {
        Self {
            hydration: expression.clone(),
            retained: expression,
        }
    }

    fn distinct(hydration: String, retained: String) -> Self {
        Self {
            hydration,
            retained,
        }
    }
}

pub(super) struct GooseSqlExpressions {
    pub(super) hydration: Vec<String>,
    pub(super) retained: Vec<String>,
}

fn goose_sql_expressions<const N: usize>(
    fields: [GooseSqlFieldExpressions; N],
) -> GooseSqlExpressions {
    let (hydration, retained) = fields
        .into_iter()
        .map(|field| (field.hydration, field.retained))
        .unzip();
    GooseSqlExpressions {
        hydration,
        retained,
    }
}

fn goose_optional_field(
    columns: &BTreeSet<String>,
    alias: &str,
    column: &str,
    fallback: &str,
) -> GooseSqlFieldExpressions {
    GooseSqlFieldExpressions::same(goose_qualified_optional_column(
        columns, alias, column, fallback,
    ))
}

pub(super) fn goose_session_expressions(
    columns: &BTreeSet<String>,
    alias: &str,
) -> GooseSqlExpressions {
    goose_sql_expressions([
        GooseSqlFieldExpressions::distinct(
            format!("CAST({alias}.id AS TEXT)"),
            format!("{alias}.id"),
        ),
        goose_optional_field(columns, alias, "name", "NULL"),
        goose_optional_field(columns, alias, "description", "NULL"),
        goose_optional_field(columns, alias, "user_set_name", "0"),
        goose_optional_field(columns, alias, "session_type", "NULL"),
        goose_optional_field(columns, alias, "working_dir", "NULL"),
        goose_optional_field(columns, alias, "created_at", "NULL"),
        goose_optional_field(columns, alias, "updated_at", "NULL"),
        goose_optional_field(columns, alias, "extension_data", "NULL"),
        goose_optional_field(columns, alias, "total_tokens", "NULL"),
        goose_optional_field(columns, alias, "input_tokens", "NULL"),
        goose_optional_field(columns, alias, "output_tokens", "NULL"),
        goose_optional_field(columns, alias, "accumulated_total_tokens", "NULL"),
        goose_optional_field(columns, alias, "accumulated_input_tokens", "NULL"),
        goose_optional_field(columns, alias, "accumulated_output_tokens", "NULL"),
        goose_optional_field(columns, alias, "accumulated_cost", "NULL"),
        goose_optional_field(columns, alias, "provider_name", "NULL"),
        goose_optional_field(columns, alias, "model_config_json", "NULL"),
        goose_optional_field(columns, alias, "goose_mode", "NULL"),
        goose_optional_field(columns, alias, "archived_at", "NULL"),
        goose_optional_field(columns, alias, "project_id", "NULL"),
    ])
}

pub(super) fn goose_message_expressions(
    columns: &BTreeSet<String>,
    alias: &str,
) -> GooseSqlExpressions {
    let id = if columns.contains("id") {
        format!("{alias}.id")
    } else {
        format!("{alias}.rowid")
    };
    let tokens = if columns.contains("tokens") {
        GooseSqlFieldExpressions::distinct(
            format!("CAST({alias}.tokens AS TEXT)"),
            format!("{alias}.tokens"),
        )
    } else {
        GooseSqlFieldExpressions::same("NULL".to_owned())
    };
    goose_sql_expressions([
        GooseSqlFieldExpressions::same(format!("{alias}.rowid")),
        GooseSqlFieldExpressions::same(id),
        goose_optional_field(columns, alias, "message_id", "NULL"),
        GooseSqlFieldExpressions::distinct(
            format!("CAST({alias}.session_id AS TEXT)"),
            format!("{alias}.session_id"),
        ),
        GooseSqlFieldExpressions::same(format!("{alias}.role")),
        GooseSqlFieldExpressions::same(format!("{alias}.content_json")),
        goose_optional_field(columns, alias, "created_timestamp", "NULL"),
        goose_optional_field(columns, alias, "timestamp", "NULL"),
        tokens,
        goose_optional_field(columns, alias, "metadata_json", "NULL"),
    ])
}

pub(super) fn goose_message_only_values(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<Vec<CapturedSqliteValue>> {
    Ok(vec![
        CapturedSqliteValue::Integer(row.get(0)?),
        CapturedSqliteValue::Integer(row.get(1)?),
        goose_optional_text_value(row.get(2)?),
        CapturedSqliteValue::Text(row.get(3)?),
        CapturedSqliteValue::Text(row.get(4)?),
        CapturedSqliteValue::Text(row.get(5)?),
        goose_optional_integer_value(row.get(6)?),
        goose_optional_text_value(row.get(7)?),
        goose_optional_text_value(row.get(8)?),
        goose_optional_text_value(row.get(9)?),
    ])
}

pub(super) fn goose_session_values(
    row: &rusqlite::Row<'_>,
    offset: usize,
) -> rusqlite::Result<Vec<CapturedSqliteValue>> {
    Ok(vec![
        goose_optional_text_value(row.get(offset)?),
        goose_optional_text_value(row.get(offset + 1)?),
        goose_optional_text_value(row.get(offset + 2)?),
        goose_optional_integer_value(row.get(offset + 3)?),
        goose_optional_text_value(row.get(offset + 4)?),
        goose_optional_text_value(row.get(offset + 5)?),
        goose_optional_text_value(row.get(offset + 6)?),
        goose_optional_text_value(row.get(offset + 7)?),
        goose_optional_text_value(row.get(offset + 8)?),
        goose_optional_integer_value(row.get(offset + 9)?),
        goose_optional_integer_value(row.get(offset + 10)?),
        goose_optional_integer_value(row.get(offset + 11)?),
        goose_optional_integer_value(row.get(offset + 12)?),
        goose_optional_integer_value(row.get(offset + 13)?),
        goose_optional_integer_value(row.get(offset + 14)?),
        goose_optional_real_value(row.get(offset + 15)?),
        goose_optional_text_value(row.get(offset + 16)?),
        goose_optional_text_value(row.get(offset + 17)?),
        goose_optional_text_value(row.get(offset + 18)?),
        goose_optional_text_value(row.get(offset + 19)?),
        goose_optional_text_value(row.get(offset + 20)?),
    ])
}

fn goose_optional_text_value(value: Option<String>) -> CapturedSqliteValue {
    value.map_or(CapturedSqliteValue::Null, CapturedSqliteValue::Text)
}

fn goose_optional_integer_value(value: Option<i64>) -> CapturedSqliteValue {
    value.map_or(CapturedSqliteValue::Null, CapturedSqliteValue::Integer)
}

fn goose_optional_real_value(value: Option<f64>) -> CapturedSqliteValue {
    value.map_or(CapturedSqliteValue::Null, CapturedSqliteValue::from_real)
}

pub(super) fn decode_goose_message_record(
    values: &[CapturedSqliteValue],
) -> Result<(Option<i64>, GooseMessageRow)> {
    if values.len() != GOOSE_MESSAGE_VALUE_COUNT + 1 {
        return Err(CaptureError::InvalidPayload(
            "Goose message logical row has an unexpected value count".to_owned(),
        ));
    }
    let parent_rowid = goose_optional_integer(values, 0, "message parent rowid")?;
    let message = GooseMessageRow {
        rowid: goose_required_integer(values, 1, "message rowid")?,
        id: goose_required_integer(values, 2, "message id")?,
        message_id: goose_optional_text(values, 3, "message_id")?,
        session_id: goose_required_text(values, 4, "message session_id")?,
        role: goose_required_text(values, 5, "message role")?,
        content_json: goose_required_text(values, 6, "message content_json")?,
        created_timestamp: goose_optional_integer(values, 7, "message created_timestamp")?,
        timestamp: goose_optional_text(values, 8, "message timestamp")?,
        tokens: goose_optional_text(values, 9, "message tokens")?,
        metadata_json: goose_optional_text(values, 10, "message metadata_json")?,
    };
    Ok((parent_rowid, message))
}

pub(super) fn decode_goose_session(values: &[CapturedSqliteValue]) -> Result<GooseSessionRow> {
    if values.len() != GOOSE_SESSION_VALUE_COUNT {
        return Err(CaptureError::InvalidPayload(
            "Goose session logical row has an unexpected value count".to_owned(),
        ));
    }
    Ok(GooseSessionRow {
        id: goose_required_text(values, 0, "session id")?,
        name: goose_optional_text(values, 1, "session name")?,
        description: goose_optional_text(values, 2, "session description")?,
        user_set_name: goose_optional_integer(values, 3, "session user_set_name")?
            .is_some_and(|value| value != 0),
        session_type: goose_optional_text(values, 4, "session_type")?,
        working_dir: goose_optional_text(values, 5, "session working_dir")?,
        created_at: goose_optional_text(values, 6, "session created_at")?,
        updated_at: goose_optional_text(values, 7, "session updated_at")?,
        extension_data: goose_optional_text(values, 8, "session extension_data")?,
        total_tokens: goose_optional_integer(values, 9, "session total_tokens")?,
        input_tokens: goose_optional_integer(values, 10, "session input_tokens")?,
        output_tokens: goose_optional_integer(values, 11, "session output_tokens")?,
        accumulated_total_tokens: goose_optional_integer(
            values,
            12,
            "session accumulated_total_tokens",
        )?,
        accumulated_input_tokens: goose_optional_integer(
            values,
            13,
            "session accumulated_input_tokens",
        )?,
        accumulated_output_tokens: goose_optional_integer(
            values,
            14,
            "session accumulated_output_tokens",
        )?,
        accumulated_cost: goose_optional_real(values, 15, "session accumulated_cost")?,
        provider_name: goose_optional_text(values, 16, "session provider_name")?,
        model_config_json: goose_optional_text(values, 17, "session model_config_json")?,
        goose_mode: goose_optional_text(values, 18, "session goose_mode")?,
        archived_at: goose_optional_text(values, 19, "session archived_at")?,
        project_id: goose_optional_text(values, 20, "session project_id")?,
    })
}

fn goose_value<'a>(
    values: &'a [CapturedSqliteValue],
    index: usize,
    field: &str,
) -> Result<&'a CapturedSqliteValue> {
    values.get(index).ok_or_else(|| {
        CaptureError::InvalidPayload(format!("Goose logical row is missing {field}"))
    })
}

fn goose_required_text(
    values: &[CapturedSqliteValue],
    index: usize,
    field: &str,
) -> Result<String> {
    match goose_value(values, index, field)? {
        CapturedSqliteValue::Text(value) => Ok(value.clone()),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Goose logical row {field} must be text"
        ))),
    }
}

fn goose_optional_text(
    values: &[CapturedSqliteValue],
    index: usize,
    field: &str,
) -> Result<Option<String>> {
    match goose_value(values, index, field)? {
        CapturedSqliteValue::Null => Ok(None),
        CapturedSqliteValue::Text(value) => Ok(Some(value.clone())),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Goose logical row {field} must be text or null"
        ))),
    }
}

fn goose_required_integer(
    values: &[CapturedSqliteValue],
    index: usize,
    field: &str,
) -> Result<i64> {
    match goose_value(values, index, field)? {
        CapturedSqliteValue::Integer(value) => Ok(*value),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Goose logical row {field} must be an integer"
        ))),
    }
}

fn goose_optional_integer(
    values: &[CapturedSqliteValue],
    index: usize,
    field: &str,
) -> Result<Option<i64>> {
    match goose_value(values, index, field)? {
        CapturedSqliteValue::Null => Ok(None),
        CapturedSqliteValue::Integer(value) => Ok(Some(*value)),
        _ => Err(CaptureError::InvalidPayload(format!(
            "Goose logical row {field} must be an integer or null"
        ))),
    }
}

fn goose_optional_real(
    values: &[CapturedSqliteValue],
    index: usize,
    field: &str,
) -> Result<Option<f64>> {
    match goose_value(values, index, field)? {
        CapturedSqliteValue::Null => Ok(None),
        value => value.as_real().map(Some).ok_or_else(|| {
            CaptureError::InvalidPayload(format!("Goose logical row {field} must be real or null"))
        }),
    }
}

pub(super) fn goose_schema_version(conn: &Connection) -> Result<Option<i64>> {
    if !sqlite_table_exists(conn, "schema_version")? {
        return Ok(None);
    }
    let columns = sqlite_table_columns(conn, "schema_version")?;
    let version_column = if columns.contains("version") {
        "version"
    } else if columns.contains("id") {
        "id"
    } else {
        return Ok(None);
    };
    let sql = format!("select max({version_column}) from schema_version");
    conn.query_row(&sql, [], |row| row.get::<_, Option<i64>>(0))
        .map_err(CaptureError::from)
}
