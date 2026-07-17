use rusqlite::Connection;

use crate::schema::ddl::table_exists;
use crate::Result;

/// Optional accelerator for empty stores. Upgraded stores rely on the triggers
/// below so enforcing new writes never requires a foreground corpus index.
pub(crate) const FRESH_STORE_PROVIDER_SESSION_UNIQUE_INDEX_SQL: &str = r#"
CREATE UNIQUE INDEX IF NOT EXISTS idx_sessions_unique_capture_source_external_session
ON sessions(capture_source_id, provider, external_session_id)
WHERE capture_source_id IS NOT NULL
  AND external_session_id IS NOT NULL
  AND deleted_at_ms IS NULL;
"#;

pub(crate) const PROVIDER_SESSION_INVARIANTS_SQL: &str = r#"
CREATE TRIGGER IF NOT EXISTS trg_sessions_provider_source_identity_insert
BEFORE INSERT ON sessions
WHEN NEW.capture_source_id IS NOT NULL
 AND NEW.external_session_id IS NOT NULL
 AND NEW.deleted_at_ms IS NULL
BEGIN
    SELECT RAISE(ABORT, 'duplicate provider session for capture source identity')
    WHERE EXISTS (
        SELECT 1
        FROM sessions existing
        JOIN capture_sources existing_source ON existing_source.id = existing.capture_source_id
        JOIN capture_sources incoming_source ON incoming_source.id = NEW.capture_source_id
        WHERE existing.id <> NEW.id
          AND existing.provider = NEW.provider
          AND existing.external_session_id = NEW.external_session_id
          AND existing.deleted_at_ms IS NULL
          AND (
              existing.capture_source_id = NEW.capture_source_id
              OR (
                  existing_source.source_identity IS NOT NULL
                  AND existing_source.source_identity = incoming_source.source_identity
              )
              OR (
                  existing_source.raw_source_path IS NOT NULL
                  AND existing_source.raw_source_path = incoming_source.raw_source_path
                  AND (
                      existing_source.source_format = incoming_source.source_format
                      OR existing_source.source_format IS NULL
                      OR incoming_source.source_format IS NULL
                  )
              )
          )
    );
END;

CREATE TRIGGER IF NOT EXISTS trg_sessions_provider_source_identity_update
BEFORE UPDATE OF capture_source_id, provider, external_session_id, deleted_at_ms ON sessions
WHEN NEW.capture_source_id IS NOT NULL
 AND NEW.external_session_id IS NOT NULL
 AND NEW.deleted_at_ms IS NULL
BEGIN
    SELECT RAISE(ABORT, 'duplicate provider session for capture source identity')
    WHERE EXISTS (
        SELECT 1
        FROM sessions existing
        JOIN capture_sources existing_source ON existing_source.id = existing.capture_source_id
        JOIN capture_sources incoming_source ON incoming_source.id = NEW.capture_source_id
        WHERE existing.id <> NEW.id
          AND existing.provider = NEW.provider
          AND existing.external_session_id = NEW.external_session_id
          AND existing.deleted_at_ms IS NULL
          AND (
              existing.capture_source_id = NEW.capture_source_id
              OR (
                  existing_source.source_identity IS NOT NULL
                  AND existing_source.source_identity = incoming_source.source_identity
              )
              OR (
                  existing_source.raw_source_path IS NOT NULL
                  AND existing_source.raw_source_path = incoming_source.raw_source_path
                  AND (
                      existing_source.source_format = incoming_source.source_format
                      OR existing_source.source_format IS NULL
                      OR incoming_source.source_format IS NULL
                  )
              )
          )
    );
END;
"#;

pub(crate) const DROP_PROVIDER_SESSION_INVARIANT_TRIGGERS_SQL: &str = r#"
DROP TRIGGER IF EXISTS trg_sessions_provider_source_identity_insert;
DROP TRIGGER IF EXISTS trg_sessions_provider_source_identity_update;
"#;

pub(crate) fn prepare_provider_session_migrations(
    conn: &Connection,
    user_version: i64,
) -> Result<()> {
    if user_version < 47 {
        conn.execute_batch(DROP_PROVIDER_SESSION_INVARIANT_TRIGGERS_SQL)?;
    }
    Ok(())
}

pub(crate) fn suspend_invariants_for_capture_source_rebuild(conn: &Connection) -> Result<bool> {
    let existed = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM sqlite_master
            WHERE type = 'trigger'
              AND name = 'trg_sessions_provider_source_identity_insert'
        )",
        [],
        |row| row.get::<_, i64>(0),
    )? != 0;
    if existed {
        conn.execute_batch(DROP_PROVIDER_SESSION_INVARIANT_TRIGGERS_SQL)?;
    }
    Ok(existed)
}

pub(crate) fn restore_invariants_after_capture_source_rebuild(
    conn: &Connection,
    restore: bool,
) -> Result<()> {
    if restore {
        conn.execute_batch(PROVIDER_SESSION_INVARIANTS_SQL)?;
    }
    Ok(())
}

pub(crate) fn backfill_capture_source_identity_columns(conn: &Connection) -> Result<()> {
    if !table_exists(conn, "capture_sources")? {
        return Ok(());
    }
    conn.execute(
        r#"
        UPDATE capture_sources
        SET source_root = raw_source_path
        WHERE source_root IS NULL
          AND raw_source_path IS NOT NULL
        "#,
        [],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invariant_sql_is_valid_on_an_empty_schema() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(crate::schema::ddl::CREATE_TABLES_SQL)
            .unwrap();
        conn.execute_batch(PROVIDER_SESSION_INVARIANTS_SQL).unwrap();
        conn.execute_batch(FRESH_STORE_PROVIDER_SESSION_UNIQUE_INDEX_SQL)
            .unwrap();
    }
}
