use std::{
    fs,
    path::Path,
    thread,
    time::{Duration, Instant},
};

use ctx_history_core::HistoryRecord;
use fs2::FileExt;
use rusqlite::{types::ValueRef, Connection};
use same_file::Handle;
use sha2::{Digest, Sha256};

use super::{
    adjacent_retired_path, link_absent, link_count, query_count, quoted_identifier, FTS_TABLES,
};
use crate::{
    connection::{lock_is_contended, open_publication_lease, BUSY_TIMEOUT},
    Result, Store, StoreError,
};

/// How long publication waits for every writable Store to be released.
///
/// The lease is only ever held for the milliseconds an install takes, so a wait
/// this long means a writer is actively holding the destination open and the
/// rebuild fails closed rather than publishing over it.
const PUBLICATION_LEASE_WAIT: Duration = Duration::from_secs(5);
const PUBLICATION_LEASE_POLL: Duration = Duration::from_millis(20);

/// Exclusive publication lease over a Store path.
///
/// Held across the emptiness proof, the retirement and the install. Every
/// writable `Store::open` takes the same lease shared for the lifetime of the
/// Store, so while this is held no writable Store exists and no commit can
/// reach the destination.
pub(super) struct PublicationLease {
    file: fs::File,
}

impl Drop for PublicationLease {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

/// Takes the publication lease exclusively, waiting a bounded time.
pub(super) fn acquire_publication_lease(path: &Path, wait: Duration) -> Result<PublicationLease> {
    let file = open_publication_lease(path)?;
    let deadline = Instant::now() + wait;
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(PublicationLease { file }),
            Err(error) if lock_is_contended(&error) => {
                if Instant::now() >= deadline {
                    return Err(StoreError::ColdStoreBuildBusy(path.to_path_buf()));
                }
                thread::sleep(PUBLICATION_LEASE_POLL);
            }
            Err(error) => return Err(error.into()),
        }
    }
}

