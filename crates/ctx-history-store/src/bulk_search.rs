//! Crash-safe FTS5 merge suppression and bounded compaction for bulk imports.
//!
//! FTS5 may perform an automatic or crisis merge inside a single row insert,
//! producing a WAL far larger than the imported data. Bulk mode persists a
//! recovery marker before disabling those merges. Event rows and their search
//! projections still commit together; interrupted work remains searchable.
//! Bounded merge steps run before the saved settings and marker are cleared.

use ctx_history_core::utc_now;
use std::{
    cell::Cell,
    ffi::OsString,
    fs,
    marker::PhantomData,
    path::PathBuf,
    rc::Rc,
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
#[cfg(test)]
const BULK_MODE_TEST_REMAINING_MERGE_PASSES_KEY: &str =
    "event_search_bulk_mode_v1:test_remaining_merge_passes";
const FTS_AUTOMERGE_DEFAULT: i64 = 4;
const FTS_CRISISMERGE_DEFAULT: i64 = 16;
const FTS_BULK_CRISISMERGE: i64 = 1_000_000;
// FTS5's merge page budget is not a hard upper bound on WAL pages: merging a
// large segment can rewrite substantially more data inside one statement.
// Keep each step deliberately small so checkpoints remain safe on large real
// indexes, not only on compact synthetic fixtures.
const FTS_MERGE_PAGE_BUDGET: i64 = 16;
// Target one WAL write and one checkpoint copy per requested or observed FTS
// byte. FTS5 may amplify both operations beyond the nominal page estimate.
const FTS_MAINTENANCE_IO_PASSES: u64 = 2;
// Preserve the common changed-pass plus quiescence-pass completion path while
// putting a hard ceiling on large or resumed merge backlogs.
const EVENT_SEARCH_MERGE_PASSES_PER_CALL: usize = 2;
const BULK_LOCK_SUFFIX: &str = ".event-search-bulk.lock.sqlite";

thread_local! {
    static EVENT_SEARCH_MAINTENANCE_PACER: Cell<Option<fn(u64)>> = const { Cell::new(None) };
}

/// Restores the previous thread-local event-search maintenance pacer on drop.
#[doc(hidden)]
pub struct EventSearchMaintenancePacingGuard {
    previous: Option<fn(u64)>,
    _not_send: PhantomData<Rc<()>>,
}

impl Drop for EventSearchMaintenancePacingGuard {
    fn drop(&mut self) {
        EVENT_SEARCH_MAINTENANCE_PACER.with(|slot| slot.set(self.previous.take()));
    }
}

/// Installs I/O accounting for bounded FTS merge and checkpoint work.
///
/// Merge work is precharged from its logical page budget, then supplemented to
/// cover an observed SQLite WAL write and checkpoint copy before each required
/// truncating checkpoint.
#[doc(hidden)]
pub fn install_event_search_maintenance_pacer(pacer: fn(u64)) -> EventSearchMaintenancePacingGuard {
    let previous = EVENT_SEARCH_MAINTENANCE_PACER.with(|slot| slot.replace(Some(pacer)));
    EventSearchMaintenancePacingGuard {
        previous,
        _not_send: PhantomData,
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

/// Result of one bounded event-search maintenance slice.
///
/// `Pending` is durable: the bulk-mode marker remains present and a later
/// maintenance call can resume without restarting the merge. It can also mean
/// that an in-progress provider publication currently fences FTS maintenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventSearchBulkMaintenanceOutcome {
    Complete,
    Pending,
}

impl EventSearchBulkMaintenanceOutcome {
    pub fn is_complete(self) -> bool {
        self == Self::Complete
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
        if bulk_mode_pending(self)? && self.has_pending_provider_file_publications()? {
            return Ok(guard);
        }
        self.begin_immediate_batch()?;
        let result = (|| {
            ensure_search_projection_stats_table(self)?;
            let pending = bulk_mode_pending(self)?;
            if !pending {
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
            if pending && self.has_pending_provider_file_publications()? {
                Ok(())
            } else {
                suppress_event_search_merges(self)
            }
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

    /// Advance pending bulk compaction by one bounded slice.
    ///
    /// Bulk finalization deliberately uses positive FTS5 merge commands. Starting
    /// a full merge with a negative command would assign every pre-existing
    /// segment to the same level and rewrite the entire shared event index. That
    /// is appropriate for an explicit optimize, but not for finishing one
    /// provider import in an already-populated multi-source index.
    pub fn finish_event_search_bulk_mode(
        &self,
        guard: &EventSearchBulkGuard,
    ) -> Result<EventSearchBulkMaintenanceOutcome> {
        if guard.store_path != self.path {
            return Err(StoreError::InvalidBulkSearchGuard);
        }
        if guard.lock_conn.is_none() {
            // The outer guard owns the durable marker and final merge. The
            // nested caller has no independent maintenance debt to surface.
            return Ok(EventSearchBulkMaintenanceOutcome::Complete);
        }
        if guard.depth_counted && guard.depth.load(Ordering::SeqCst) != 1 {
            return Err(StoreError::InvalidBulkSearchGuard);
        }
        if !bulk_mode_pending(self)? {
            return Ok(EventSearchBulkMaintenanceOutcome::Complete);
        }
        // A bounded provider publication can intentionally span scheduler
        // passes. Keep merge suppression durable until that publication is
        // resumed and finalized; FTS maintenance is fenced while its material
        // is not yet visible.
        if self.has_pending_provider_file_publications()? {
            return Ok(EventSearchBulkMaintenanceOutcome::Pending);
        }
        for _ in 0..EVENT_SEARCH_MERGE_PASSES_PER_CALL {
            if self.finish_event_search_bulk_mode_step()? {
                return Ok(EventSearchBulkMaintenanceOutcome::Complete);
            }
        }
        self.event_search_bulk_maintenance_outcome()
    }

    pub fn event_search_bulk_maintenance_outcome(
        &self,
    ) -> Result<EventSearchBulkMaintenanceOutcome> {
        Ok(if bulk_mode_pending(self)? {
            EventSearchBulkMaintenanceOutcome::Pending
        } else {
            EventSearchBulkMaintenanceOutcome::Complete
        })
    }

    pub fn advance_event_search_bulk_maintenance(
        &self,
    ) -> Result<EventSearchBulkMaintenanceOutcome> {
        let guard = self
            .acquire_event_search_bulk_lock(self.busy_timeout)?
            .ok_or(StoreError::BulkSearchImportBusy)?;
        if !bulk_mode_pending(self)? {
            return Ok(EventSearchBulkMaintenanceOutcome::Complete);
        }
        self.finish_event_search_bulk_mode(&guard)
    }

    pub(crate) fn recover_event_search_bulk_mode(&self) -> Result<()> {
        // Check and reassert under one writer lock. A guarded importer may
        // restore settings and clear the marker while another connection is
        // waiting for this transaction, so an earlier check would be stale.
        self.begin_immediate_batch()?;
        let result = (|| {
            let pending = bulk_mode_pending(self)?;
            if pending && !self.has_pending_provider_file_publications()? {
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
        // A live importer owns this lock. Leave an unowned stale marker for an
        // import or daemon path with an installed pacer; arbitrary writable
        // opens must not opportunistically run expensive merge recovery.
        if event_search_maintenance_pacer().is_none() {
            return Ok(());
        }
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
        if self.has_pending_provider_file_publications()? {
            return Ok(());
        }
        if bulk_mode_pending(self)? {
            self.finish_event_search_bulk_mode(&guard)?;
            if bulk_mode_pending(self)? {
                return Ok(());
            }
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
        let nominal_bytes = pace_fts_maintenance(self, 1, page_budget)?;
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
        pace_observed_fts_wal(self, nominal_bytes)?;
        self.checkpoint_wal_truncate_required()?;
        Ok(changed)
    }

    /// Perform one bounded merge on both tables from the same writer snapshot.
    /// A quiescent pass is checkpointed before a second locked pass may restore
    /// settings, so a failed large-WAL checkpoint always leaves recovery marked.
    fn finish_event_search_bulk_mode_step(&self) -> Result<bool> {
        let nominal_bytes =
            pace_fts_maintenance(self, EVENT_SEARCH_FTS_TABLES.len(), FTS_MERGE_PAGE_BUDGET)?;
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
        pace_observed_fts_wal(self, nominal_bytes)?;
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
        let nominal_bytes =
            pace_fts_maintenance(self, EVENT_SEARCH_FTS_TABLES.len(), FTS_MERGE_PAGE_BUDGET)?;
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
        pace_observed_fts_wal(self, nominal_bytes)?;
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

fn pace_fts_maintenance(store: &Store, table_count: usize, page_budget: i64) -> Result<u64> {
    let Some(pacer) = event_search_maintenance_pacer() else {
        return Ok(0);
    };
    let page_size = store
        .conn
        .query_row("PRAGMA page_size", [], |row| row.get::<_, u64>(0))?;
    let logical_bytes = page_size
        .saturating_mul(page_budget.unsigned_abs())
        .saturating_mul(table_count as u64)
        .saturating_mul(FTS_MAINTENANCE_IO_PASSES);
    pacer(logical_bytes);
    Ok(logical_bytes)
}

fn pace_observed_fts_wal(store: &Store, nominal_bytes: u64) -> Result<()> {
    let Some(pacer) = event_search_maintenance_pacer() else {
        return Ok(());
    };
    if let Some(wal_bytes) = observed_wal_bytes(store)? {
        let supplement = observed_wal_supplement_bytes(nominal_bytes, wal_bytes);
        if supplement > 0 {
            pacer(supplement);
        }
    }
    Ok(())
}

fn observed_wal_supplement_bytes(nominal_bytes: u64, wal_bytes: u64) -> u64 {
    wal_bytes
        .saturating_mul(FTS_MAINTENANCE_IO_PASSES)
        .saturating_sub(nominal_bytes)
}

fn event_search_maintenance_pacer() -> Option<fn(u64)> {
    EVENT_SEARCH_MAINTENANCE_PACER.with(Cell::get)
}

fn observed_wal_bytes(store: &Store) -> Result<Option<u64>> {
    let mut path = OsString::from(store.path.as_os_str());
    path.push("-wal");
    match fs::metadata(PathBuf::from(path)) {
        Ok(metadata) => Ok(Some(metadata.len())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(StoreError::Io(err)),
    }
}

fn merge_event_search_tables_in_transaction(store: &Store) -> Result<bool> {
    let mut changed = false;
    for table in EVENT_SEARCH_FTS_TABLES {
        if table_exists(&store.conn, table)? {
            changed |= merge_fts_table_in_transaction(store, table, FTS_MERGE_PAGE_BUDGET)?;
        }
    }
    #[cfg(test)]
    if let Some(remaining) = bulk_mode_config(store, BULK_MODE_TEST_REMAINING_MERGE_PASSES_KEY)? {
        if remaining > 0 {
            // Keep fault-injected work durable so reopen tests exercise the
            // same marker/config state machine as interrupted FTS work.
            save_bulk_mode_config(
                store,
                BULK_MODE_TEST_REMAINING_MERGE_PASSES_KEY,
                remaining - 1,
            )?;
            changed = true;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const LARGE_MERGE_EVENT_COUNT: usize = 512;
    const LARGE_MERGE_PAYLOAD_WORDS: usize = 256;
    const FORCED_MERGE_PASSES: i64 = 5;
    const MAX_CONVERGENCE_CALLS: usize = 256;

    thread_local! {
        static MAINTENANCE_CHARGES: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
    }

    fn record_maintenance_bytes(bytes: u64) {
        MAINTENANCE_CHARGES.with(|charges| charges.borrow_mut().push(bytes));
    }

    fn charged_maintenance_bytes() -> u64 {
        MAINTENANCE_CHARGES.with(|charges| {
            charges
                .borrow()
                .iter()
                .copied()
                .fold(0_u64, u64::saturating_add)
        })
    }

    fn maintenance_charges() -> Vec<u64> {
        MAINTENANCE_CHARGES.with(|charges| charges.borrow().clone())
    }

    fn clear_maintenance_charges() {
        MAINTENANCE_CHARGES.with(|charges| charges.borrow_mut().clear());
    }

    fn tempdir() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("ctx-history-store-bulk-search-")
            .tempdir()
            .unwrap()
    }

    fn insert_search_events(
        store: &Store,
        token: &str,
        batch: &str,
        count: usize,
        payload_words: usize,
    ) {
        let payload = "payload ".repeat(payload_words);
        for index in 0..count {
            store
                .conn
                .execute(
                    r#"
                    INSERT INTO event_search
                    (event_id, history_record_id, session_id, role, preview_text, rank_bucket)
                    VALUES (?1, NULL, NULL, 'user', ?2, 'message')
                    "#,
                    params![
                        format!("{token}-{batch}-event-{index}"),
                        format!("{token} {index} {payload}")
                    ],
                )
                .unwrap();
        }
    }

    fn seed_large_merge_work(store: &Store, token: &str) {
        insert_search_events(
            store,
            token,
            "pending",
            LARGE_MERGE_EVENT_COUNT,
            LARGE_MERGE_PAYLOAD_WORDS,
        );
        save_bulk_mode_config(
            store,
            BULK_MODE_TEST_REMAINING_MERGE_PASSES_KEY,
            FORCED_MERGE_PASSES,
        )
        .unwrap();
    }

    fn remaining_forced_merge_passes(store: &Store) -> Option<i64> {
        bulk_mode_config(store, BULK_MODE_TEST_REMAINING_MERGE_PASSES_KEY).unwrap()
    }

    fn marker(store: &Store) -> Option<i64> {
        bulk_mode_config(store, BULK_MODE_MARKER_KEY).unwrap()
    }

    fn search_count(store: &Store, token: &str) -> i64 {
        store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM event_search WHERE event_search MATCH ?1",
                params![token],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn assert_merge_suppressed(store: &Store) {
        for table in EVENT_SEARCH_FTS_TABLES {
            assert_eq!(fts_config_value(store, table, "automerge", 4).unwrap(), 0);
            assert_eq!(
                fts_config_value(store, table, "crisismerge", 16).unwrap(),
                FTS_BULK_CRISISMERGE
            );
        }
    }

    fn assert_merge_config_restored(store: &Store) {
        for table in EVENT_SEARCH_FTS_TABLES {
            assert_eq!(
                fts_config_value(store, table, "automerge", 4).unwrap(),
                FTS_AUTOMERGE_DEFAULT
            );
            assert_eq!(
                fts_config_value(store, table, "crisismerge", 16).unwrap(),
                FTS_CRISISMERGE_DEFAULT
            );
        }
    }

    #[test]
    fn observed_wal_supplement_targets_two_passes_minus_nominal_precharge() {
        let nominal_bytes = 256;
        let wal_bytes = 1_024;
        let supplement = observed_wal_supplement_bytes(nominal_bytes, wal_bytes);

        assert_eq!(supplement, 1_792);
        assert_eq!(nominal_bytes + supplement, wal_bytes * 2);
        assert_eq!(observed_wal_supplement_bytes(2_048, wal_bytes), 0);
        assert_eq!(observed_wal_supplement_bytes(4_096, wal_bytes), 0);
    }

    #[test]
    fn finish_call_is_bounded_and_repeated_calls_converge() {
        let temp = tempdir();
        let db_path = temp.path().join("work.sqlite");
        let store = Store::open(&db_path).unwrap();
        let guard = store.begin_event_search_bulk_mode().unwrap();
        seed_large_merge_work(&store, "boundedfinish");
        let page_size = store
            .conn
            .query_row("PRAGMA page_size", [], |row| row.get::<_, u64>(0))
            .unwrap();
        clear_maintenance_charges();
        let _pacing = install_event_search_maintenance_pacer(record_maintenance_bytes);

        assert_eq!(
            store.finish_event_search_bulk_mode(&guard).unwrap(),
            EventSearchBulkMaintenanceOutcome::Pending
        );
        let nominal_step_bytes = page_size
            * FTS_MERGE_PAGE_BUDGET as u64
            * EVENT_SEARCH_FTS_TABLES.len() as u64
            * FTS_MAINTENANCE_IO_PASSES;
        let charges = maintenance_charges();
        assert_eq!(
            charges.len(),
            EVENT_SEARCH_MERGE_PASSES_PER_CALL * 2,
            "each merge step must precharge and then supplement for observed WAL"
        );
        for step_charges in charges.chunks_exact(2) {
            assert_eq!(step_charges[0], nominal_step_bytes);
            assert!(step_charges[1] > 0, "checkpoint WAL was not supplemented");
        }
        assert!(
            charges[1] > nominal_step_bytes,
            "fixture did not produce WAL amplification beyond the nominal precharge"
        );
        assert_eq!(
            observed_wal_bytes(&store).unwrap().unwrap_or_default(),
            0,
            "observed WAL must be charged before the truncating checkpoint"
        );
        assert_eq!(
            store.event_search_bulk_maintenance_outcome().unwrap(),
            EventSearchBulkMaintenanceOutcome::Pending
        );

        assert_eq!(
            marker(&store),
            Some(1),
            "one call must leave a large merge restartable"
        );
        assert_eq!(
            remaining_forced_merge_passes(&store),
            Some(FORCED_MERGE_PASSES - EVENT_SEARCH_MERGE_PASSES_PER_CALL as i64),
            "one call exceeded its merge-pass bound"
        );
        assert_merge_suppressed(&store);
        assert_eq!(
            search_count(&store, "boundedfinish"),
            LARGE_MERGE_EVENT_COUNT as i64
        );

        let mut calls = 1;
        while marker(&store).is_some() && calls < MAX_CONVERGENCE_CALLS {
            store.finish_event_search_bulk_mode(&guard).unwrap();
            calls += 1;
        }

        assert!(
            calls < MAX_CONVERGENCE_CALLS,
            "bounded finalization did not converge"
        );
        assert!(calls > 1, "fixture did not exceed one bounded call");
        assert_eq!(marker(&store), None);
        assert_eq!(
            store.event_search_bulk_maintenance_outcome().unwrap(),
            EventSearchBulkMaintenanceOutcome::Complete
        );
        assert_merge_config_restored(&store);
        assert_eq!(
            search_count(&store, "boundedfinish"),
            LARGE_MERGE_EVENT_COUNT as i64
        );
    }

    #[test]
    fn unpaced_reopen_preserves_recovery_for_a_paced_path() {
        let temp = tempdir();
        let db_path = temp.path().join("work.sqlite");
        {
            let store = Store::open(&db_path).unwrap();
            let _guard = store.begin_event_search_bulk_mode().unwrap();
            seed_large_merge_work(&store, "boundedreopen");
            assert_eq!(marker(&store), Some(1));
        }

        let unpaced_reopen = Store::open(&db_path).unwrap();
        assert_eq!(
            marker(&unpaced_reopen),
            Some(1),
            "an unpaced writable open must preserve the recovery marker"
        );
        assert_eq!(
            remaining_forced_merge_passes(&unpaced_reopen),
            Some(FORCED_MERGE_PASSES),
            "an unpaced writable open advanced expensive merge recovery"
        );
        assert_merge_suppressed(&unpaced_reopen);
        assert_eq!(
            search_count(&unpaced_reopen, "boundedreopen"),
            LARGE_MERGE_EVENT_COUNT as i64
        );
        drop(unpaced_reopen);

        clear_maintenance_charges();
        let _pacing = install_event_search_maintenance_pacer(record_maintenance_bytes);
        let first_paced_reopen = Store::open(&db_path).unwrap();
        assert_eq!(marker(&first_paced_reopen), Some(1));
        assert_eq!(
            remaining_forced_merge_passes(&first_paced_reopen),
            Some(FORCED_MERGE_PASSES - EVENT_SEARCH_MERGE_PASSES_PER_CALL as i64),
            "a paced recovery open must advance one bounded slice"
        );
        assert!(charged_maintenance_bytes() > 0);
        drop(first_paced_reopen);

        let mut reopens = 1;
        loop {
            let reopened = Store::open(&db_path).unwrap();
            reopens += 1;
            assert_eq!(
                search_count(&reopened, "boundedreopen"),
                LARGE_MERGE_EVENT_COUNT as i64
            );
            if marker(&reopened).is_none() {
                assert_merge_config_restored(&reopened);
                break;
            }
            assert_merge_suppressed(&reopened);
            assert!(
                reopens < MAX_CONVERGENCE_CALLS,
                "bounded reopen recovery did not converge"
            );
        }

        assert!(
            reopens > 1,
            "fixture did not exceed one paced recovery open"
        );
    }

    #[test]
    fn maintenance_api_reports_and_advances_durable_debt() {
        let temp = tempdir();
        let db_path = temp.path().join("work.sqlite");
        let store = Store::open(&db_path).unwrap();
        let guard = store.begin_event_search_bulk_mode().unwrap();
        seed_large_merge_work(&store, "maintenanceapi");
        assert_eq!(
            store.finish_event_search_bulk_mode(&guard).unwrap(),
            EventSearchBulkMaintenanceOutcome::Pending
        );
        drop(guard);

        let before = remaining_forced_merge_passes(&store).unwrap();
        let outcome = store.advance_event_search_bulk_maintenance().unwrap();
        let after = remaining_forced_merge_passes(&store).unwrap_or_default();

        assert!(after < before);
        assert_eq!(outcome, EventSearchBulkMaintenanceOutcome::Pending);
    }
}
