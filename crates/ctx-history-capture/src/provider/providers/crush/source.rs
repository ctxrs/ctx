use std::collections::BTreeSet;

use rusqlite::Connection;

use crate::provider::sqlite::{
    ensure_sqlite_table_columns, sqlite_table_columns, sqlite_table_exists,
};
use crate::{CaptureError, Result};

pub(super) fn session_columns(conn: &Connection) -> Result<BTreeSet<String>> {
    if !sqlite_table_exists(conn, "sessions")? {
        return Err(CaptureError::InvalidPayload(
            "Crush crush.db is missing required sessions table".into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "sessions")?;
    ensure_sqlite_table_columns(&columns, "Crush sessions table", &["id"])?;
    Ok(columns)
}

pub(super) fn message_columns(conn: &Connection) -> Result<BTreeSet<String>> {
    if !sqlite_table_exists(conn, "messages")? {
        return Err(CaptureError::InvalidPayload(
            "Crush crush.db is missing required messages table".into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "messages")?;
    ensure_sqlite_table_columns(
        &columns,
        "Crush messages table",
        &["id", "session_id", "role", "parts"],
    )?;
    Ok(columns)
}

fn optional_qualified(
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

pub(super) fn session_projection(columns: &BTreeSet<String>, alias: &str) -> String {
    let optional = |column, fallback| optional_qualified(columns, alias, column, fallback);
    format!(
        "{alias}.id, {}, {}, {}, {}, {}, {}, {}, {}",
        optional("parent_session_id", "NULL"),
        optional("title", "NULL"),
        optional("created_at", "NULL"),
        optional("updated_at", "NULL"),
        optional("prompt_tokens", "NULL"),
        optional("completion_tokens", "NULL"),
        optional("cost", "NULL"),
        optional("summary_message_id", "NULL"),
    )
}

pub(super) fn message_projection(columns: &BTreeSet<String>, alias: &str) -> String {
    let optional = |column, fallback| optional_qualified(columns, alias, column, fallback);
    format!(
        "{alias}.rowid, {alias}.id, {alias}.session_id, \
         {alias}.role, {alias}.parts, {}, {}, {}, {}, {}",
        optional("created_at", "NULL"),
        optional("updated_at", "NULL"),
        optional("provider", "NULL"),
        optional("model", "NULL"),
        optional("is_summary_message", "0"),
    )
}

pub(super) fn message_session_join() -> &'static str {
    "messages m left join sessions s \
     on typeof(m.session_id) = 'text' \
     and typeof(s.id) = 'text' \
     and s.id collate binary = m.session_id collate binary"
}

pub(super) fn retained_length_expr(
    columns: &BTreeSet<String>,
    alias: &str,
    projected_columns: &[&str],
) -> String {
    projected_columns
        .iter()
        .filter(|column| columns.contains(**column))
        .fold("0".to_owned(), |expression, column| {
            format!(
                "{expression} + case typeof({alias}.{column}) \
                 when 'null' then 0 when 'integer' then 8 when 'real' then 8 \
                 else coalesce(octet_length({alias}.{column}), 0) end"
            )
        })
}

pub(super) fn optional_session_column(columns: &BTreeSet<String>, column: &str) -> String {
    optional_qualified(columns, "s", column, "NULL")
}
