use rusqlite::Connection;

use crate::provider::sqlite::{
    ensure_sqlite_table_columns, sqlite_table_columns, sqlite_table_exists,
};
use crate::{CaptureError, Result};

pub(super) struct WarpSqliteSchema {
    pub(super) task_keyset_index: String,
}

impl WarpSqliteSchema {
    pub(super) fn detect(conn: &Connection) -> Result<Self> {
        warp_validate_schema(conn)?;
        Ok(Self {
            task_keyset_index: warp_task_keyset_index(conn)?,
        })
    }
}

fn warp_validate_schema(conn: &Connection) -> Result<()> {
    if !sqlite_table_exists(conn, "agent_conversations")? {
        return Err(CaptureError::InvalidPayload(
            "Warp SQLite database is missing required agent_conversations table".into(),
        ));
    }
    let conversation_columns = sqlite_table_columns(conn, "agent_conversations")?;
    ensure_sqlite_table_columns(
        &conversation_columns,
        "Warp agent_conversations table",
        &["conversation_id", "conversation_data", "last_modified_at"],
    )?;
    if !sqlite_table_exists(conn, "agent_tasks")? {
        return Err(CaptureError::InvalidPayload(
            "Warp SQLite database is missing required agent_tasks table".into(),
        ));
    }
    let task_columns = sqlite_table_columns(conn, "agent_tasks")?;
    ensure_sqlite_table_columns(
        &task_columns,
        "Warp agent_tasks table",
        &["conversation_id", "task_id", "task", "last_modified_at"],
    )?;
    Ok(())
}

const WARP_INDEX_PROBE: ctx_history_source_sqlite::UniqueIndexProbe<'static> =
    ctx_history_source_sqlite::UniqueIndexProbe {
        provider: "Warp",
        max_rows: 64,
    };

pub(super) fn warp_task_keyset_index(conn: &Connection) -> Result<String> {
    let task_id_not_null: i64 = conn.query_row(
        "select count(*) from pragma_table_info('agent_tasks') \
         where name = 'task_id' and \"notnull\" = 1",
        [],
        |row| row.get(0),
    )?;
    if task_id_not_null != 1 {
        return Err(CaptureError::InvalidPayload(
            "Warp agent_tasks task_id must be declared NOT NULL for bounded keyset traversal"
                .to_owned(),
        ));
    }

    // Warp needs a single-column ascending UNIQUE BINARY index on task_id to
    // traverse tasks by keyset; the shared prober also bounds how much schema
    // it will inspect before refusing the source.
    ctx_history_source_sqlite::sqlite_unique_index_for_columns(
        conn,
        "agent_tasks",
        &["task_id"],
        &WARP_INDEX_PROBE,
    )?
    .ok_or_else(|| {
        CaptureError::InvalidPayload(
            "Warp agent_tasks requires a non-partial ascending UNIQUE BINARY index on task_id for bounded global keyset traversal"
                .to_owned(),
        )
    })
}

pub(super) fn warp_quote_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

#[cfg(test)]
#[path = "schema_tests.rs"]
mod tests;
