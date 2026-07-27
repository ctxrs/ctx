use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

use chrono::{DateTime, Utc};
use rusqlite::{functions::FunctionFlags, Connection, OpenFlags};
use uuid::Uuid;

use crate::object_store::{
    migrate_legacy_history_layout, restrict_private_dir, restrict_private_file, OBJECTS_DIR,
    SPOOL_DIR,
};
use crate::{Result, Store, StoreError, SCHEMA_VERSION};

pub(crate) const BUSY_TIMEOUT: Duration = Duration::from_millis(30_000);

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_busy_timeout(path, BUSY_TIMEOUT)
    }

    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        verify_private_store_paths(&path)?;
        let object_dir = path
            .parent()
            .map(|parent| parent.join(OBJECTS_DIR))
            .unwrap_or_else(|| PathBuf::from(OBJECTS_DIR));
        // A live Store uses WAL. Every reader must participate in SQLite's
        // normal read protocol; immutable=1 can silently omit committed WAL
        // frames when a writer appears after a sidecar-presence check.
        let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        configure_read_only_connection(&conn, BUSY_TIMEOUT)?;
        let user_version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if user_version != SCHEMA_VERSION {
            return Err(StoreError::UnsupportedSchemaVersion(user_version));
        }
        crate::schema::verify_final_schema_identity(&conn)?;
        let store = Self {
            path,
            object_dir,
            conn,
            busy_timeout: BUSY_TIMEOUT,
            event_search_bulk_depth: Default::default(),
            event_search_bulk_epoch: Default::default(),
            batch_depth: Default::default(),
            import_batch_depth: Default::default(),
            event_search_projection_capabilities: Default::default(),
            projection_journal_active_in_batch: Default::default(),
            projection_journal_group_collector: Default::default(),
            native_path_group_token: Default::default(),
            native_path_mutation_scope: Default::default(),
            native_path_group_poisoned: Default::default(),
            native_path_transaction_control_scope: Default::default(),
            event_search_bulk_group_admission_outstanding: Default::default(),
        };
        store.initialize_event_search_projection_capabilities()?;
        Ok(store)
    }

    pub fn open_with_busy_timeout(path: impl AsRef<Path>, busy_timeout: Duration) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let mut migrated_legacy_layout = false;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
            restrict_private_dir(parent)?;
            migrated_legacy_layout = migrate_legacy_history_layout(parent)?;
            restrict_private_dir(parent)?;
        }
        let object_dir = path
            .parent()
            .map(|parent| parent.join(OBJECTS_DIR))
            .unwrap_or_else(|| PathBuf::from(OBJECTS_DIR));
        fs::create_dir_all(&object_dir)?;
        restrict_private_dir(&object_dir)?;
        if let Some(spool_dir) = path.parent().map(|parent| parent.join(SPOOL_DIR)) {
            fs::create_dir_all(&spool_dir)?;
            restrict_private_dir(&spool_dir)?;
        }
        let conn = Connection::open(&path)?;
        restrict_private_file(&path)?;
        let store = Self {
            path,
            object_dir,
            conn,
            busy_timeout,
            event_search_bulk_depth: Default::default(),
            event_search_bulk_epoch: Default::default(),
            batch_depth: Default::default(),
            import_batch_depth: Default::default(),
            event_search_projection_capabilities: Default::default(),
            projection_journal_active_in_batch: Default::default(),
            projection_journal_group_collector: Default::default(),
            native_path_group_token: Default::default(),
            native_path_mutation_scope: Default::default(),
            native_path_group_poisoned: Default::default(),
            native_path_transaction_control_scope: Default::default(),
            event_search_bulk_group_admission_outstanding: Default::default(),
        };
        store.migrate()?;
        restrict_existing_sqlite_auxiliary_files(&store.path)?;
        store.recover_event_search_bulk_mode()?;
        if migrated_legacy_layout {
            store.normalize_legacy_blob_paths()?;
        }
        store.ensure_search_projection_initialized()?;
        store.initialize_event_search_projection_capabilities()?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns whether no provider-owned Core projection has begun.
    ///
    /// Catalog rows and their owning history record are intentionally allowed:
    /// the native import lifecycle creates those control rows before projecting
    /// provider content. The predicate fails closed on every provider
    /// projection, locator, route, and cursor table used by the fresh Codex
    /// bootstrap path.
    #[doc(hidden)]
    pub fn fresh_provider_projection_eligible(&self) -> Result<bool> {
        self.conn
            .query_row(
                r#"
                SELECT
                    NOT EXISTS (SELECT 1 FROM capture_sources)
                    AND NOT EXISTS (SELECT 1 FROM provider_source_locators)
                    AND NOT EXISTS (SELECT 1 FROM capture_source_provider_routes)
                    AND NOT EXISTS (SELECT 1 FROM sessions)
                    AND NOT EXISTS (SELECT 1 FROM session_edges)
                    AND NOT EXISTS (SELECT 1 FROM runs)
                    AND NOT EXISTS (SELECT 1 FROM events)
                    AND NOT EXISTS (SELECT 1 FROM event_search_lookup)
                    AND NOT EXISTS (SELECT 1 FROM files_touched)
                    AND NOT EXISTS (SELECT 1 FROM sync_cursors)
                "#,
                [],
                |row| row.get(0),
            )
            .map_err(StoreError::from)
    }

    pub fn begin_immediate_batch(&self) -> Result<()> {
        self.reject_unowned_native_path_transaction_control()?;
        let depth = self.batch_depth.get();
        if depth == 0 {
            self.conn.execute_batch("BEGIN IMMEDIATE")?;
            self.projection_journal_active_in_batch.set(None);
        } else {
            self.conn
                .execute_batch(&format!("SAVEPOINT ctx_store_batch_{}", depth + 1))?;
        }
        self.batch_depth.set(depth + 1);
        Ok(())
    }

    pub fn commit_batch(&self) -> Result<()> {
        self.reject_unowned_native_path_transaction_control()?;
        let depth = self.batch_depth.get();
        if depth == 0 {
            return Err(StoreError::Sql(rusqlite::Error::InvalidQuery));
        }
        if depth == 1 {
            self.conn.execute_batch("COMMIT")?;
        } else {
            self.conn
                .execute_batch(&format!("RELEASE SAVEPOINT ctx_store_batch_{depth}"))?;
        }
        self.batch_depth.set(depth - 1);
        if depth == 1 {
            self.projection_journal_active_in_batch.set(None);
        }
        Ok(())
    }

    pub fn rollback_batch(&self) -> Result<()> {
        self.reject_unowned_native_path_transaction_control()?;
        let depth = self.batch_depth.get();
        if depth == 0 {
            return Err(StoreError::Sql(rusqlite::Error::InvalidQuery));
        }
        if depth == 1 {
            self.conn.execute_batch("ROLLBACK")?;
        } else {
            self.conn.execute_batch(&format!(
                "ROLLBACK TO SAVEPOINT ctx_store_batch_{depth};\n\
                 RELEASE SAVEPOINT ctx_store_batch_{depth};"
            ))?;
        }
        self.batch_depth.set(depth - 1);
        // A savepoint may have activated or disabled the journal. Its cached
        // activity must be re-read after any rollback restores the prior state.
        self.projection_journal_active_in_batch.set(None);
        Ok(())
    }

    fn reject_unowned_native_path_transaction_control(&self) -> Result<()> {
        if self.native_path_group_token.get().is_some()
            && !self.native_path_transaction_control_scope.get()
        {
            self.poison_native_path_group();
            return Err(StoreError::NativePathTransactionControlDenied);
        }
        Ok(())
    }

    /// Begins an importer-owned atomic batch. Event and run mutations can rely on this owner to
    /// roll back the complete batch instead of adding a savepoint around every row.
    #[doc(hidden)]
    pub fn begin_import_batch(&self) -> Result<()> {
        self.begin_immediate_batch()?;
        self.import_batch_depth
            .set(self.import_batch_depth.get().saturating_add(1));
        Ok(())
    }

    #[doc(hidden)]
    pub fn commit_import_batch(&self) -> Result<()> {
        let depth = self.import_batch_depth.get();
        if depth == 0 {
            return Err(StoreError::Sql(rusqlite::Error::InvalidQuery));
        }
        self.commit_batch()?;
        self.import_batch_depth.set(depth - 1);
        Ok(())
    }

    #[doc(hidden)]
    pub fn rollback_import_batch(&self) -> Result<()> {
        let depth = self.import_batch_depth.get();
        if depth == 0 {
            return Err(StoreError::Sql(rusqlite::Error::InvalidQuery));
        }
        self.rollback_batch()?;
        self.import_batch_depth.set(depth - 1);
        Ok(())
    }

    pub(crate) fn with_import_batch_write<T>(
        &self,
        operation: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        if self.import_batch_depth.get() > 0 {
            debug_assert!(self.batch_depth.get() > 0);
            operation()
        } else {
            self.with_atomic_write(operation)
        }
    }

    pub(crate) fn with_atomic_write<T>(&self, operation: impl FnOnce() -> Result<T>) -> Result<T> {
        let owns_transaction = self.conn.is_autocommit();
        if owns_transaction {
            self.conn.execute_batch("BEGIN IMMEDIATE")?;
        } else {
            self.conn
                .execute_batch("SAVEPOINT ctx_atomic_canonical_mutation")?;
        }
        let result = operation();
        if !owns_transaction {
            return match result {
                Ok(value) => {
                    self.conn
                        .execute_batch("RELEASE SAVEPOINT ctx_atomic_canonical_mutation")?;
                    Ok(value)
                }
                Err(error) => {
                    self.conn.execute_batch(
                        "ROLLBACK TO SAVEPOINT ctx_atomic_canonical_mutation;
                         RELEASE SAVEPOINT ctx_atomic_canonical_mutation;",
                    )?;
                    Err(error)
                }
            };
        }
        match result {
            Ok(value) => {
                if let Err(error) = self.conn.execute_batch("COMMIT") {
                    let _ = self.conn.execute_batch("ROLLBACK");
                    return Err(StoreError::Sql(error));
                }
                Ok(value)
            }
            Err(error) => {
                if let Err(rollback_error) = self.conn.execute_batch("ROLLBACK") {
                    return Err(StoreError::Sql(rollback_error));
                }
                Err(error)
            }
        }
    }

    pub fn checkpoint_wal_passive(&self) -> Result<()> {
        let _ = self.checkpoint_wal("PASSIVE")?;
        Ok(())
    }

    pub fn checkpoint_wal_truncate(&self) -> Result<()> {
        let _ = self.checkpoint_wal("TRUNCATE")?;
        Ok(())
    }

    pub fn checkpoint_wal_truncate_required(&self) -> Result<()> {
        let outcome = self.checkpoint_wal("TRUNCATE")?;
        if outcome.busy {
            return Err(StoreError::WalCheckpointBusy {
                log_frames: outcome.log_frames,
                checkpointed_frames: outcome.checkpointed_frames,
            });
        }
        Ok(())
    }

    pub fn checkpoint_wal_passive_if_larger_than(&self, min_bytes: u64) -> Result<bool> {
        let Some(wal_bytes) = self.wal_bytes()? else {
            return Ok(false);
        };
        if wal_bytes < min_bytes {
            return Ok(false);
        }
        self.checkpoint_wal_passive()?;
        Ok(true)
    }

    pub fn checkpoint_wal_truncate_if_larger_than(&self, min_bytes: u64) -> Result<bool> {
        let Some(wal_bytes) = self.wal_bytes()? else {
            return Ok(false);
        };
        if wal_bytes < min_bytes {
            return Ok(false);
        }
        self.checkpoint_wal_truncate()?;
        Ok(true)
    }

    fn wal_path(&self) -> PathBuf {
        let mut path = self.path.as_os_str().to_os_string();
        path.push("-wal");
        PathBuf::from(path)
    }

    fn wal_bytes(&self) -> Result<Option<u64>> {
        match fs::metadata(self.wal_path()) {
            Ok(metadata) => Ok(Some(metadata.len())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(StoreError::Io(err)),
        }
    }

    fn checkpoint_wal(&self, mode: &'static str) -> Result<WalCheckpointOutcome> {
        let sql = match mode {
            "PASSIVE" => "PRAGMA wal_checkpoint(PASSIVE)",
            "TRUNCATE" => "PRAGMA wal_checkpoint(TRUNCATE)",
            _ => unreachable!("unsupported WAL checkpoint mode"),
        };
        self.conn
            .query_row(sql, [], |row| {
                Ok(WalCheckpointOutcome {
                    busy: row.get::<_, i64>(0)? != 0,
                    log_frames: row.get(1)?,
                    checkpointed_frames: row.get(2)?,
                })
            })
            .map_err(StoreError::from)
    }

    pub fn validate(&self) -> Result<Vec<String>> {
        let integrity: String = self
            .conn
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        let foreign_key_failures = count_foreign_key_failures(&self.conn)?;

        let mut findings = Vec::new();
        if integrity != "ok" {
            findings.push(format!("sqlite integrity_check returned {integrity}"));
        }
        if foreign_key_failures > 0 {
            findings.push(format!(
                "{foreign_key_failures} foreign key violations detected"
            ));
        }
        Ok(findings)
    }
}

