use std::{fs, path::Path, time::Duration};

use ctx_history_core::platform_security::{restrict_private_directory, restrict_private_file};
use rusqlite::{Connection, OpenFlags, TransactionBehavior};

use super::{
    raw_sql::raw_sql_query_connection, schema, sqlite_u32, sqlite_u64, RawSqlOptions, RawSqlResult,
    RelationalProjectionError, RelationalProjectionMetadata, RelationalProjectionStatus, Result,
    SourceBackedRelationalProjection,
};

const BUSY_TIMEOUT: Duration = Duration::from_secs(30);

impl SourceBackedRelationalProjection {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
            restrict_private_directory(parent)?;
        }
        let mut conn = Connection::open(&path)?;
        restrict_private_file(&path)?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")?;
        // Opening the disposable projection is a writer operation because it
        // creates or verifies the stable compatibility views. Serialize that
        // complete schema transaction with publication so a foreground import
        // and the persistent daemon cannot interleave DROP/CREATE statements.
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        schema::initialize(&tx)?;
        tx.commit()?;
        Ok(Self {
            path,
            conn,
            read_only: false,
        })
    }

    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        configure_read_only_connection(&conn)?;
        schema::verify(&conn)?;
        Ok(Self {
            path,
            conn,
            read_only: true,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn metadata(&self) -> Result<RelationalProjectionMetadata> {
        let row = self.conn.query_row(
            "SELECT build_generation, active_generation_id, active_manifest_version,
                    active_core_record_version, active_core_record_contract_fingerprint,
                    active_lexical_schema_version, active_policy_schema_hash,
                    active_materializer_revision, target_generation_id, status,
                    source_count, session_count, event_count, repository_binding_count,
                    file_observation_count, vcs_observation_count, last_error
             FROM core_relational_state WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, i64>(11)?,
                    row.get::<_, i64>(12)?,
                    row.get::<_, i64>(13)?,
                    row.get::<_, i64>(14)?,
                    row.get::<_, i64>(15)?,
                    row.get::<_, Option<String>>(16)?,
                ))
            },
        )?;
        let status = match row.9.as_str() {
            "empty" => RelationalProjectionStatus::Empty,
            "ready" => RelationalProjectionStatus::Ready,
            "behind" => RelationalProjectionStatus::Behind,
            other => {
                return Err(RelationalProjectionError::InvalidRecord(format!(
                    "stored projection status {other} is invalid"
                )))
            }
        };
        Ok(RelationalProjectionMetadata {
            build_generation: sqlite_u64(row.0, "build_generation")?,
            active_core_generation_id: row.1,
            active_manifest_version: row
                .2
                .map(|value| sqlite_u32(value, "active_manifest_version"))
                .transpose()?,
            active_core_record_version: row
                .3
                .map(|value| sqlite_u32(value, "active_core_record_version"))
                .transpose()?,
            active_core_record_contract_fingerprint: row.4,
            active_lexical_schema_version: row
                .5
                .map(|value| sqlite_u32(value, "active_lexical_schema_version"))
                .transpose()?,
            active_policy_schema_hash: row.6,
            active_materializer_revision: row
                .7
                .map(|value| sqlite_u32(value, "active_materializer_revision"))
                .transpose()?,
            target_core_generation_id: row.8,
            status,
            source_count: sqlite_u64(row.10, "source_count")?,
            session_count: sqlite_u64(row.11, "session_count")?,
            event_count: sqlite_u64(row.12, "event_count")?,
            repository_binding_count: sqlite_u64(row.13, "repository_binding_count")?,
            file_touch_count: sqlite_u64(row.14, "file_observation_count")?,
            vcs_observation_count: sqlite_u64(row.15, "vcs_observation_count")?,
            last_error: row.16,
        })
    }

    /// Checkpoints a fully built candidate so its main file can be published.
    pub fn seal_for_replacement(&mut self) -> Result<()> {
        if self.read_only {
            return Err(RelationalProjectionError::IncompatibleState(
                "a read-only projection cannot be sealed".to_owned(),
            ));
        }
        self.conn.execute_batch(
            "PRAGMA wal_checkpoint(TRUNCATE);
             PRAGMA journal_mode = DELETE;",
        )?;
        Ok(())
    }

    pub fn raw_sql_query(&self, sql: &str, options: RawSqlOptions) -> Result<RawSqlResult> {
        raw_sql_query_connection(&self.conn, sql, options)
    }
}

fn configure_read_only_connection(conn: &Connection) -> Result<()> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA temp_store = MEMORY;
         PRAGMA cache_size = -32768;
         PRAGMA query_only = ON;",
    )?;
    Ok(())
}
