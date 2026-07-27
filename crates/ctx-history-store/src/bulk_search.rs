//! Crash-safe FTS5 merge suppression and bounded maintenance for bulk imports.
//!
//! FTS5 may perform an automatic or crisis merge inside a single row insert,
//! producing a WAL far larger than the imported data. Bulk mode persists a
//! recovery marker before disabling automerge while retaining a safe crisis
//! guard. Event rows and their search projections still commit together;
//! interrupted work remains searchable.
//! Finishing a bulk group restores the saved settings and durably schedules
//! bounded compaction without making cursor publication wait for a fully merged
//! index or a truncating WAL checkpoint.

use ctx_history_core::utc_now;
use std::{
    ffi::OsString,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use rusqlite::{params, Connection, ErrorCode, OptionalExtension};

use crate::object_store::restrict_private_file;
use crate::schema::ddl::table_exists;
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
const MAINTENANCE_PENDING_KEY: &str = "event_search_maintenance_v1";
const MAINTENANCE_GROUPS_KEY: &str = "event_search_maintenance_v1:groups";
const FTS_AUTOMERGE_DEFAULT: i64 = 4;
const FTS_CRISISMERGE_DEFAULT: i64 = 16;
// FTS5 has a hard total-segment ceiling of 2,000. Values at or above that are
// clamped to 1,999 by SQLite, but crisis merge counts segments per level, so
// that clamp does not protect the total ceiling (GitHub #181). Keep the normal
// safe crisis guard while disabling only automerge. Ordinary bounded
// maintenance should keep it from firing; it remains the in-transaction safety
// net if one unusually dense captured batch creates many segments at once.
const FTS5_MAX_SEGMENTS: i64 = 2_000;
const FTS_BULK_CRISISMERGE: i64 = FTS_CRISISMERGE_DEFAULT;
const FTS5_SEGMENT_GUARD: i64 = 1_024;
const FTS5_SEGMENT_RESUME: i64 = 768;
const MAINTENANCE_GROUP_INTERVAL: i64 = 8;
const MAINTENANCE_STEPS_PER_SLICE: usize = 4;
const CRISIS_MAINTENANCE_SLICES: usize = 16;
const BULK_WAL_HARD_LIMIT_BYTES: u64 = 64 * 1024 * 1024;
const _: () = assert!(FTS_BULK_CRISISMERGE < FTS5_SEGMENT_GUARD);
const _: () = assert!(FTS5_SEGMENT_GUARD < FTS5_MAX_SEGMENTS);
// FTS5's merge page budget is not a hard upper bound on WAL pages: merging a
// large segment can rewrite substantially more data inside one statement.
// Keep each step deliberately small so checkpoints remain safe on large real
// indexes, not only on compact synthetic fixtures.
const FTS_MERGE_PAGE_BUDGET: i64 = 16;
const BULK_LOCK_SUFFIX: &str = ".event-search-bulk.lock.sqlite";
const SOURCE_INVENTORY_LOCK_SUFFIX: &str = ".source-inventory.lock.sqlite";

#[cfg(test)]
std::thread_local! {
    static TEST_BULK_WAL_HARD_LIMIT_BYTES: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
    static TEST_FTS5_SEGMENT_GUARD: std::cell::Cell<Option<i64>> = const { std::cell::Cell::new(None) };
    static TEST_MAINTENANCE_SLICE_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) struct EventSearchBulkTestLimits {
    previous_wal_limit: Option<u64>,
    previous_segment_guard: Option<i64>,
}

#[cfg(test)]
impl Drop for EventSearchBulkTestLimits {
    fn drop(&mut self) {
        TEST_BULK_WAL_HARD_LIMIT_BYTES.with(|limit| limit.set(self.previous_wal_limit));
        TEST_FTS5_SEGMENT_GUARD.with(|guard| guard.set(self.previous_segment_guard));
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

pub struct SourceInventoryGuard {
    lock_conn: Connection,
}

impl Drop for SourceInventoryGuard {
    fn drop(&mut self) {
        let _ = self.lock_conn.execute_batch("ROLLBACK");
    }
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
    pub fn acquire_source_inventory_lock(&self) -> Result<SourceInventoryGuard> {
        let lock_path = store_sidecar_lock_path(&self.path, SOURCE_INVENTORY_LOCK_SUFFIX);
        let lock_conn = Connection::open(&lock_path)?;
        restrict_private_file(&lock_path)?;
        lock_conn.busy_timeout(self.busy_timeout)?;
        let result = lock_conn.execute_batch(
            "PRAGMA journal_mode=DELETE;\
             CREATE TABLE IF NOT EXISTS source_inventory_lock (id INTEGER PRIMARY KEY);\
             BEGIN IMMEDIATE",
        );
        match result {
            Ok(()) => Ok(SourceInventoryGuard { lock_conn }),
            Err(error) if sqlite_is_busy(&error) => Err(StoreError::SourceInventoryBusy),
            Err(error) => Err(error.into()),
        }
    }

    /// Acquire the bulk-import lock and persist merge suppression.
    pub fn begin_event_search_bulk_mode(&self) -> Result<EventSearchBulkGuard> {
        if self.event_search_bulk_depth.fetch_add(1, Ordering::SeqCst) > 0 {
            if let Err(error) = self.enforce_bulk_wal_bound() {
                self.event_search_bulk_depth.fetch_sub(1, Ordering::SeqCst);
                return Err(error);
            }
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
        self.enforce_bulk_wal_bound()?;
        self.run_event_search_maintenance_if_due()?;
        // A due slice may itself grow the WAL, and legacy databases can reopen
        // with a high segment count but no durable debt marker. Audit both
        // boundaries before admitting the next bounded provider group.
        self.ensure_event_search_segment_headroom()?;
        self.enforce_bulk_wal_bound()?;
        self.begin_immediate_batch()?;
        let result = (|| {
            ensure_search_projection_stats_table(self)?;
            if !bulk_mode_pending(self)? {
                for table in EVENT_SEARCH_FTS_TABLES {
                    if !table_exists(&self.conn, table)? {
                        continue;
                    }
                    save_search_stat(
                        self,
                        &format!("{BULK_MODE_AUTOMERGE_KEY_PREFIX}{table}"),
                        fts_config_value(self, table, "automerge", FTS_AUTOMERGE_DEFAULT)?,
                    )?;
                    save_search_stat(
                        self,
                        &format!("{BULK_MODE_CRISISMERGE_KEY_PREFIX}{table}"),
                        fts_config_value(self, table, "crisismerge", FTS_CRISISMERGE_DEFAULT)?,
                    )?;
                }
                save_search_stat(self, BULK_MODE_MARKER_KEY, 1)?;
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

    /// Restore merge settings and durably schedule bounded maintenance.
    ///
    /// FTS rows were committed in the same transactions as their events, so
    /// neither search visibility nor cursor safety depends on completing segment
    /// compaction here. Keeping this handoff small avoids a full positive-merge
    /// drain and strict WAL truncation for every four-batch publication group.
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
        self.begin_immediate_batch()?;
        let result = (|| {
            if !bulk_mode_pending(self)? {
                return Ok(());
            }
            restore_event_search_merge_config(self)?;
            schedule_event_search_maintenance(self)?;
            clear_bulk_mode_state(self)
        })();
        if let Err(err) = result {
            let _ = self.rollback_batch();
            return Err(err);
        }
        if let Err(err) = self.commit_batch() {
            let _ = self.rollback_batch();
            return Err(err);
        }
        Ok(())
    }

    pub(crate) fn recover_event_search_bulk_mode(&self) -> Result<()> {
        let stale_bulk_suppression = bulk_mode_pending(self)?;
        if !stale_bulk_suppression && !event_search_maintenance_due(self)? {
            return Ok(());
        }
        // A live importer owns this lock. A stale suppression marker has no
        // owner, so the next writable open restores its settings and converts
        // it to ordinary durable maintenance. Recovery performs at most one
        // bounded slice; remaining work stays marked for later opens/groups.
        let Some(guard) = self.acquire_event_search_bulk_lock(Duration::ZERO)? else {
            return Ok(());
        };
        let recovered_stale_bulk_suppression = bulk_mode_pending(self)?;
        if recovered_stale_bulk_suppression {
            self.finish_event_search_bulk_mode(&guard)?;
        }
        if recovered_stale_bulk_suppression || event_search_maintenance_due(self)? {
            self.run_event_search_maintenance_slice(MAINTENANCE_STEPS_PER_SLICE)?;
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
        self.clear_event_search_maintenance()?;
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
                return Err(if EVENT_SEARCH_FTS_TABLES.contains(&table) {
                    diagnose_event_search_sqlite_full(self, err)
                } else {
                    err
                });
            }
        };
        if let Err(err) = self.commit_batch() {
            let _ = self.rollback_batch();
            return Err(err);
        }
        self.checkpoint_wal_truncate_required()?;
        Ok(changed)
    }

    fn enforce_bulk_wal_bound(&self) -> Result<()> {
        let mut wal_path = OsString::from(self.path.as_os_str());
        wal_path.push("-wal");
        match std::fs::metadata(PathBuf::from(wal_path)) {
            Ok(metadata) if metadata.len() >= bulk_wal_hard_limit_bytes() => {
                self.checkpoint_wal_truncate_required()
            }
            Ok(_) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(StoreError::Io(err)),
        }
    }

    fn run_event_search_maintenance_if_due(&self) -> Result<()> {
        if !event_search_maintenance_due(self)? {
            return Ok(());
        }
        self.ensure_event_search_segment_headroom()?;
        self.run_event_search_maintenance_slice(MAINTENANCE_STEPS_PER_SLICE)?;
        Ok(())
    }

    /// Run a fixed number of positive merge commands in one transaction.
    ///
    /// Positive commands preserve already-optimized historical levels. The
    /// durable marker is cleared only after a command reports quiescence; a
    /// crash or a slice that exhausts its budget leaves the work discoverable.
    fn run_event_search_maintenance_slice(&self, max_steps: usize) -> Result<bool> {
        if !event_search_maintenance_pending(self)? {
            return Ok(false);
        }
        #[cfg(test)]
        TEST_MAINTENANCE_SLICE_CALLS.with(|calls| calls.set(calls.get().saturating_add(1)));
        self.begin_immediate_batch()?;
        let result = (|| {
            if !event_search_maintenance_pending(self)? {
                return Ok(false);
            }
            save_search_stat(self, MAINTENANCE_GROUPS_KEY, 0)?;
            let mut changed_any = false;
            let mut quiescent = false;
            for _ in 0..max_steps {
                let changed = merge_event_search_tables_in_transaction(self)?;
                changed_any |= changed;
                if !changed {
                    quiescent = true;
                    break;
                }
            }
            if quiescent {
                clear_event_search_maintenance_state(self)?;
            }
            Ok(changed_any)
        })();
        let changed = match result {
            Ok(changed) => changed,
            Err(err) => {
                let _ = self.rollback_batch();
                return Err(diagnose_event_search_sqlite_full(self, err));
            }
        };
        if let Err(err) = self.commit_batch() {
            let _ = self.rollback_batch();
            return Err(err);
        }
        // Passive checkpointing never makes maintenance or cursor publication
        // fail behind a pinned reader. The next group enforces the hard WAL
        // threshold before admitting additional bounded work.
        self.checkpoint_wal_passive()?;
        Ok(changed)
    }

    fn ensure_event_search_segment_headroom(&self) -> Result<()> {
        for table in EVENT_SEARCH_FTS_TABLES {
            let mut segments = event_search_segment_count(self, table)?;
            if segments < fts5_segment_guard() {
                continue;
            }
            for _ in 0..CRISIS_MAINTENANCE_SLICES {
                if !event_search_maintenance_pending(self)? {
                    self.begin_immediate_batch()?;
                    let schedule_result = schedule_event_search_maintenance(self);
                    if let Err(err) = schedule_result {
                        let _ = self.rollback_batch();
                        return Err(err);
                    }
                    if let Err(err) = self.commit_batch() {
                        let _ = self.rollback_batch();
                        return Err(err);
                    }
                }
                let changed =
                    self.run_event_search_maintenance_slice(MAINTENANCE_STEPS_PER_SLICE)?;
                segments = event_search_segment_count(self, table)?;
                if segments < fts5_segment_resume() {
                    break;
                }
                if !changed {
                    return event_search_segment_headroom_result(table, segments);
                }
            }
            event_search_segment_headroom_result(table, segments)?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn event_search_segment_guard_diagnostic_for_test(
        table: &'static str,
        segments: i64,
    ) -> Result<()> {
        event_search_segment_headroom_result(table, segments)
    }

    #[cfg(test)]
    pub(crate) fn event_search_bulk_test_limits(
        wal_limit_bytes: Option<u64>,
        segment_guard: Option<i64>,
    ) -> EventSearchBulkTestLimits {
        assert!(wal_limit_bytes.is_none_or(|limit| limit > 0));
        assert!(segment_guard.is_none_or(|guard| guard > 0));
        let previous_wal_limit =
            TEST_BULK_WAL_HARD_LIMIT_BYTES.with(|limit| limit.replace(wal_limit_bytes));
        let previous_segment_guard =
            TEST_FTS5_SEGMENT_GUARD.with(|guard| guard.replace(segment_guard));
        EventSearchBulkTestLimits {
            previous_wal_limit,
            previous_segment_guard,
        }
    }

    #[cfg(test)]
    pub(crate) fn reset_event_search_maintenance_slice_calls_for_test() {
        TEST_MAINTENANCE_SLICE_CALLS.with(|calls| calls.set(0));
    }

    #[cfg(test)]
    pub(crate) fn event_search_maintenance_slice_calls_for_test() -> usize {
        TEST_MAINTENANCE_SLICE_CALLS.with(std::cell::Cell::get)
    }

    fn clear_event_search_maintenance(&self) -> Result<()> {
        if !event_search_maintenance_pending(self)? {
            return Ok(());
        }
        self.begin_immediate_batch()?;
        if let Err(err) = clear_event_search_maintenance_state(self) {
            let _ = self.rollback_batch();
            return Err(err);
        }
        if let Err(err) = self.commit_batch() {
            let _ = self.rollback_batch();
            return Err(err);
        }
        self.checkpoint_wal_truncate_required()
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
    store_sidecar_lock_path(store_path, BULK_LOCK_SUFFIX)
}

fn store_sidecar_lock_path(store_path: &std::path::Path, suffix: &str) -> PathBuf {
    let mut value = OsString::from(store_path.as_os_str());
    value.push(suffix);
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

fn ensure_search_projection_stats_table(store: &Store) -> Result<()> {
    store.conn.execute(
        r#"
        CREATE TABLE IF NOT EXISTS search_projection_stats (
            key TEXT PRIMARY KEY NOT NULL,
            value INTEGER NOT NULL,
            updated_at_ms INTEGER NOT NULL
        )
        "#,
        [],
    )?;
    Ok(())
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

fn save_search_stat(store: &Store, key: &str, value: i64) -> Result<()> {
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

fn schedule_event_search_maintenance(store: &Store) -> Result<()> {
    save_search_stat(store, MAINTENANCE_PENDING_KEY, 1)?;
    store.conn.execute(
        r#"
        INSERT INTO search_projection_stats (key, value, updated_at_ms)
        VALUES (?1, 1, ?2)
        ON CONFLICT(key) DO UPDATE SET
            value = CASE
                WHEN value < ?3 THEN value + 1
                ELSE value
            END,
            updated_at_ms = excluded.updated_at_ms
        "#,
        params![
            MAINTENANCE_GROUPS_KEY,
            utc_now().timestamp_millis(),
            MAINTENANCE_GROUP_INTERVAL,
        ],
    )?;
    Ok(())
}

fn event_search_maintenance_pending(store: &Store) -> Result<bool> {
    if !table_exists(&store.conn, "search_projection_stats")? {
        return Ok(false);
    }
    Ok(bulk_mode_config(store, MAINTENANCE_PENDING_KEY)?.is_some())
}

fn event_search_maintenance_groups(store: &Store) -> Result<i64> {
    Ok(bulk_mode_config(store, MAINTENANCE_GROUPS_KEY)?
        .unwrap_or(0)
        .max(0))
}

fn event_search_maintenance_due(store: &Store) -> Result<bool> {
    Ok(event_search_maintenance_pending(store)?
        && event_search_maintenance_groups(store)? >= MAINTENANCE_GROUP_INTERVAL)
}

fn bulk_wal_hard_limit_bytes() -> u64 {
    #[cfg(test)]
    if let Some(limit) = TEST_BULK_WAL_HARD_LIMIT_BYTES.with(std::cell::Cell::get) {
        return limit;
    }
    BULK_WAL_HARD_LIMIT_BYTES
}

fn fts5_segment_guard() -> i64 {
    #[cfg(test)]
    if let Some(guard) = TEST_FTS5_SEGMENT_GUARD.with(std::cell::Cell::get) {
        return guard;
    }
    FTS5_SEGMENT_GUARD
}

fn fts5_segment_resume() -> i64 {
    #[cfg(test)]
    if let Some(guard) = TEST_FTS5_SEGMENT_GUARD.with(std::cell::Cell::get) {
        return guard.saturating_mul(3) / 4;
    }
    FTS5_SEGMENT_RESUME
}

fn event_search_segment_count(store: &Store, table: &'static str) -> Result<i64> {
    debug_assert!(EVENT_SEARCH_FTS_TABLES.contains(&table));
    if !table_exists(&store.conn, table)? {
        return Ok(0);
    }
    let sql = format!("SELECT COUNT(DISTINCT segid) FROM {table}_idx");
    Ok(store.conn.query_row(&sql, [], |row| row.get(0))?)
}

fn event_search_segment_headroom_result(table: &'static str, segments: i64) -> Result<()> {
    let guard = fts5_segment_guard();
    if segments < guard {
        return Ok(());
    }
    Err(StoreError::EventSearchSegmentLimit {
        table,
        segments,
        guard,
        hard_limit: FTS5_MAX_SEGMENTS,
    })
}

fn diagnose_event_search_sqlite_full(store: &Store, error: StoreError) -> StoreError {
    if !matches!(
        &error,
        StoreError::Sql(rusqlite::Error::SqliteFailure(failure, _))
            if failure.code == ErrorCode::DiskFull
    ) {
        return error;
    }
    let mut largest = None;
    for table in EVENT_SEARCH_FTS_TABLES {
        let Ok(segments) = event_search_segment_count(store, table) else {
            return error;
        };
        if largest.is_none_or(|(_, largest_count)| segments > largest_count) {
            largest = Some((table, segments));
        }
    }
    match largest {
        Some((table, segments)) if segments >= FTS5_MAX_SEGMENTS => {
            StoreError::EventSearchSegmentLimit {
                table,
                segments,
                guard: FTS5_SEGMENT_GUARD,
                hard_limit: FTS5_MAX_SEGMENTS,
            }
        }
        _ => error,
    }
}

fn clear_bulk_mode_state(store: &Store) -> Result<()> {
    store.conn.execute(
        "DELETE FROM search_projection_stats WHERE key = ?1 OR key LIKE ?2",
        params![BULK_MODE_MARKER_KEY, "event_search_bulk_mode_v1:%"],
    )?;
    Ok(())
}

fn clear_event_search_maintenance_state(store: &Store) -> Result<()> {
    store.conn.execute(
        "DELETE FROM search_projection_stats WHERE key = ?1 OR key = ?2",
        params![MAINTENANCE_PENDING_KEY, MAINTENANCE_GROUPS_KEY],
    )?;
    Ok(())
}
