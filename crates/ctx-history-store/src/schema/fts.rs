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

/// Keeps the contentless event indexes consistent when the key they are
/// addressed by moves or disappears.
///
/// `events.seq` is not immutable per event id: it is derived from
/// `provider_event_sequence_index` while `events.id` is derived from the
/// separate `provider_event_index`, `avoid_provider_source_event_seq_collision`
/// reassigns it on collision, and the event upsert persists that with
/// `ON CONFLICT(id) DO UPDATE SET seq = excluded.seq`. A contentless row can
/// only be addressed by rowid, so a seq that moves without the old posting
/// being removed leaves that posting orphaned.
///
/// These triggers fire inside the same statement as the `events` write, which
/// `with_atomic_write` has already wrapped in `BEGIN IMMEDIATE` together with
/// the projection insert that follows. So the old posting's removal and the new
/// posting's insertion commit together or not at all: a crash cannot leave the
/// event indexed under a stale key, and - the direction that matters more - it
/// cannot leave the event unindexed either, because losing the insert also
/// rolls back the delete.
///
/// Doing this in SQL rather than in the Rust write path is deliberate. It
/// covers every writer of `events`, including migrations and repairs, and it
/// costs nothing on the cold path where events are inserted rather than
/// updated.
pub(crate) const EVENT_FTS_KEY_TRIGGERS_SQL: &str = r#"
CREATE TRIGGER IF NOT EXISTS ctx_event_search_rekey_on_seq_change
AFTER UPDATE OF seq ON events
WHEN old.seq IS NOT new.seq
BEGIN
    DELETE FROM event_search WHERE rowid = old.seq;
    DELETE FROM event_search_scriptgram WHERE rowid = old.seq;
END;

CREATE TRIGGER IF NOT EXISTS ctx_event_search_prune_on_event_delete
AFTER DELETE ON events
BEGIN
    DELETE FROM event_search WHERE rowid = old.seq;
    DELETE FROM event_search_scriptgram WHERE rowid = old.seq;
END;
"#;

/// The event FTS tables whose on-disk shape changed in schema v48.
pub(crate) const CONTENTLESS_EVENT_FTS_TABLES: [&str; 2] =
    ["event_search", "event_search_scriptgram"];

pub(crate) fn create_fts_tables_if_supported(conn: &Connection) -> Result<()> {
    if let Err(error) = conn.execute_batch(FTS_TABLES_SQL) {
        return match error {
            rusqlite::Error::SqliteFailure(code, message)
                if is_missing_fts_module(code.extended_code, message.as_deref()) =>
            {
                Ok(())
            }
            other => Err(StoreError::Sql(other)),
        };
    }
    // Only meaningful once the tables they reference exist.
    conn.execute_batch(EVENT_FTS_KEY_TRIGGERS_SQL)?;
    Ok(())
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
