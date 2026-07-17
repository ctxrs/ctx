pub(crate) mod ddl;
pub(crate) mod fts;
pub(crate) mod import_pending_work;
pub(crate) mod indexes;
pub(crate) mod migrations;
pub(crate) mod provider_session_identity;
pub(crate) mod rebuild;
pub(crate) mod scriptgram;
#[cfg(test)]
mod tests;
pub(crate) mod views;
pub(crate) mod writer_fence;

use rusqlite::Connection;

use crate::connection::configure_connection;
use crate::schema::indexes::{
    BASELINE_INDEXES_SQL, FRESH_STORE_OPTIMIZED_INDEXES_SQL, REPAIR_LEDGER_INITIALIZATION_SQL,
};
use crate::{Result, Store, StoreError, SCHEMA_VERSION};

pub(crate) use fts::create_fts_tables_if_supported;

pub(crate) fn migrate_to_latest(conn: &Connection) -> Result<()> {
    let user_version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if user_version > SCHEMA_VERSION {
        return Err(StoreError::UnsupportedSchemaVersion(user_version));
    }
    let fresh_empty_store = user_version == 0
        && conn.query_row(
            "SELECT NOT EXISTS (\
               SELECT 1 FROM sqlite_schema \
               WHERE type = 'table' AND name NOT LIKE 'sqlite_%'\
             )",
            [],
            |row| row.get::<_, bool>(0),
        )?;
    migrations::run_migrations(conn, user_version)?;
    conn.execute_batch(provider_session_identity::PROVIDER_SESSION_INVARIANTS_SQL)?;
    if fresh_empty_store {
        conn.execute(
            "UPDATE import_pending_work_state SET selection_mode = 'direct' WHERE singleton = 1",
            [],
        )?;
    }
    import_pending_work::install_import_pending_work_invariants(conn)?;
    create_fts_tables_if_supported(conn)?;
    conn.execute_batch(BASELINE_INDEXES_SQL)?;
    conn.execute_batch(REPAIR_LEDGER_INITIALIZATION_SQL)?;
    if fresh_empty_store {
        conn.execute_batch(FRESH_STORE_OPTIMIZED_INDEXES_SQL)?;
        conn.execute_batch(
            provider_session_identity::FRESH_STORE_PROVIDER_SESSION_UNIQUE_INDEX_SQL,
        )?;
    }
    Ok(())
}

impl Store {
    pub fn migrate(&self) -> Result<()> {
        configure_connection(&self.conn, self.busy_timeout)?;
        migrate_to_latest(&self.conn)
    }

    pub fn schema(&self) -> Result<String> {
        let mut stmt = self.conn.prepare(
            "SELECT sql FROM sqlite_master
             WHERE type IN ('table', 'index', 'view') AND sql IS NOT NULL
             ORDER BY type, name",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut schema = Vec::new();
        for row in rows {
            schema.push(row?);
        }
        Ok(schema.join(";\n"))
    }
}
