pub(crate) mod ddl;
pub(crate) mod fts;
pub(crate) mod indexes;
pub(crate) mod migrations;
#[cfg(test)]
mod tests;
pub(crate) mod views;

use rusqlite::Connection;

use crate::connection::configure_connection;
use crate::{Result, Store, StoreError, SCHEMA_VERSION};

pub(crate) use fts::create_fts_tables_if_supported;

pub(crate) fn migrate_to_latest(conn: &Connection) -> Result<()> {
    let user_version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if user_version > SCHEMA_VERSION {
        return Err(StoreError::UnsupportedSchemaVersion(user_version));
    }
    migrations::run_migrations(conn, user_version)?;
    create_fts_tables_if_supported(conn)?;
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
