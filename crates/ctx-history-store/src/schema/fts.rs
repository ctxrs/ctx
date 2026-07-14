use rusqlite::Connection;

use crate::{Result, StoreError};

pub(crate) const FTS_TABLES_SQL: &str = r#"
CREATE VIRTUAL TABLE IF NOT EXISTS ctx_history_search USING fts5(
    record_id UNINDEXED,
    title,
    summary,
    primary_user_text,
    decision_text,
    context_text,
    tag_text
);

CREATE VIRTUAL TABLE IF NOT EXISTS event_search USING fts5(
    event_id UNINDEXED,
    history_record_id UNINDEXED,
    session_id UNINDEXED,
    role UNINDEXED,
    preview_text,
    rank_bucket UNINDEXED
);

CREATE VIRTUAL TABLE IF NOT EXISTS artifact_search USING fts5(
    artifact_id UNINDEXED,
    history_record_id UNINDEXED,
    preview_text
);

CREATE VIRTUAL TABLE IF NOT EXISTS ctx_history_search_scriptgram USING fts5(
    record_id UNINDEXED,
    token_text
);

CREATE VIRTUAL TABLE IF NOT EXISTS event_search_scriptgram USING fts5(
    event_id UNINDEXED,
    history_record_id UNINDEXED,
    session_id UNINDEXED,
    role UNINDEXED,
    token_text,
    rank_bucket UNINDEXED
);
"#;

pub(crate) fn execute_fts_ddl_if_supported(conn: &Connection, sql: &str) -> Result<()> {
    match conn.execute_batch(sql) {
        Ok(()) => Ok(()),
        Err(rusqlite::Error::SqliteFailure(error, message))
            if is_missing_fts_module(error.extended_code, message.as_deref()) =>
        {
            Ok(())
        }
        Err(err) => Err(StoreError::Sql(err)),
    }
}

pub(crate) fn drop_fts_table_if_exists(conn: &Connection, table: &str) -> Result<()> {
    if crate::schema::ddl::table_exists(conn, table)? {
        conn.execute(&format!("DROP TABLE {table}"), [])?;
    }
    Ok(())
}

fn is_missing_fts_module(extended_code: i32, message: Option<&str>) -> bool {
    extended_code == rusqlite::ffi::SQLITE_ERROR
        && message
            .map(|value| value.contains("no such module: fts5"))
            .unwrap_or(false)
}
