use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OpenFlags};
use uuid::Uuid;

use crate::object_store::{
    migrate_legacy_history_layout, restrict_private_dir, restrict_private_file, OBJECTS_DIR,
    SPOOL_DIR,
};
use crate::work_control::{canonical_existing_store_path, prepare_store_path};
use crate::{
    IndexingAdmission, IndexingWorkClass, Result, Store, StoreError, WalCheckpointStatus,
    SCHEMA_VERSION, WAL_PASSIVE_MIN_BYTES, WAL_RESTART_MIN_BYTES, WAL_TRUNCATE_MIN_BYTES,
};

pub(crate) const BUSY_TIMEOUT: Duration = Duration::from_millis(30_000);

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_busy_timeout(path, BUSY_TIMEOUT)
    }

    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self> {
        let path = canonical_existing_store_path(path.as_ref())?;
        Self::open_read_only_connection(path)
    }

    fn open_read_only_connection(path: PathBuf) -> Result<Self> {
        let object_dir = path
            .parent()
            .map(|parent| parent.join(OBJECTS_DIR))
            .unwrap_or_else(|| PathBuf::from(OBJECTS_DIR));
        // A normal read-only WAL connection may still create -wal/-shm files.
        // A clean checkpointed store can instead be opened as an immutable
        // snapshot. If a writer races this check, immutable mode still sees a
        // coherent (possibly older) main database rather than an uncommitted
        // WAL generation.
        let conn = if sqlite_sidecar_exists(&path, "-wal") || sqlite_sidecar_exists(&path, "-shm") {
            Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)?
        } else {
            Connection::open_with_flags(
                sqlite_read_only_immutable_uri(&path),
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
            )?
        };
        configure_read_only_connection(&conn, BUSY_TIMEOUT)?;
        let user_version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if user_version != SCHEMA_VERSION {
            return Err(StoreError::UnsupportedSchemaVersion(user_version));
        }
        Ok(Self {
            path,
            object_dir,
            conn,
            busy_timeout: BUSY_TIMEOUT,
            event_search_bulk_depth: Default::default(),
            event_search_transaction_lock: Default::default(),
            indexing_admission: None,
            indexing_writer_lease: Default::default(),
            connection_quarantined: Default::default(),
        })
    }

    pub fn open_with_busy_timeout(path: impl AsRef<Path>, busy_timeout: Duration) -> Result<Self> {
        let path = path.as_ref();
        let admission = IndexingAdmission::acquire(path, IndexingWorkClass::Foreground)?;
        let path = admission.ensure_store_path(path)?;
        Self::open_inner(&path, busy_timeout, admission, false)
    }

    pub fn open_admitted(path: impl AsRef<Path>, admission: &IndexingAdmission) -> Result<Self> {
        Self::open_admitted_with_busy_timeout(path, BUSY_TIMEOUT, admission)
    }

    pub fn open_admitted_with_busy_timeout(
        path: impl AsRef<Path>,
        busy_timeout: Duration,
        admission: &IndexingAdmission,
    ) -> Result<Self> {
        let path = admission.ensure_store_path(path.as_ref())?;
        Self::open_inner(&path, busy_timeout, admission.clone(), true)
    }

    fn open_inner(
        path: &Path,
        busy_timeout: Duration,
        indexing_admission: IndexingAdmission,
        run_maintenance_on_open: bool,
    ) -> Result<Self> {
        let open_lease = indexing_admission.lease()?;
        let path = prepare_store_path(path)?;
        let mut migrated_legacy_layout = false;
        if let Some(parent) = path.parent() {
            migrated_legacy_layout = migrate_legacy_history_layout(parent)?;
            fs::create_dir_all(parent)?;
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
            event_search_transaction_lock: Default::default(),
            indexing_admission: Some(indexing_admission),
            indexing_writer_lease: std::cell::RefCell::new(Some(open_lease)),
            connection_quarantined: Default::default(),
        };
        configure_connection(&store.conn, busy_timeout)?;
        store.run_migrations_with_handoff()?;
        if migrated_legacy_layout {
            let slice = store.begin_indexing_slice()?;
            store.acquire_indexing_writer_lease(false)?;
            let result = store.normalize_legacy_blob_paths();
            if store.conn.is_autocommit() {
                store.release_indexing_writer_lease();
            } else {
                store.connection_quarantined.set(true);
            }
            result?;
            store.ensure_connection_usable()?;
            store.finish_indexing_slice(slice)?;
        }
        store.ensure_search_projection_initialized()?;
        store.consume_search_projection_repair_request()?;
        if run_maintenance_on_open && store.event_search_maintenance_pending()? {
            store.run_event_search_maintenance_slice()?;
        }
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn begin_immediate_batch(&self) -> Result<()> {
        let event_search_lock = self.event_search_bulk_mode_active();
        if !self.begin_immediate_batch_inner(false, event_search_lock, event_search_lock)? {
            unreachable!("blocking transaction admission returned without a lease");
        }
        Ok(())
    }

    pub fn commit_batch(&self) -> Result<()> {
        if let Err(error) =
            crate::search::projections::certify_active_search_projection_revision(&self.conn)
        {
            let _ = self.rollback_batch();
            return Err(error);
        }
        match self.conn.execute_batch("COMMIT") {
            Ok(()) => {
                let lock_result = self.release_event_search_transaction_lock(true);
                self.release_indexing_writer_lease();
                lock_result
            }
            Err(error) => {
                // SQLite can leave a transaction active after a deferred
                // constraint fails at COMMIT. Release admission only after the
                // main transaction is definitely gone; a failed rollback must
                // leave this Store exclusive until cleanup can be retried.
                let _ = self.rollback_batch();
                Err(error.into())
            }
        }
    }

    pub fn rollback_batch(&self) -> Result<()> {
        self.rollback_batch_with(|| self.conn.execute_batch("ROLLBACK"))
    }

    pub(crate) fn rollback_batch_with(
        &self,
        rollback: impl FnOnce() -> rusqlite::Result<()>,
    ) -> Result<()> {
        let rollback_result = rollback().map_err(StoreError::from);
        let lock_result = if self.conn.is_autocommit() {
            let result = self.release_event_search_transaction_lock(false);
            self.release_indexing_writer_lease();
            result
        } else {
            self.connection_quarantined.set(true);
            Ok(())
        };
        rollback_result.and(lock_result)
    }

    pub(crate) fn with_write_transaction<T>(
        &self,
        operation: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        if !self.conn.is_autocommit() {
            return operation();
        }
        self.begin_immediate_batch()?;
        let value = match operation() {
            Ok(value) => value,
            Err(error) => {
                let _ = self.rollback_batch();
                return Err(error);
            }
        };
        self.commit_batch()?;
        Ok(value)
    }

    pub(crate) fn with_read_snapshot<T>(&self, operation: impl FnOnce() -> Result<T>) -> Result<T> {
        self.ensure_connection_usable()?;
        if !self.conn.is_autocommit() {
            return operation();
        }
        self.conn.execute_batch("BEGIN DEFERRED")?;
        match operation() {
            Ok(value) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(value)
            }
            Err(error) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    pub(crate) fn begin_event_search_batch(&self, nonblocking: bool) -> Result<bool> {
        self.begin_immediate_batch_inner(nonblocking, true, false)
    }

    fn begin_immediate_batch_inner(
        &self,
        nonblocking: bool,
        event_search_lock: bool,
        prepare_bulk_mode: bool,
    ) -> Result<bool> {
        self.ensure_connection_usable()?;
        let writer_lease_preexisting = self.indexing_writer_lease_held();
        if !self.acquire_indexing_writer_lease(nonblocking)? {
            return Ok(false);
        }
        let event_search_lock_preexisting = self.event_search_transaction_lock_held();
        if event_search_lock && !self.acquire_event_search_transaction_lock(nonblocking)? {
            if !writer_lease_preexisting {
                self.release_indexing_writer_lease();
            }
            return Ok(false);
        }
        if let Err(error) = self.conn.execute_batch("BEGIN IMMEDIATE") {
            if !event_search_lock_preexisting {
                let _ = self.release_event_search_transaction_lock(false);
            }
            if !writer_lease_preexisting {
                self.release_indexing_writer_lease();
            }
            return Err(error.into());
        }
        if prepare_bulk_mode {
            if let Err(error) = self.prepare_active_event_search_bulk_transaction() {
                let _ = self.rollback_batch();
                return Err(error);
            }
        }
        Ok(true)
    }

    fn ensure_connection_usable(&self) -> Result<()> {
        if self.connection_quarantined.get() {
            Err(StoreError::ConnectionQuarantined)
        } else {
            Ok(())
        }
    }

    pub fn checkpoint_wal_passive(&self) -> Result<()> {
        let slice = self.begin_indexing_slice()?;
        let (_, written_frames) =
            self.with_indexing_writer_lease(|| self.checkpoint_wal_passive_measured())?;
        self.finish_indexing_checkpoint(slice, written_frames);
        Ok(())
    }

    pub fn checkpoint_wal_truncate(&self) -> Result<()> {
        let slice = self.begin_indexing_slice()?;
        let written_frames = self.with_indexing_writer_lease(|| {
            let (_, written_frames) = self.checkpoint_wal_passive_measured()?;
            let (_, reset_frames) = self.checkpoint_wal_measured("TRUNCATE")?;
            Ok(written_frames.saturating_add(reset_frames))
        })?;
        self.finish_indexing_checkpoint(slice, written_frames);
        Ok(())
    }

    pub fn checkpoint_wal_truncate_required(&self) -> Result<()> {
        let slice = self.begin_indexing_slice()?;
        let (outcome, written_frames) = self.with_indexing_writer_lease(|| {
            let (_, written_frames) = self.checkpoint_wal_passive_measured()?;
            let (outcome, reset_frames) = self.checkpoint_wal_measured("TRUNCATE")?;
            Ok((outcome, written_frames.saturating_add(reset_frames)))
        })?;
        self.finish_indexing_checkpoint(slice, written_frames);
        if outcome.busy {
            return Err(StoreError::WalCheckpointBusy {
                log_frames: outcome.log_frames,
                checkpointed_frames: outcome.checkpointed_frames,
            });
        }
        Ok(())
    }

    pub fn checkpoint_wal_for_pressure(&self) -> Result<WalCheckpointStatus> {
        let slice = self.begin_indexing_slice()?;
        let (status, checkpointed_frames) = self.checkpoint_wal_for_pressure_work()?;
        self.finish_indexing_checkpoint(slice, checkpointed_frames);
        Ok(status)
    }

    pub(crate) fn checkpoint_wal_for_pressure_work(&self) -> Result<(WalCheckpointStatus, u64)> {
        self.with_indexing_writer_lease(|| self.checkpoint_wal_for_pressure_unleased())
    }

    pub(crate) fn try_checkpoint_wal_for_pressure(
        &self,
    ) -> Result<Option<(WalCheckpointStatus, u64)>> {
        self.try_with_indexing_writer_lease(|| self.checkpoint_wal_for_pressure_unleased())
    }

    fn checkpoint_wal_for_pressure_unleased(&self) -> Result<(WalCheckpointStatus, u64)> {
        self.checkpoint_wal_for_pressure_work_after_passive(|| {})
    }

    #[cfg(test)]
    pub(crate) fn checkpoint_wal_for_pressure_after_passive(
        &self,
        after_passive: impl FnOnce(),
    ) -> Result<WalCheckpointStatus> {
        self.checkpoint_wal_for_pressure_work_after_passive(after_passive)
            .map(|(status, _)| status)
    }

    fn checkpoint_wal_for_pressure_work_after_passive(
        &self,
        after_passive: impl FnOnce(),
    ) -> Result<(WalCheckpointStatus, u64)> {
        let before_bytes = self.wal_bytes()?.unwrap_or(0);
        if before_bytes < WAL_PASSIVE_MIN_BYTES {
            return Ok((
                WalCheckpointStatus {
                    wal_bytes: before_bytes,
                    ..WalCheckpointStatus::default()
                },
                0,
            ));
        }

        let (passive, mut written_frames) = self.checkpoint_wal_passive_measured()?;
        after_passive();
        let after_bytes = self.wal_bytes()?.unwrap_or(0);
        let mut status = WalCheckpointStatus {
            attempted: true,
            busy: passive.busy,
            log_frames: passive.log_frames,
            checkpointed_frames: passive.checkpointed_frames,
            wal_bytes: after_bytes,
        };
        if after_bytes < WAL_RESTART_MIN_BYTES || status.pinned() {
            return Ok((status, written_frames));
        }

        let mode = if after_bytes >= WAL_TRUNCATE_MIN_BYTES {
            "TRUNCATE"
        } else {
            "RESTART"
        };
        let (_, additional_frames) = self.checkpoint_wal_passive_measured()?;
        written_frames = written_frames.saturating_add(additional_frames);
        let (outcome, reset_frames) = self.checkpoint_wal_measured(mode)?;
        written_frames = written_frames.saturating_add(reset_frames);
        status.busy = outcome.busy;
        status.log_frames = outcome.log_frames;
        status.checkpointed_frames = outcome.checkpointed_frames;
        status.wal_bytes = self.wal_bytes()?.unwrap_or(0);
        Ok((status, written_frames))
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

    pub fn wal_size_bytes(&self) -> Result<u64> {
        Ok(self.wal_bytes()?.unwrap_or(0))
    }

    pub(crate) fn wal_bytes(&self) -> Result<Option<u64>> {
        match fs::metadata(self.wal_path()) {
            Ok(metadata) => Ok(Some(metadata.len())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(StoreError::Io(err)),
        }
    }

    fn wal_backfill_snapshot(&self) -> Option<WalBackfillSnapshot> {
        let mut shm_path = self.path.as_os_str().to_os_string();
        shm_path.push("-shm");
        let mut file = fs::File::open(PathBuf::from(shm_path)).ok()?;
        let mut header = [0_u8; 100];
        file.seek(SeekFrom::Start(0)).ok()?;
        file.read_exact(&mut header).ok()?;
        if header[..48] != header[48..96] || header[12] == 0 {
            return None;
        }
        Some(WalBackfillSnapshot {
            salt: [
                u32::from_ne_bytes(header[32..36].try_into().ok()?),
                u32::from_ne_bytes(header[36..40].try_into().ok()?),
            ],
            frames: u32::from_ne_bytes(header[96..100].try_into().ok()?),
        })
    }

    fn checkpoint_wal(&self, mode: &'static str) -> Result<WalCheckpointOutcome> {
        let sql = match mode {
            "PASSIVE" => "PRAGMA wal_checkpoint(PASSIVE)",
            "RESTART" => "PRAGMA wal_checkpoint(RESTART)",
            "TRUNCATE" => "PRAGMA wal_checkpoint(TRUNCATE)",
            _ => unreachable!("unsupported WAL checkpoint mode"),
        };
        self.conn.busy_timeout(Duration::ZERO)?;
        let outcome = self
            .conn
            .query_row(sql, [], |row| {
                Ok(WalCheckpointOutcome {
                    busy: row.get::<_, i64>(0)? != 0,
                    log_frames: row.get(1)?,
                    checkpointed_frames: row.get(2)?,
                })
            })
            .map_err(StoreError::from);
        let restore = self
            .conn
            .busy_timeout(self.busy_timeout)
            .map_err(StoreError::from);
        match (outcome, restore) {
            (Ok(outcome), Ok(())) => Ok(outcome),
            (Err(error), _) | (_, Err(error)) => Err(error),
        }
    }

    fn checkpoint_wal_passive_measured(&self) -> Result<(WalCheckpointOutcome, u64)> {
        self.checkpoint_wal_measured("PASSIVE")
    }

    fn checkpoint_wal_measured(&self, mode: &'static str) -> Result<(WalCheckpointOutcome, u64)> {
        let wal_bytes_before = self.wal_bytes()?.unwrap_or(0);
        let before = self.wal_backfill_snapshot();
        let outcome = self.checkpoint_wal(mode)?;
        let after = self.wal_backfill_snapshot();
        let page_size = crate::work_control::sqlite_page_size(&self.conn).unwrap_or(4096);
        Ok((
            outcome,
            checkpointed_frames_conservative(before, after, outcome, wal_bytes_before, page_size),
        ))
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WalCheckpointOutcome {
    busy: bool,
    log_frames: i64,
    checkpointed_frames: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WalBackfillSnapshot {
    salt: [u32; 2],
    frames: u32,
}

fn checkpointed_frames_conservative(
    before: Option<WalBackfillSnapshot>,
    after: Option<WalBackfillSnapshot>,
    outcome: WalCheckpointOutcome,
    wal_bytes_before: u64,
    page_size: u64,
) -> u64 {
    let reported = u64::try_from(outcome.checkpointed_frames.max(0)).unwrap_or(0);
    let wal_bound = wal_frame_upper_bound(wal_bytes_before, page_size);
    match (before, after) {
        (Some(before), Some(after))
            if before.salt == after.salt && after.frames >= before.frames =>
        {
            u64::from(after.frames - before.frames)
        }
        (Some(before), Some(after)) => reported
            .max(wal_bound)
            .max(u64::from(before.frames))
            .max(u64::from(after.frames)),
        (before, after) => {
            // A missing or racing shm header must fail closed for pacing. The
            // PRAGMA counters are cumulative rather than a precise delta, so
            // use the larger of that conservative value and the WAL's maximum
            // possible frame count. Over-accounting only makes background work
            // quieter; returning zero can create an unmetered checkpoint storm.
            reported
                .max(wal_bound)
                .max(before.map_or(0, |snapshot| u64::from(snapshot.frames)))
                .max(after.map_or(0, |snapshot| u64::from(snapshot.frames)))
        }
    }
}

fn wal_frame_upper_bound(wal_bytes: u64, page_size: u64) -> u64 {
    const WAL_HEADER_BYTES: u64 = 32;
    const WAL_FRAME_HEADER_BYTES: u64 = 24;
    if wal_bytes <= WAL_HEADER_BYTES || page_size == 0 {
        return 0;
    }
    wal_bytes
        .saturating_sub(WAL_HEADER_BYTES)
        .div_ceil(page_size.saturating_add(WAL_FRAME_HEADER_BYTES))
}

pub(crate) fn configure_connection(conn: &Connection, busy_timeout: Duration) -> Result<()> {
    conn.busy_timeout(busy_timeout)?;
    conn.execute_batch(
        r#"
        PRAGMA foreign_keys = ON;
        PRAGMA journal_mode = WAL;
        PRAGMA synchronous = NORMAL;
        PRAGMA temp_store = MEMORY;
        PRAGMA cache_size = -32768;
        PRAGMA wal_autocheckpoint = 10000;
        "#,
    )?;
    Ok(())
}

fn sqlite_sidecar_exists(path: &Path, suffix: &str) -> bool {
    let mut sidecar = path.as_os_str().to_os_string();
    sidecar.push(suffix);
    Path::new(&sidecar).exists()
}

fn sqlite_read_only_immutable_uri(path: &Path) -> String {
    let mut uri = String::from("file:");
    for byte in path.to_string_lossy().as_bytes() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b':' | b'.' | b'_' | b'-' => {
                uri.push(*byte as char)
            }
            byte => {
                uri.push('%');
                uri.push_str(&format!("{byte:02X}"));
            }
        }
    }
    uri.push_str("?mode=ro&immutable=1");
    uri
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

#[cfg(test)]
mod checkpoint_work_tests {
    use super::{checkpointed_frames_conservative, WalBackfillSnapshot, WalCheckpointOutcome};

    #[test]
    fn checkpoint_work_counts_only_new_frames_and_handles_wal_reset() {
        let generation = [17, 29];
        assert_eq!(
            checkpointed_frames_conservative(
                Some(WalBackfillSnapshot {
                    salt: generation,
                    frames: 2_048,
                }),
                Some(WalBackfillSnapshot {
                    salt: generation,
                    frames: 2_049,
                }),
                WalCheckpointOutcome {
                    busy: false,
                    log_frames: 2_049,
                    checkpointed_frames: 2_049,
                },
                8 * 1024 * 1024,
                4096,
            ),
            1
        );
        assert_eq!(
            checkpointed_frames_conservative(
                Some(WalBackfillSnapshot {
                    salt: generation,
                    frames: 2_049,
                }),
                Some(WalBackfillSnapshot {
                    salt: [31, 37],
                    frames: 3,
                }),
                WalCheckpointOutcome {
                    busy: false,
                    log_frames: 3,
                    checkpointed_frames: 3,
                },
                8 * 1024 * 1024,
                4096,
            ),
            2_049
        );
        assert!(
            checkpointed_frames_conservative(
                None,
                None,
                WalCheckpointOutcome {
                    busy: false,
                    log_frames: 2_048,
                    checkpointed_frames: 2_048,
                },
                8 * 1024 * 1024,
                4096,
            ) > 0
        );
        assert_eq!(
            checkpointed_frames_conservative(
                Some(WalBackfillSnapshot {
                    salt: generation,
                    frames: 2_048,
                }),
                Some(WalBackfillSnapshot {
                    salt: generation,
                    frames: 0,
                }),
                WalCheckpointOutcome {
                    busy: false,
                    log_frames: 0,
                    checkpointed_frames: 0,
                },
                8 * 1024 * 1024,
                4096,
            ),
            2_048
        );
        assert_eq!(
            checkpointed_frames_conservative(
                Some(WalBackfillSnapshot {
                    salt: generation,
                    frames: 77,
                }),
                None,
                WalCheckpointOutcome {
                    busy: true,
                    log_frames: 0,
                    checkpointed_frames: 0,
                },
                0,
                4096,
            ),
            77
        );
    }
}