// Test-only override so the lease-contention path can be exercised without
// waiting out the production timeout.
#[cfg(test)]
std::thread_local! {
    static TEST_PUBLICATION_LEASE_WAIT: std::cell::Cell<Option<Duration>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(super) fn set_publication_lease_wait(wait: Duration) {
    TEST_PUBLICATION_LEASE_WAIT.with(|slot| slot.set(Some(wait)));
}

pub(super) fn publication_lease_wait() -> Duration {
    #[cfg(test)]
    if let Some(wait) = TEST_PUBLICATION_LEASE_WAIT.with(std::cell::Cell::get) {
        return wait;
    }
    PUBLICATION_LEASE_WAIT
}

/// Upper bound on the control records an empty generation may carry forward.
///
/// A generation that owns more history than this is not a bootstrap state, so
/// it stays on the ordinary incremental writer instead of being rebuilt.
pub(super) const MAX_REBUILDABLE_HISTORY_RECORDS: usize = 4096;

/// Tables a pristine migrated Store legitimately owns rows in.
///
/// Each is singleton or statistical control state that the rebuilt generation
/// recreates for itself. None of them carry canonical provider content.
const CONTROL_TABLES: [&str; 4] = [
    "canonical_semantic_projection_state",
    "ctx_store_schema_identity",
    "projection_journal_state",
    "search_projection_stats",
];

/// Content tables whose rows are carried into the rebuilt generation.
const CARRIED_TABLES: [&str; 1] = ["history_records"];

/// FTS5 shadow suffixes. Every search table is a projection of a canonical
/// table, so requiring the canonical tables to be empty already bounds them.
const FTS_SHADOW_SUFFIXES: [&str; 5] = ["_config", "_content", "_data", "_docsize", "_idx"];

/// An existing target admitted for whole-generation replacement.
pub(super) struct EmptyGenerationAdmission {
    pub(super) identity: Handle,
    pub(super) records: Vec<HistoryRecord>,
    /// Digest over every column of every carried row, not just their ids: an
    /// update that rewrites a record's contents leaves the id set identical and
    /// must still invalidate the admission.
    pub(super) records_digest: [u8; 32],
}

/// Admits an existing regular target only when it is an empty generation.
///
/// The Store is opened normally first: a released v0.25 database does not yet
/// own every current projection table, so emptiness is only meaningful after
/// the ordinary migration has run. Any target the ordinary writer would need to
/// diagnose stays on the ordinary writer.
pub(super) fn admit_empty_generation(
    target_path: &Path,
) -> Result<Option<EmptyGenerationAdmission>> {
    admit_empty_generation_within(target_path, MAX_REBUILDABLE_HISTORY_RECORDS)
}

pub(super) fn admit_empty_generation_within(
    target_path: &Path,
    max_records: usize,
) -> Result<Option<EmptyGenerationAdmission>> {
    // Admission runs under the same exclusive lease publication takes, so a live
    // writable Store declines the rebuild here instead of failing a completed
    // build later. The lease is released again immediately: holding it for the
    // whole multi-second build would block every ordinary writer.
    let lease = match acquire_publication_lease(target_path, Duration::ZERO) {
        Ok(lease) => lease,
        Err(StoreError::ColdStoreBuildBusy(_)) => return Ok(None),
        Err(error) => return Err(error),
    };
    // The migrating open must not take the shared lease: this thread already
    // owns the same lock file exclusively.
    let Ok(store) = Store::open_without_publication_lease(target_path.to_path_buf(), BUSY_TIMEOUT)
    else {
        return Ok(None);
    };
    if !empty_generation(&store.conn)? {
        return Ok(None);
    }
    if query_count(&store.conn, "SELECT COUNT(*) FROM history_records")? > max_records {
        return Ok(None);
    }
    let records_digest = history_records_digest(&store.conn)?;
    let records = store.list_records(max_records)?;
    drop(store);

    let identity = Handle::from_path(target_path)
        .map_err(|_| StoreError::ColdStoreTargetChanged(target_path.to_path_buf()))?;
    // Publication has to make the destination name absent. Prove that here, in
    // microseconds, rather than discovering after a full build that another
    // process holds the file open. Windows refuses to unlink a database another
    // handle already owns, so that install stays on the ordinary writer.
    if !prove_target_retirable(target_path, &identity)? {
        return Ok(None);
    }
    drop(lease);
    Ok(Some(EmptyGenerationAdmission {
        identity,
        records,
        records_digest,
    }))
}

/// Proves the destination name can be retired and restored.
///
/// The probe performs the exact publication sequence against a second link the
/// lock owner minted, and puts the admitted object straight back. It fails
/// closed: a concurrent winner keeps the name, and a build interrupted inside
/// the probe leaves the same recoverable retired name a real install would.
fn prove_target_retirable(target_path: &Path, identity: &Handle) -> Result<bool> {
    let probe_path = adjacent_retired_path(target_path);
    if link_absent(target_path, &probe_path).is_err() {
        let _ = fs::remove_file(&probe_path);
        return Ok(false);
    }
    let linked = Handle::from_path(&probe_path)
        .map(|current| current == *identity)
        .unwrap_or(false)
        && link_count(&probe_path).is_ok_and(|links| !links.is_some_and(|actual| actual != 2));
    if !linked || fs::remove_file(target_path).is_err() {
        let _ = fs::remove_file(&probe_path);
        return Ok(false);
    }
    link_absent(&probe_path, target_path)?;
    let restored = Handle::from_path(target_path)
        .map(|current| current == *identity)
        .unwrap_or(false);
    let _ = fs::remove_file(&probe_path);
    if !restored {
        return Err(StoreError::ColdStoreTargetChanged(
            target_path.to_path_buf(),
        ));
    }
    Ok(true)
}

/// Re-proves emptiness immediately before publication.
///
/// The caller must already hold the publication lease exclusively and must keep
/// holding it through the install. That lease — not this function's transaction
/// — is what makes the proof hold: every writable `Store::open` takes the same
/// lease shared for the lifetime of the Store, so no writable Store exists
/// between this proof and the install, and no commit can reach the destination
/// in that window. `BEGIN IMMEDIATE` here only guarantees the proof itself is
/// not taken against a half-applied transaction.
pub(super) fn revalidate_empty_generation(
    target_path: &Path,
    records_digest: &[u8; 32],
) -> Result<()> {
    let changed = || StoreError::ColdStoreTargetChanged(target_path.to_path_buf());
    let conn = Connection::open(target_path).map_err(|_| changed())?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.execute_batch("PRAGMA foreign_keys = ON; BEGIN IMMEDIATE")?;
    let observed = (|| -> Result<bool> {
        Ok(empty_generation(&conn)? && &history_records_digest(&conn)? == records_digest)
    })();
    let _ = conn.execute_batch("ROLLBACK");
    drop(conn);
    if !observed? {
        return Err(changed());
    }
    Ok(())
}

/// Returns whether every table outside the documented control, carried, and
/// search-projection sets is empty.
///
/// This is strictly stronger than [`Store::fresh_provider_projection_eligible`]:
/// that predicate admits catalog, artifact, and summary rows because it only
/// ever runs against a stage this builder owns. Replacing a destination the
/// user already owns requires proving the whole generation is empty, and an
/// unrecognized table with rows fails closed.
pub(super) fn empty_generation(conn: &Connection) -> Result<bool> {
    let mut statement = conn.prepare(
        r"SELECT name FROM sqlite_master
          WHERE type = 'table' AND name NOT LIKE 'sqlite\_%' ESCAPE '\'
          ORDER BY name",
    )?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(statement);
    for name in names {
        if is_rebuildable_table(&name) {
            continue;
        }
        let occupied: bool = conn.query_row(
            &format!(
                "SELECT EXISTS(SELECT 1 FROM {} LIMIT 1)",
                quoted_identifier(&name)
            ),
            [],
            |row| row.get(0),
        )?;
        if occupied {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Digests every column of every carried row in a stable order.
///
/// Comparing id sets would miss an update that rewrites a record's contents in
/// place, which publication would then discard. Every column is hashed, so any
/// insert, delete or update invalidates the admission.
fn history_records_digest(conn: &Connection) -> Result<[u8; 32]> {
    let mut statement = conn.prepare("SELECT * FROM history_records ORDER BY id")?;
    let column_count = statement.column_count();
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-history-records-v1");
    hasher.update((column_count as u64).to_le_bytes());
    for name in statement.column_names() {
        hasher.update((name.len() as u64).to_le_bytes());
        hasher.update(name.as_bytes());
    }
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        for column in 0..column_count {
            match row.get_ref(column)? {
                ValueRef::Null => hasher.update([0_u8]),
                ValueRef::Integer(value) => {
                    hasher.update([1_u8]);
                    hasher.update(value.to_le_bytes());
                }
                ValueRef::Real(value) => {
                    hasher.update([2_u8]);
                    hasher.update(value.to_le_bytes());
                }
                ValueRef::Text(value) => {
                    hasher.update([3_u8]);
                    hasher.update((value.len() as u64).to_le_bytes());
                    hasher.update(value);
                }
                ValueRef::Blob(value) => {
                    hasher.update([4_u8]);
                    hasher.update((value.len() as u64).to_le_bytes());
                    hasher.update(value);
                }
            }
        }
    }
    Ok(hasher.finalize().into())
}

fn is_rebuildable_table(name: &str) -> bool {
    if CONTROL_TABLES.contains(&name) || CARRIED_TABLES.contains(&name) {
        return true;
    }
    FTS_TABLES.iter().any(|table| {
        name == *table
            || FTS_SHADOW_SUFFIXES
                .iter()
                .any(|suffix| name == format!("{table}{suffix}"))
    })
}

#[cfg(test)]
#[path = "target_tests.rs"]
mod tests;
