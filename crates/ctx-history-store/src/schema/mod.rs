pub(crate) mod ddl;
pub(crate) mod fts;
pub(crate) mod indexes;
pub(crate) mod migrations;
pub(crate) mod rebuild;
pub(crate) mod resumable;
#[cfg(test)]
mod tests;
pub(crate) mod views;

use crate::connection::configure_connection;
use crate::schema::resumable::{MigrationDiskNeed, MigrationStep};
use crate::{
    sqlite_amplifying_write_estimate, Result, Store, StoreError, INDEXING_WAL_DELTA_BYTES,
    SCHEMA_VERSION,
};

impl Store {
    pub fn migrate(&self) -> Result<()> {
        configure_connection(&self.conn, self.busy_timeout)?;
        self.run_migrations_with_handoff()
    }

    pub(crate) fn run_migrations_with_handoff(&self) -> Result<()> {
        loop {
            self.acquire_indexing_writer_lease(false)?;
            let slice = self.begin_indexing_slice()?;
            let result = self.migrate_one_step_unleased();
            if result.is_err() && !self.conn.is_autocommit() {
                let _ = self.rollback_batch();
            }
            if !self.conn.is_autocommit() {
                self.connection_quarantined.set(true);
                return result.and(Err(StoreError::ConnectionQuarantined));
            }
            self.release_indexing_writer_lease();
            let pacing = self.finish_indexing_slice(slice);
            let complete = result?;
            pacing?;
            if complete {
                return Ok(());
            }
        }
    }

    pub(crate) fn migrate_one_step_unleased(&self) -> Result<bool> {
        let user_version: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if user_version > SCHEMA_VERSION {
            return Err(StoreError::UnsupportedSchemaVersion(user_version));
        }
        if user_version == SCHEMA_VERSION {
            return Ok(true);
        }
        if user_version < 16 {
            let estimated =
                sqlite_amplifying_write_estimate(&self.path, 2, INDEXING_WAL_DELTA_BYTES)?;
            self.ensure_disk_headroom(estimated, "ctx legacy migration")?;
            migrations::run_next_legacy_migration(&self.conn, user_version, || {
                self.ensure_disk_headroom(estimated, "ctx legacy migration")
            })?;
            return Ok(false);
        }
        let target = resumable::target_for_version(user_version)
            .ok_or(StoreError::UnsupportedSchemaVersion(user_version))?;
        let step = resumable::run_step(&self.conn, target, |need, operation| {
            let estimated = match need {
                MigrationDiskNeed::None => 0,
                MigrationDiskNeed::Fixed(bytes) => bytes,
                MigrationDiskNeed::DatabaseAmplification(multiplier) => {
                    sqlite_amplifying_write_estimate(
                        &self.path,
                        multiplier,
                        INDEXING_WAL_DELTA_BYTES,
                    )?
                }
            };
            self.ensure_disk_headroom(estimated, operation)
        })?;
        Ok(step == MigrationStep::Complete && target == SCHEMA_VERSION)
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
