use std::{collections::BTreeSet, path::Path};

use rusqlite::Connection;

use crate::provider::sqlite::{
    ensure_sqlite_table_columns, sqlite_table_columns, sqlite_table_exists,
    ProviderSqliteSourceSnapshot, SqliteLengthPreflightGuard,
};
use crate::{CaptureError, Result};

use super::{SHELLEY_CAPTURE_REVISION, SHELLEY_POLICY_REVISION};

pub(super) fn shelley_source_snapshot(path: &Path) -> Result<ProviderSqliteSourceSnapshot> {
    ProviderSqliteSourceSnapshot::read(
        path,
        "Shelley SQLite source must be a regular non-symlink file",
        "Shelley SQLite sidecar must be a regular non-symlink file",
    )
}

pub(super) fn shelley_source_revision(
    snapshot: &ProviderSqliteSourceSnapshot,
    user_version: i64,
    schema_fingerprint: &str,
) -> String {
    format!(
        "shelley-sqlite-snapshot-v1:capture={SHELLEY_CAPTURE_REVISION};policy={SHELLEY_POLICY_REVISION};user_version={user_version};schema={schema_fingerprint};{}",
        snapshot.revision_component(),
    )
}

pub(crate) fn shelley_conversation_columns(conn: &Connection) -> Result<BTreeSet<String>> {
    if !sqlite_table_exists(conn, "conversations")? {
        return Err(CaptureError::InvalidPayload(
            "Shelley shelley.db is missing required conversations table".into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "conversations")?;
    ensure_sqlite_table_columns(
        &columns,
        "Shelley conversations table",
        &["conversation_id"],
    )?;
    Ok(columns)
}

pub(crate) fn shelley_message_columns(conn: &Connection) -> Result<BTreeSet<String>> {
    if !sqlite_table_exists(conn, "messages")? {
        return Err(CaptureError::InvalidPayload(
            "Shelley shelley.db is missing required messages table".into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "messages")?;
    ensure_sqlite_table_columns(
        &columns,
        "Shelley messages table",
        &["message_id", "conversation_id", "type"],
    )?;
    Ok(columns)
}

pub(super) fn shelley_require_message_index(conn: &Connection, has_sequence: bool) -> Result<()> {
    let mut indexes = conn.prepare(
        "select name from pragma_index_list('messages') where partial = 0 order by name",
    )?;
    let index_names = indexes
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut index_columns = conn.prepare(
        "select name, desc, coll from pragma_index_xinfo(?1)
         where key = 1 order by seqno",
    )?;
    // Migration 002 has the conversation-only index; migration 003 introduces sequence_id.
    let expected_columns: &[&str] = if has_sequence {
        &["conversation_id", "sequence_id"]
    } else {
        &["conversation_id"]
    };
    for index_name in index_names {
        let columns = index_columns
            .query_map([index_name], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let compatible = columns.len() == expected_columns.len()
            && columns
                .iter()
                .zip(expected_columns)
                .all(|((name, _, _), expected)| name.as_deref() == Some(*expected))
            && columns.iter().all(|(_, descending, collation)| {
                *descending == 0
                    && collation
                        .as_deref()
                        .is_some_and(|value| value.eq_ignore_ascii_case("binary"))
            });
        if compatible {
            return Ok(());
        }
    }
    let expected_index = expected_columns.join(", ");
    Err(CaptureError::InvalidPayload(
        format!(
            "Shelley messages table requires a non-partial ascending BINARY index on ({expected_index})"
        ),
    ))
}

pub(super) fn shelley_qualified_optional_column(
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

pub(crate) fn shelley_conversation_select_expressions(
    columns: &BTreeSet<String>,
    alias: &str,
) -> Vec<String> {
    [
        format!("{alias}.rowid"),
        format!("{alias}.conversation_id"),
        shelley_qualified_optional_column(columns, alias, "slug", "NULL"),
        shelley_qualified_optional_column(columns, alias, "user_initiated", "1"),
        shelley_qualified_optional_column(columns, alias, "created_at", "NULL"),
        shelley_qualified_optional_column(columns, alias, "updated_at", "NULL"),
        shelley_qualified_optional_column(columns, alias, "cwd", "NULL"),
        shelley_qualified_optional_column(columns, alias, "archived", "0"),
        shelley_qualified_optional_column(columns, alias, "parent_conversation_id", "NULL"),
        shelley_qualified_optional_column(columns, alias, "model", "NULL"),
        shelley_qualified_optional_column(columns, alias, "conversation_options", "NULL"),
        shelley_qualified_optional_column(columns, alias, "current_generation", "NULL"),
        shelley_qualified_optional_column(columns, alias, "agent_working", "0"),
        shelley_qualified_optional_column(columns, alias, "tags", "NULL"),
        shelley_qualified_optional_column(columns, alias, "is_draft", "0"),
        shelley_qualified_optional_column(columns, alias, "draft", "NULL"),
        shelley_qualified_optional_column(columns, alias, "queued_messages", "NULL"),
    ]
    .into_iter()
    .collect()
}

pub(crate) fn shelley_message_select_expressions(
    columns: &BTreeSet<String>,
    alias: &str,
) -> Vec<String> {
    [
        format!("{alias}.rowid"),
        format!("{alias}.message_id"),
        format!("{alias}.conversation_id"),
        shelley_qualified_optional_column(columns, alias, "sequence_id", &format!("{alias}.rowid")),
        format!("{alias}.type"),
        shelley_qualified_optional_column(columns, alias, "llm_data", "NULL"),
        shelley_qualified_optional_column(columns, alias, "user_data", "NULL"),
        shelley_qualified_optional_column(columns, alias, "usage_data", "NULL"),
        shelley_qualified_optional_column(columns, alias, "created_at", "NULL"),
        shelley_qualified_optional_column(columns, alias, "display_data", "NULL"),
        shelley_qualified_optional_column(columns, alias, "excluded_from_context", "0"),
        shelley_qualified_optional_column(columns, alias, "generation", "NULL"),
        shelley_qualified_optional_column(columns, alias, "llm_api_url", "NULL"),
        shelley_qualified_optional_column(columns, alias, "model_name", "NULL"),
        shelley_qualified_optional_column(columns, alias, "forked_from_message_id", "NULL"),
    ]
    .into_iter()
    .collect()
}

pub(super) fn with_shelley_length_preflight<T>(
    conn: &Connection,
    query: impl FnOnce() -> rusqlite::Result<T>,
) -> Result<T> {
    // SQLITE_LIMIT_LENGTH rejects even integer-only octet_length inspection of
    // an oversized stored record. The preflight SQL returns no raw TEXT/BLOB;
    // restore the provider cap before any hydration statement can run.
    let _guard = SqliteLengthPreflightGuard::new(conn);
    query().map_err(CaptureError::from)
}

pub(super) fn shelley_retained_length_expr(expressions: &[String]) -> String {
    // Unlike a cast to BLOB, octet_length can inspect large TEXT/BLOB columns without
    // materializing them through the bounded connection's SQLITE_LIMIT_LENGTH.
    expressions
        .iter()
        .map(|expression| format!("coalesce(octet_length({expression}), 0)"))
        .collect::<Vec<_>>()
        .join(" + ")
}