fn restrict_existing_sqlite_auxiliary_files(path: &Path) -> Result<()> {
    for suffix in ["-journal", "-wal", "-shm"] {
        let mut value = OsString::from(path.as_os_str());
        value.push(suffix);
        let auxiliary = PathBuf::from(value);
        match fs::symlink_metadata(&auxiliary) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                restrict_private_file(&auxiliary)?;
            }
            Ok(_) => {
                return Err(StoreError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "SQLite auxiliary path has an unsafe file type",
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn verify_private_store_paths(path: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        use ctx_history_core::platform_security::{verify_private_directory, verify_private_file};

        if let Some(parent) = path.parent() {
            verify_private_directory(parent)?;
        }
        verify_private_file(path)?;
    }
    #[cfg(not(windows))]
    let _ = path;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WalCheckpointOutcome {
    busy: bool,
    log_frames: i64,
    checkpointed_frames: i64,
}

pub(crate) fn configure_connection(conn: &Connection, busy_timeout: Duration) -> Result<()> {
    conn.busy_timeout(busy_timeout)?;
    conn.create_scalar_function(
        "ctx_projection_writer_authorized_v1",
        0,
        FunctionFlags::SQLITE_UTF8
            | FunctionFlags::SQLITE_DETERMINISTIC
            | FunctionFlags::SQLITE_INNOCUOUS,
        |_| Ok(1_i64),
    )?;
    conn.execute_batch(
        r#"
        PRAGMA foreign_keys = ON;
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = NORMAL;
        PRAGMA temp_store = MEMORY;
        PRAGMA cache_size = -32768;
        -- Keep automatic checkpoints below the Store's 64 MiB hard WAL bound,
        -- while avoiding repeated full-database writeback during large imports.
        PRAGMA wal_autocheckpoint = 14000;
        "#,
    )?;
    Ok(())
}

pub(crate) fn configure_read_only_connection(
    conn: &Connection,
    busy_timeout: Duration,
) -> Result<()> {
    conn.busy_timeout(busy_timeout)?;
    conn.execute_batch(
        r#"
        PRAGMA foreign_keys = ON;
        PRAGMA temp_store = MEMORY;
        PRAGMA cache_size = -32768;
        PRAGMA query_only = ON;
        "#,
    )?;
    Ok(())
}

pub(crate) fn count_foreign_key_failures(conn: &Connection) -> Result<i64> {
    let mut stmt = conn.prepare("PRAGMA foreign_key_check")?;
    let mut rows = stmt.query([])?;
    let mut count = 0;
    while rows.next()?.is_some() {
        count += 1;
    }
    Ok(count)
}

pub(crate) fn timestamp_ms(value: DateTime<Utc>) -> i64 {
    value.timestamp_millis()
}

pub(crate) fn capped_i64(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}

pub(crate) fn nonnegative_i64_to_u64(value: i64) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))
}

