use rusqlite::Connection;

use crate::{Result, StoreError};

/// `event_search` and `event_search_scriptgram` are contentless FTS5 tables
/// keyed by `events.seq`.
///
/// Contentless means FTS5 keeps only the inverted index and no private copy of
/// the indexed text. On a real 4.23 GB store that removes the 510 MB
/// `event_search_content` table outright; the hit path recovers `preview_text`
/// by re-deriving it from `events.payload_json`, which it already reads and
/// already parses for cursor extraction.
///
/// The rowid is `events.seq`, not the dense implicit `events.rowid`. FTS5
/// delta-encodes rowids as varints, so the sparse key costs about 56 MiB more
/// index, but the implicit rowid is renumbered by `VACUUM`, which would
/// silently desync every posting with no error surfaced anywhere. The bytes are
/// the cheaper side of that trade.
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
    preview_text,
    content='',
    contentless_delete=1
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
    token_text,
    content='',
    contentless_delete=1
);
"#;

/// The event FTS tables whose on-disk shape changed in schema v48.
pub(crate) const CONTENTLESS_EVENT_FTS_TABLES: [&str; 2] =
    ["event_search", "event_search_scriptgram"];

pub(crate) fn create_fts_tables_if_supported(conn: &Connection) -> Result<()> {
    match conn.execute_batch(FTS_TABLES_SQL) {
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

/// True when `table` exists and already has the contentless shape.
///
/// A contentless FTS5 table has no `%_content` shadow table, which is the
/// cheapest durable signal that the v48 rebuild has run. The declared column
/// list alone would be ambiguous, since the pre-v48 tables also carry a
/// `preview_text` column.
pub(crate) fn event_fts_table_is_contentless(conn: &Connection, table: &str) -> Result<bool> {
    if !crate::schema::ddl::table_exists(conn, table)? {
        return Ok(false);
    }
    Ok(!crate::schema::ddl::table_exists(
        conn,
        &format!("{table}_content"),
    )?)
}

fn is_missing_fts_module(extended_code: i32, message: Option<&str>) -> bool {
    extended_code == rusqlite::ffi::SQLITE_ERROR
        && message
            .map(|value| value.contains("no such module: fts5"))
            .unwrap_or(false)
}
