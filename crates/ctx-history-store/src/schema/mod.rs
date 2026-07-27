pub(crate) mod ddl;
pub(crate) mod fts;
pub(crate) mod indexes;
pub(crate) mod migrations;
pub(crate) mod provider_checks;
pub(crate) mod provider_session_identity;
pub(crate) mod rebuild;
pub(crate) mod scriptgram;
pub(crate) mod semantic_projection_epoch;
#[cfg(test)]
mod tests;
pub(crate) mod views;

use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use rusqlite::Connection;

use crate::connection::configure_connection;
use crate::object_store::restrict_private_file;
use crate::schema::indexes::INDEXES_SQL;
use crate::{Result, Store, StoreError, FINAL_SCHEMA_IDENTITY, SCHEMA_VERSION};

pub(crate) use fts::create_fts_tables_if_supported;

const MIGRATION_LOCK_SUFFIX: &str = ".migration.lock.sqlite";

/// Cross-process schema ownership. SQLite releases the sidecar write lock if
/// the owner exits, so an interrupted migration cannot leave a stale lease.
struct MigrationGuard {
    conn: Connection,
}

impl Drop for MigrationGuard {
    fn drop(&mut self) {
        let _ = self.conn.execute_batch("ROLLBACK");
    }
}

fn migration_lock_path(store_path: &std::path::Path) -> Result<PathBuf> {
    // SQLite itself resolves symlinks for the main file. Resolve them for the
    // sidecar too so equivalent Store paths cannot acquire different owners.
    let canonical_store_path = std::fs::canonicalize(store_path)?;
    let mut value = OsString::from(canonical_store_path.as_os_str());
    value.push(MIGRATION_LOCK_SUFFIX);
    Ok(PathBuf::from(value))
}

fn acquire_migration_lock(store: &Store) -> Result<MigrationGuard> {
    let path = migration_lock_path(&store.path)?;
    let conn = Connection::open(&path)?;
    restrict_private_file(&path)?;
    conn.busy_timeout(store.busy_timeout)?;
    conn.execute_batch(
        "PRAGMA journal_mode=DELETE;
         CREATE TABLE IF NOT EXISTS migration_lock (id INTEGER PRIMARY KEY);
         BEGIN IMMEDIATE",
    )?;
    Ok(MigrationGuard { conn })
}

pub(crate) fn migrate_to_latest(conn: &Connection, object_dir: &Path) -> Result<()> {
    let user_version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if user_version > SCHEMA_VERSION {
        return Err(StoreError::UnsupportedSchemaVersion(user_version));
    }
    migrations::run_migrations(conn, object_dir, user_version)?;
    verify_final_schema_identity(conn)?;
    conn.execute_batch(provider_session_identity::PROVIDER_SESSION_INVARIANTS_SQL)?;
    create_fts_tables_if_supported(conn)?;
    conn.execute_batch(INDEXES_SQL)?;
    crate::projection_journal::ensure_projection_writer_fence(conn)?;
    Ok(())
}

pub(crate) fn verify_final_schema_identity(conn: &Connection) -> Result<()> {
    let identity = conn
        .query_row(
            "SELECT schema_identity FROM ctx_store_schema_identity
             WHERE singleton = 1 AND schema_version = ?1",
            [SCHEMA_VERSION],
            |row| row.get::<_, String>(0),
        )
        .map_err(|error| match error {
            rusqlite::Error::SqliteFailure(_, Some(message)) => {
                StoreError::UnsupportedSchemaIdentity(message)
            }
            rusqlite::Error::QueryReturnedNoRows => {
                StoreError::UnsupportedSchemaIdentity("missing".to_owned())
            }
            other => StoreError::Sql(other),
        })?;
    if identity != FINAL_SCHEMA_IDENTITY {
        return Err(StoreError::UnsupportedSchemaIdentity(identity));
    }
    Ok(())
}

impl Store {
    pub fn migrate(&self) -> Result<()> {
        // Own migration before configuring the main connection or observing
        // user_version. Individual migrations retain their existing atomic
        // transactions while this sidecar serializes the complete dispatch.
        let _guard = acquire_migration_lock(self)?;
        #[cfg(test)]
        migration_test_barrier();
        configure_connection(&self.conn, self.busy_timeout)?;
        migrate_to_latest(&self.conn, &self.object_dir)
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

#[cfg(test)]
fn migration_test_barrier() {
    use std::{
        fs, thread,
        time::{Duration, Instant},
    };

    let Some(ready) = std::env::var_os("CTX_TEST_MIGRATION_READY") else {
        return;
    };
    let release = std::env::var_os("CTX_TEST_MIGRATION_RELEASE")
        .expect("migration test barrier requires a release path");
    fs::write(ready, b"ready").expect("write migration ready marker");
    let deadline = Instant::now() + Duration::from_secs(15);
    while !std::path::Path::new(&release).exists() {
        assert!(
            Instant::now() < deadline,
            "migration test barrier timed out"
        );
        thread::sleep(Duration::from_millis(5));
    }
}