pub(crate) fn nonnegative_i64_to_u32(value: i64) -> rusqlite::Result<u32> {
    u32::try_from(value).map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))
}

pub(crate) fn time_ms(value: i64) -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp_millis(value).unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
}

pub(crate) fn optional_uuid_string(id: Option<Uuid>) -> Option<String> {
    id.map(|id| id.to_string())
}

pub(crate) fn optional_timestamp_ms(value: Option<DateTime<Utc>>) -> Option<i64> {
    value.map(timestamp_ms)
}

pub(crate) fn ms_to_time(value: i64) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::<Utc>::from_timestamp_millis(value).ok_or_else(|| {
        rusqlite::Error::ToSqlConversionFailure(format!("invalid timestamp millis: {value}").into())
    })
}

pub(crate) fn optional_ms_to_time(value: Option<i64>) -> rusqlite::Result<Option<DateTime<Utc>>> {
    value.map(ms_to_time).transpose()
}

pub(crate) fn parse_optional_uuid(value: Option<String>) -> rusqlite::Result<Option<Uuid>> {
    value.map(parse_uuid).transpose()
}

pub(crate) fn parse_json(value: String) -> rusqlite::Result<serde_json::Value> {
    serde_json::from_str(&value)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))
}

pub(crate) fn parse_uuid(value: String) -> rusqlite::Result<Uuid> {
    Uuid::parse_str(&value).map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))
}

pub(crate) fn parse_time(value: String) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))
}

pub(crate) fn parse_text_enum<T>(value: String) -> rusqlite::Result<T>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    value
        .parse()
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))
}

pub(crate) fn parse_optional_text_enum<T>(value: Option<String>) -> rusqlite::Result<Option<T>>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    value.map(parse_text_enum).transpose()
}

pub(crate) fn collect_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
) -> Result<Vec<T>> {
    let mut values = Vec::new();
    for row in rows {
        values.push(row?);
    }
    Ok(values)
}
