//! Crash-safe FTS5 merge suppression and bounded compaction for bulk imports.
//!
//! FTS5 may perform an automatic or crisis merge inside a single row insert,
//! producing a WAL far larger than the imported data. Bulk mode persists a
//! recovery marker before disabling those merges. Event rows and their search
//! projections still commit together; interrupted work remains searchable.
//! Bounded merge steps run before the saved settings and marker are cleared.

use ctx_history_core::utc_now;
use std::{
    ffi::OsString,
    num::NonZeroUsize,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use rusqlite::{params, Connection, ErrorCode, OptionalExtension};

use crate::object_store::restrict_private_file;
use crate::schema::ddl::{ensure_search_projection_stats_table, table_exists};
use crate::{Result, Store, StoreError};

const EVENT_SEARCH_FTS_TABLES: [&str; 2] = ["event_search", "event_search_scriptgram"];
const ALL_FTS_TABLES: [&str; 5] = [
    "ctx_history_search",
    "event_search",
    "artifact_search",
    "ctx_history_search_scriptgram",
    "event_search_scriptgram",
];
const BULK_MODE_MARKER_KEY: &str = "event_search_bulk_mode_v1";
const BULK_MODE_AUTOMERGE_KEY_PREFIX: &str = "event_search_bulk_mode_v1:automerge:";
const BULK_MODE_CRISISMERGE_KEY_PREFIX: &str = "event_search_bulk_mode_v1:crisismerge:";
const FTS_AUTOMERGE_DEFAULT: i64 = 4;
const FTS_CRISISMERGE_DEFAULT: i64 = 16;
const FTS_BULK_CRISISMERGE: i64 = 1_000_000;
// FTS5's merge page budget is not a hard upper bound on WAL pages: merging a
// large segment can rewrite substantially more data inside one statement.
// Keep each step deliberately small so checkpoints remain safe on large real
// indexes, not only on compact synthetic fixtures.
const FTS_MERGE_PAGE_BUDGET: i64 = 16;
const BULK_LOCK_SUFFIX: &str = ".event-search-bulk.lock.sqlite";
pub const BULK_WRITE_BATCH_BYTES: usize = 8 * 1024 * 1024;
pub const BULK_WRITE_BATCH_UNITS: usize = 64;
const MAX_DEFERRED_MAINTENANCE_ROTATIONS: usize = 8;

/// Shared bounded write lifecycle for provider imports and search rebuilds.
pub struct BoundedBulkWriteTransaction {
    active: bool,
    batch_size: Option<NonZeroUsize>,
    units: usize,
    bytes: usize,
    deferred_maintenance_rotations: usize,
}

impl BoundedBulkWriteTransaction {
    pub fn begin(store: &Store, has_work: bool, batch_size: Option<NonZeroUsize>) -> Result<Self> {
        if has_work {
            store.begin_immediate_batch()?;
        }
        Ok(Self {
            active: has_work,
            batch_size,
            units: 0,
            bytes: 0,
            deferred_maintenance_rotations: 0,
        })
    }

    pub fn begin_bounded(store: &Store, has_work: bool) -> Result<Self> {
        Self::begin(store, has_work, NonZeroUsize::new(BULK_WRITE_BATCH_UNITS))
    }

    pub fn prepare_unit(&mut self, store: &Store, unit_bytes: usize) -> Result<()> {
        let result = if self.active
            && self.batch_size.is_some()
            && self.units > 0
            && self.bytes.saturating_add(unit_bytes) > BULK_WRITE_BATCH_BYTES
        {
            self.rotate(store)
        } else {
            Ok(())
        };
        if result.is_err() {
            self.rollback(store);
        }
        result
    }

    pub fn record_unit(&mut self, store: &Store, unit_bytes: usize) -> Result<()> {
        if !self.active {
            return Ok(());
        }
        self.units = self.units.saturating_add(1);
        self.bytes = self.bytes.saturating_add(unit_bytes);
        let below_unit_limit = self
            .batch_size
            .is_none_or(|batch_size| self.units < batch_size.get());
        let below_byte_limit = self.batch_size.is_none() || self.bytes < BULK_WRITE_BATCH_BYTES;
        if below_unit_limit && below_byte_limit {
            return Ok(());
        }
        let result = self.rotate(store);
        if result.is_err() {
            self.rollback(store);
        }
        result
    }

    fn rotate(&mut self, store: &Store) -> Result<()> {
        self.rotate_with_maintenance(store, || store.maintain_event_search_bulk_mode())
    }

    fn rotate_with_maintenance<F>(&mut self, store: &Store, maintain: F) -> Result<()>
    where
        F: FnOnce() -> Result<()>,
    {
        store.commit_batch()?;
        self.active = false;
        let mut deferred_error = None;
        if let Err(error) = maintain() {
            if !is_recoverable_bulk_maintenance_error(&error) {
                return Err(error);
            }
            deferred_error = Some(error);
        }
        if let Err(error) = store.checkpoint_wal_truncate_required() {
            if !is_recoverable_wal_checkpoint_error(&error) {
                return Err(error);
            }
            deferred_error.get_or_insert(error);
        }
        if let Some(error) = deferred_error {
            self.deferred_maintenance_rotations =
                self.deferred_maintenance_rotations.saturating_add(1);
            if self.deferred_maintenance_rotations >= MAX_DEFERRED_MAINTENANCE_ROTATIONS {
                return Err(error);
            }
        } else {
            self.deferred_maintenance_rotations = 0;
        }
        store.begin_immediate_batch()?;
        self.active = true;
        self.units = 0;
        self.bytes = 0;
        Ok(())
    }

    pub fn commit(&mut self, store: &Store) -> Result<()> {
        let result = if self.active {
            store.commit_batch()
        } else {
            Ok(())
        };
        if result.is_ok() {
            self.active = false;
        } else {
            self.rollback(store);
        }
        result
    }

    pub fn rollback(&mut self, store: &Store) {
        if self.active {
            let _ = store.rollback_batch();
            self.active = false;
        }
    }
}

/// Owns the cross-process lock for one event-search bulk operation.
///
/// SQLite releases the sidecar database's writer lock if the process exits,
/// including after an unclean exit. The guard intentionally cannot be cloned.
pub struct EventSearchBulkGuard {
    lock_conn: Option<Connection>,
    store_path: PathBuf,
    depth: Arc<AtomicUsize>,
    depth_counted: bool,
}

impl Drop for EventSearchBulkGuard {
    fn drop(&mut self) {
        if let Some(lock_conn) = &self.lock_conn {
            let _ = lock_conn.execute_batch("ROLLBACK");
        }
        if self.depth_counted {
            self.depth.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

impl Store {
    /// Acquire the bulk-import lock and persist merge suppression.
    pub fn begin_event_search_bulk_mode(&self) -> Result<EventSearchBulkGuard> {
        if self.event_search_bulk_depth.fetch_add(1, Ordering::SeqCst) > 0 {
            return Ok(EventSearchBulkGuard {
                lock_conn: None,
                store_path: self.path.clone(),
                depth: Arc::clone(&self.event_search_bulk_depth),
                depth_counted: true,
            });
        }
        let acquired = match self.acquire_event_search_bulk_lock(self.busy_timeout) {
            Ok(acquired) => acquired,
            Err(error) => {
                self.event_search_bulk_depth.fetch_sub(1, Ordering::SeqCst);
                return Err(error);
            }
        };
        let mut guard = match acquired {
            Some(guard) => guard,
            None => {
                self.event_search_bulk_depth.fetch_sub(1, Ordering::SeqCst);
                return Err(StoreError::BulkSearchImportBusy);
            }
        };
        guard.depth_counted = true;
        self.begin_immediate_batch()?;
        let result = (|| {
            ensure_search_projection_stats_table(&self.conn)?;
            if !bulk_mode_pending(self)? {
                for table in EVENT_SEARCH_FTS_TABLES {
                    if !table_exists(&self.conn, table)? {
                        continue;
                    }
                    save_bulk_mode_config(
                        self,
                        &format!("{BULK_MODE_AUTOMERGE_KEY_PREFIX}{table}"),
                        fts_config_value(self, table, "automerge", FTS_AUTOMERGE_DEFAULT)?,
                    )?;
                    save_bulk_mode_config(
                        self,
                        &format!("{BULK_MODE_CRISISMERGE_KEY_PREFIX}{table}"),
                        fts_config_value(self, table, "crisismerge", FTS_CRISISMERGE_DEFAULT)?,
                    )?;
                }
                save_bulk_mode_config(self, BULK_MODE_MARKER_KEY, 1)?;
            }
            suppress_event_search_merges(self)
        })();
        if let Err(err) = result {
            let _ = self.rollback_batch();
            return Err(err);
        }
        if let Err(err) = self.commit_batch() {
            let _ = self.rollback_batch();
            return Err(err);
        }
        Ok(guard)
    }

    /// Compact pending bulk segments in bounded steps, then restore saved settings.
    ///
    /// Bulk finalization deliberately uses positive FTS5 merge commands. Starting
    /// a full merge with a negative command would assign every pre-existing
    /// segment to the same level and rewrite the entire shared event index. That
    /// is appropriate for an explicit optimize, but not for finishing one
    /// provider import in an already-populated multi-source index.
    pub fn finish_event_search_bulk_mode(&self, guard: &EventSearchBulkGuard) -> Result<()> {
        if guard.store_path != self.path {
            return Err(StoreError::InvalidBulkSearchGuard);
        }
        if guard.lock_conn.is_none() {
            return Ok(());
        }
        if guard.depth_counted && guard.depth.load(Ordering::SeqCst) != 1 {
            return Err(StoreError::InvalidBulkSearchGuard);
        }
        if !bulk_mode_pending(self)? {
            return Ok(());
        }
        loop {
            if self.finish_event_search_bulk_mode_step()? {
                return Ok(());
            }
        }
    }

    /// Finish maintenance when possible, deferring only recoverable pressure.
    ///
    /// Callers drop `guard` after this returns. A maintenance failure is
    /// deliberately deferred: its committed import data stays searchable and
    /// the marker records the remaining work for a later writable open. A
    /// quiescent pass restores the saved settings and clears that marker.
    pub fn defer_event_search_bulk_mode(&self, guard: &EventSearchBulkGuard) -> Result<()> {
        self.defer_event_search_bulk_mode_with(guard, || self.finish_event_search_bulk_mode(guard))
    }

    fn defer_event_search_bulk_mode_with<F>(
        &self,
        guard: &EventSearchBulkGuard,
        maintain: F,
    ) -> Result<()>
    where
        F: FnOnce() -> Result<()>,
    {
        if guard.store_path != self.path {
            return Err(StoreError::InvalidBulkSearchGuard);
        }
        if guard.lock_conn.is_none() {
            return Ok(());
        }
        if guard.depth_counted && guard.depth.load(Ordering::SeqCst) != 1 {
            return Err(StoreError::InvalidBulkSearchGuard);
        }
        match maintain() {
            Ok(()) => Ok(()),
            Err(error) if is_recoverable_bulk_maintenance_error(&error) => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Perform one bounded merge pass without ending bulk mode.
    ///
    /// Importers use this between bounded write transactions. This method must
    /// never restore FTS settings or clear the marker: the enclosing import is
    /// still active and subsequent writes still require merge suppression.
    pub fn maintain_event_search_bulk_mode(&self) -> Result<()> {
        self.begin_immediate_batch()?;
        let result = (|| {
            if !bulk_mode_pending(self)? {
                return Ok(false);
            }
            merge_event_search_tables_in_transaction(self)
        })();
        match result {
            Ok(_) => {}
            Err(err) => {
                let _ = self.rollback_batch();
                return Err(err);
            }
        }
        if let Err(err) = self.commit_batch() {
            let _ = self.rollback_batch();
            return Err(err);
        }
        Ok(())
    }

    pub(crate) fn recover_event_search_bulk_mode(&self) -> Result<()> {
        // Check and reassert under one writer lock. A guarded importer may
        // restore settings and clear the marker while another connection is
        // waiting for this transaction, so an earlier check would be stale.
        self.begin_immediate_batch()?;
        let result = (|| {
            let pending = bulk_mode_pending(self)?;
            if pending {
                suppress_event_search_merges(self)?;
            }
            Ok(pending)
        })();
        let pending = match result {
            Ok(pending) => pending,
            Err(err) => {
                let _ = self.rollback_batch();
                return Err(err);
            }
        };
        if let Err(err) = self.commit_batch() {
            let _ = self.rollback_batch();
            return Err(err);
        }
        if !pending {
            return Ok(());
        }
        // A live importer owns this lock. A stale marker has no owner, so the
        // next writable open adopts it and completes the bounded merge steps.
        // Stopping after one step can leave automerge disabled indefinitely
        // when no later write happens to reopen the store.
        if let Some(guard) = self.acquire_event_search_bulk_lock(Duration::ZERO)? {
            self.finish_event_search_bulk_mode(&guard)?;
        }
        Ok(())
    }

    pub(crate) fn merge_all_fts_tables_bounded(&self) -> Result<()> {
        // Serialize unconditionally. Reading the marker before acquiring the
        // lock would let a new bulk import start in the handoff window.
        let guard = self
            .acquire_event_search_bulk_lock(self.busy_timeout)?
            .ok_or(StoreError::BulkSearchImportBusy)?;
        if bulk_mode_pending(self)? {
            self.finish_event_search_bulk_mode(&guard)?;
        }
        for table in ALL_FTS_TABLES {
            self.merge_fts_table_bounded(table, true)?;
        }
        Ok(())
    }

    fn merge_fts_table_bounded(
        &self,
        table: &'static str,
        mut start_full_merge: bool,
    ) -> Result<()> {
        if !table_exists(&self.conn, table)? {
            return Ok(());
        }
        loop {
            let page_budget = if start_full_merge {
                -FTS_MERGE_PAGE_BUDGET
            } else {
                FTS_MERGE_PAGE_BUDGET
            };
            let changed = self.merge_fts_table_step(table, page_budget)?;
            start_full_merge = false;
            if !changed {
                return Ok(());
            }
        }
    }

    fn merge_fts_table_step(&self, table: &'static str, page_budget: i64) -> Result<bool> {
        self.begin_immediate_batch()?;
        let result = merge_fts_table_in_transaction(self, table, page_budget);
        let changed = match result {
            Ok(changed) => changed,
            Err(err) => {
                let _ = self.rollback_batch();
                return Err(err);
            }
        };
        if let Err(err) = self.commit_batch() {
            let _ = self.rollback_batch();
            return Err(err);
        }
        self.checkpoint_wal_truncate_required()?;
        Ok(changed)
    }

    /// Perform one bounded merge on both tables from the same writer snapshot.
    /// A quiescent pass is checkpointed before a second locked pass may restore
    /// settings, so a failed large-WAL checkpoint always leaves recovery marked.
    fn finish_event_search_bulk_mode_step(&self) -> Result<bool> {
        self.begin_immediate_batch()?;
        let result = (|| {
            if !bulk_mode_pending(self)? {
                return Ok(true);
            }
            Ok(!merge_event_search_tables_in_transaction(self)?)
        })();
        let quiescent = match result {
            Ok(quiescent) => quiescent,
            Err(err) => {
                let _ = self.rollback_batch();
                return Err(err);
            }
        };
        if let Err(err) = self.commit_batch() {
            let _ = self.rollback_batch();
            return Err(err);
        }
        self.checkpoint_wal_truncate_required()?;
        if !quiescent {
            return Ok(false);
        }
        self.restore_event_search_bulk_mode_if_quiescent()
    }

    /// Recheck both tables and restore settings while holding one writer lock.
    /// If the final config-only checkpoint is pinned, the preceding potentially
    /// large merge WAL has already been truncated successfully.
    fn restore_event_search_bulk_mode_if_quiescent(&self) -> Result<bool> {
        self.begin_immediate_batch()?;
        let result = (|| {
            if !bulk_mode_pending(self)? {
                return Ok(true);
            }
            let changed = merge_event_search_tables_in_transaction(self)?;
            if !changed {
                restore_event_search_merge_config(self)?;
                clear_bulk_mode_state(self)?;
            }
            Ok(!changed)
        })();
        let finished = match result {
            Ok(finished) => finished,
            Err(err) => {
                let _ = self.rollback_batch();
                return Err(err);
            }
        };
        if let Err(err) = self.commit_batch() {
            let _ = self.rollback_batch();
            return Err(err);
        }
        self.checkpoint_wal_truncate_required()?;
        Ok(finished)
    }

    fn acquire_event_search_bulk_lock(
        &self,
        busy_timeout: Duration,
    ) -> Result<Option<EventSearchBulkGuard>> {
        let lock_path = event_search_bulk_lock_path(&self.path);
        let lock_conn = Connection::open(&lock_path)?;
        restrict_private_file(&lock_path)?;
        lock_conn.busy_timeout(busy_timeout)?;
        let result = lock_conn.execute_batch(
            "PRAGMA journal_mode=DELETE;\
             CREATE TABLE IF NOT EXISTS bulk_search_lock (id INTEGER PRIMARY KEY);\
             BEGIN IMMEDIATE",
        );
        match result {
            Ok(()) => Ok(Some(EventSearchBulkGuard {
                lock_conn: Some(lock_conn),
                store_path: self.path.clone(),
                depth: Arc::clone(&self.event_search_bulk_depth),
                depth_counted: false,
            })),
            Err(err) if sqlite_is_busy(&err) => Ok(None),
            Err(err) => Err(err.into()),
        }
    }
}

/// Whether a bulk-maintenance failure is temporary pressure that can safely
/// remain marked for a later bounded retry.
pub fn is_recoverable_bulk_maintenance_error(error: &StoreError) -> bool {
    match error {
        StoreError::WalCheckpointBusy { .. } | StoreError::BulkSearchImportBusy => true,
        StoreError::Sql(rusqlite::Error::SqliteFailure(failure, _)) => matches!(
            failure.code,
            ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked
        ),
        _ => false,
    }
}

/// Whether a WAL checkpoint failure is temporary pressure that can be retried
/// without treating exhausted disk space as recoverable.
pub fn is_recoverable_wal_checkpoint_error(error: &StoreError) -> bool {
    is_recoverable_bulk_maintenance_error(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn deferred_maintenance_propagates_fatal_errors() {
        let temp = tempdir().unwrap();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let guard = store.begin_event_search_bulk_mode().unwrap();

        let error = store
            .defer_event_search_bulk_mode_with(&guard, || {
                Err(StoreError::Sql(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
                    None,
                )))
            })
            .unwrap_err();
        assert!(matches!(error, StoreError::Sql(_)));
    }

    #[test]
    fn deferred_maintenance_suppresses_temporary_pressure() {
        let temp = tempdir().unwrap();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let guard = store.begin_event_search_bulk_mode().unwrap();

        store
            .defer_event_search_bulk_mode_with(&guard, || {
                Err(StoreError::WalCheckpointBusy {
                    log_frames: 2,
                    checkpointed_frames: 1,
                })
            })
            .unwrap();
    }

    #[test]
    fn disk_full_is_fatal_for_bulk_maintenance_and_checkpoint() {
        let error = StoreError::Sql(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_FULL),
            None,
        ));
        assert!(!is_recoverable_bulk_maintenance_error(&error));
        assert!(!is_recoverable_wal_checkpoint_error(&error));
    }

    #[test]
    fn bounded_rotation_propagates_fatal_maintenance_errors() {
        let temp = tempdir().unwrap();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let mut transaction = BoundedBulkWriteTransaction::begin_bounded(&store, true).unwrap();
        let error = transaction
            .rotate_with_maintenance(&store, || {
                Err(StoreError::Sql(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
                    None,
                )))
            })
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("database disk image is malformed"));
    }

    #[test]
    fn bounded_rotation_defers_then_stops_on_sustained_pressure() {
        let temp = tempdir().unwrap();
        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        let mut transaction = BoundedBulkWriteTransaction::begin_bounded(&store, true).unwrap();
        for _ in 1..MAX_DEFERRED_MAINTENANCE_ROTATIONS {
            transaction
                .rotate_with_maintenance(&store, || {
                    Err(StoreError::WalCheckpointBusy {
                        log_frames: 2,
                        checkpointed_frames: 1,
                    })
                })
                .unwrap();
        }
        let error = transaction
            .rotate_with_maintenance(&store, || {
                Err(StoreError::WalCheckpointBusy {
                    log_frames: 2,
                    checkpointed_frames: 1,
                })
            })
            .unwrap_err();
        assert!(matches!(error, StoreError::WalCheckpointBusy { .. }));
    }
}

fn merge_fts_table_in_transaction(
    store: &Store,
    table: &'static str,
    page_budget: i64,
) -> Result<bool> {
    let before = store.conn.total_changes();
    let sql = format!("INSERT INTO {table}({table}, rank) VALUES ('merge', ?1)");
    store.conn.execute(&sql, params![page_budget])?;
    Ok(store.conn.total_changes().saturating_sub(before) >= 2)
}

fn merge_event_search_tables_in_transaction(store: &Store) -> Result<bool> {
    let mut changed = false;
    for table in EVENT_SEARCH_FTS_TABLES {
        if table_exists(&store.conn, table)? {
            changed |= merge_fts_table_in_transaction(store, table, FTS_MERGE_PAGE_BUDGET)?;
        }
    }
    Ok(changed)
}

fn event_search_bulk_lock_path(store_path: &std::path::Path) -> PathBuf {
    let mut value = OsString::from(store_path.as_os_str());
    value.push(BULK_LOCK_SUFFIX);
    PathBuf::from(value)
}

fn sqlite_is_busy(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(failure, _)
            if matches!(failure.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}

fn suppress_event_search_merges(store: &Store) -> Result<()> {
    for table in EVENT_SEARCH_FTS_TABLES {
        if !table_exists(&store.conn, table)? {
            continue;
        }
        set_fts_config(store, table, "automerge", 0)?;
        set_fts_config(store, table, "crisismerge", FTS_BULK_CRISISMERGE)?;
    }
    Ok(())
}

fn restore_event_search_merge_config(store: &Store) -> Result<()> {
    for table in EVENT_SEARCH_FTS_TABLES {
        if !table_exists(&store.conn, table)? {
            continue;
        }
        let automerge =
            bulk_mode_config(store, &format!("{BULK_MODE_AUTOMERGE_KEY_PREFIX}{table}"))?
                .unwrap_or(FTS_AUTOMERGE_DEFAULT);
        let crisismerge =
            bulk_mode_config(store, &format!("{BULK_MODE_CRISISMERGE_KEY_PREFIX}{table}"))?
                .unwrap_or(FTS_CRISISMERGE_DEFAULT);
        set_fts_config(store, table, "automerge", automerge)?;
        set_fts_config(store, table, "crisismerge", crisismerge)?;
    }
    Ok(())
}

fn set_fts_config(store: &Store, table: &'static str, key: &str, value: i64) -> Result<()> {
    debug_assert!(ALL_FTS_TABLES.contains(&table));
    let sql = format!("INSERT INTO {table}({table}, rank) VALUES (?1, ?2)");
    store.conn.execute(&sql, params![key, value])?;
    Ok(())
}

fn fts_config_value(store: &Store, table: &'static str, key: &str, default: i64) -> Result<i64> {
    debug_assert!(ALL_FTS_TABLES.contains(&table));
    let sql = format!("SELECT v FROM {table}_config WHERE k = ?1");
    Ok(store
        .conn
        .query_row(&sql, params![key], |row| row.get(0))
        .optional()?
        .unwrap_or(default))
}

fn bulk_mode_pending(store: &Store) -> Result<bool> {
    if !table_exists(&store.conn, "search_projection_stats")? {
        return Ok(false);
    }
    Ok(bulk_mode_config(store, BULK_MODE_MARKER_KEY)?.is_some())
}

fn bulk_mode_config(store: &Store, key: &str) -> Result<Option<i64>> {
    Ok(store
        .conn
        .query_row(
            "SELECT value FROM search_projection_stats WHERE key = ?1",
            params![key],
            |row| row.get(0),
        )
        .optional()?)
}

fn save_bulk_mode_config(store: &Store, key: &str, value: i64) -> Result<()> {
    store.conn.execute(
        r#"
        INSERT INTO search_projection_stats (key, value, updated_at_ms)
        VALUES (?1, ?2, ?3)
        ON CONFLICT(key) DO UPDATE SET
            value = excluded.value,
            updated_at_ms = excluded.updated_at_ms
        "#,
        params![key, value, utc_now().timestamp_millis()],
    )?;
    Ok(())
}

fn clear_bulk_mode_state(store: &Store) -> Result<()> {
    store.conn.execute(
        "DELETE FROM search_projection_stats WHERE key = ?1 OR key LIKE ?2",
        params![BULK_MODE_MARKER_KEY, "event_search_bulk_mode_v1:%"],
    )?;
    Ok(())
}
