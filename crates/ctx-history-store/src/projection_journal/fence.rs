use crate::Result;

const WRITER_FENCE_FUNCTION: &str = "ctx_projection_writer_authorized_v1";
const WRITER_FENCE_TABLES: [&str; 7] = [
    "events",
    "files_touched",
    "vcs_changes",
    "capture_sources",
    "sessions",
    "runs",
    "history_record_links",
];
const WRITER_FENCE_OPERATIONS: [&str; 3] = ["INSERT", "UPDATE", "DELETE"];

/// Reinstalls the database-enforced writer fence when a Store opens with an
/// already-active projection journal. This catches pre-existing local Pro
/// state created by a development build that predates the fence.
pub(crate) fn ensure_projection_writer_fence(conn: &rusqlite::Connection) -> Result<()> {
    let journal_active = conn.query_row(
        "SELECT active FROM projection_journal_state WHERE singleton = 1",
        [],
        |row| row.get::<_, i64>(0),
    )? != 0;
    if journal_active {
        install_projection_writer_fence(conn)?;
    } else {
        drop_projection_writer_fence(conn)?;
    }
    Ok(())
}

fn writer_fence_trigger_name(table: &str, operation: &str) -> String {
    format!(
        "ctx_projection_writer_fence_{table}_{}",
        operation.to_ascii_lowercase()
    )
}

pub(super) fn install_projection_writer_fence(conn: &rusqlite::Connection) -> Result<()> {
    for table in WRITER_FENCE_TABLES {
        for operation in WRITER_FENCE_OPERATIONS {
            let trigger = writer_fence_trigger_name(table, operation);
            conn.execute_batch(&format!(
                "CREATE TRIGGER IF NOT EXISTS {trigger}
                 BEFORE {operation} ON {table}
                 BEGIN
                   SELECT CASE WHEN {WRITER_FENCE_FUNCTION}() <> 1
                     THEN RAISE(ABORT, 'ctx projection journal requires a current writer') END;
                 END;"
            ))?;
        }
    }
    Ok(())
}

pub(super) fn drop_projection_writer_fence(conn: &rusqlite::Connection) -> Result<()> {
    for table in WRITER_FENCE_TABLES {
        for operation in WRITER_FENCE_OPERATIONS {
            let trigger = writer_fence_trigger_name(table, operation);
            conn.execute_batch(&format!("DROP TRIGGER IF EXISTS {trigger};"))?;
        }
    }
    Ok(())
}
