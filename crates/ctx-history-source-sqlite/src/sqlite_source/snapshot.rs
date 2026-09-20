use super::*;

pub(super) mod acquisition;
mod copy_progress;
mod scratch;
pub(super) mod selective;
#[cfg(any(test, feature = "test-support"))]
mod test_api;

use acquisition::*;
pub(super) use acquisition::{close_private_snapshot_directory, close_private_sqlite_connection};
#[cfg(any(test, feature = "test-support"))]
pub use acquisition::{
    fail_next_opened_snapshot_cleanup_for_test, fail_next_snapshot_open_for_test,
    fail_next_snapshot_write_enospc_for_test, force_next_pinned_wal_unavailable_for_test,
};
use copy_progress::{copy_sqlite_member_with_progress, report_source_family_copy_progress};
#[cfg(any(test, feature = "test-support"))]
pub(in crate::sqlite_source) use scratch::{
    fail_next_private_scratch_close_for_test, fail_next_private_scratch_open_for_test,
};

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static FAIL_NEXT_PRIVATE_DIRECTORY_CLEANUP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(any(test, feature = "test-support"))]
pub fn fail_next_private_directory_cleanup_for_test() {
    FAIL_NEXT_PRIVATE_DIRECTORY_CLEANUP.with(|fail| fail.set(true));
}

#[cfg(any(test, feature = "test-support"))]
fn take_private_directory_cleanup_failure_for_test() -> bool {
    FAIL_NEXT_PRIVATE_DIRECTORY_CLEANUP.with(|fail| fail.replace(false))
}

/// How one authorized provider SQLite leaf is stabilized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SqliteSourceSnapshotPolicy {
    ExactRevision,
    PinnedReadOnlyWal,
    StablePrivateCopy,
    SelectivePrivateCopy(&'static [&'static str]),
}

/// Snapshot-wide scratch policy. The aggregate includes every retained DB/WAL
/// file and any SQLite-created transient private artifact for this route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SqliteSourceSnapshotLimits {
    policy: SqliteSnapshotCapacityPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SqliteSnapshotCapacityPolicy {
    AvailableDisk,
    FixedAggregate(u64),
    SourceOnly(u64),
}

impl SqliteSourceSnapshotLimits {
    pub const fn new(maximum_scratch_bytes: u64) -> Self {
        Self {
            policy: SqliteSnapshotCapacityPolicy::FixedAggregate(maximum_scratch_bytes),
        }
    }

    /// Exposes only the no-write immutable path. Platforms without a retained
    /// immutable-handle VFS fail closed before creating scratch files.
    pub const fn without_scratch(maximum_source_bytes: u64) -> Self {
        Self {
            policy: SqliteSnapshotCapacityPolicy::SourceOnly(maximum_source_bytes),
        }
    }

    /// Returns the explicit aggregate bound, or `u64::MAX` for production's
    /// available-disk policy. Retained for API compatibility; acquisition uses
    /// the typed policy rather than treating this value as a production cap.
    pub const fn maximum_scratch_bytes(self) -> u64 {
        match self.maximum_scratch_limit() {
            Some(maximum) => maximum,
            None => u64::MAX,
        }
    }

    const fn maximum_scratch_limit(self) -> Option<u64> {
        match self.policy {
            SqliteSnapshotCapacityPolicy::AvailableDisk => None,
            SqliteSnapshotCapacityPolicy::FixedAggregate(maximum) => Some(maximum),
            SqliteSnapshotCapacityPolicy::SourceOnly(_) => Some(0),
        }
    }

    const fn maximum_source_bytes(self) -> Option<u64> {
        match self.policy {
            SqliteSnapshotCapacityPolicy::AvailableDisk => None,
            SqliteSnapshotCapacityPolicy::FixedAggregate(maximum)
            | SqliteSnapshotCapacityPolicy::SourceOnly(maximum) => Some(maximum),
        }
    }
}

impl Default for SqliteSourceSnapshotLimits {
    fn default() -> Self {
        Self {
            policy: SqliteSnapshotCapacityPolicy::AvailableDisk,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SqliteSourceSnapshotOptions {
    policy: SqliteSourceSnapshotPolicy,
    limits: SqliteSourceSnapshotLimits,
}

impl SqliteSourceSnapshotOptions {
    const fn new(policy: SqliteSourceSnapshotPolicy, limits: SqliteSourceSnapshotLimits) -> Self {
        Self { policy, limits }
    }
}

/// Retains an approved parent-directory handle together with the pathname that
/// stock SQLite is allowed to open beneath it.
pub fn retain_sqlite_source_directory_authority(
    data_root: &Path,
    authorized_parent: &File,
    approved_parent_path: &Path,
) -> SqliteSourceAccessResult<SqliteSourceDirectoryAuthority> {
    SqliteSourceDirectoryAuthority::retain(data_root, authorized_parent, approved_parent_path)
}

/// Opens one approved SQLite leaf through stock rusqlite/SQLite behavior.
pub fn open_root_handle_sqlite_source_snapshot(
    authority: &SqliteSourceDirectoryAuthority,
    database_name: &OsStr,
) -> SqliteSourceAccessResult<SqliteSourceReadSnapshot> {
    open_root_handle_sqlite_source_snapshot_with_limits(
        authority,
        database_name,
        SqliteSourceSnapshotLimits::default(),
    )
}

pub fn open_root_handle_sqlite_source_snapshot_with_limits(
    authority: &SqliteSourceDirectoryAuthority,
    database_name: &OsStr,
    limits: SqliteSourceSnapshotLimits,
) -> SqliteSourceAccessResult<SqliteSourceReadSnapshot> {
    open_root_handle_sqlite_source_snapshot_with_policy(
        authority,
        database_name,
        SqliteSourceSnapshotPolicy::ExactRevision,
        limits,
    )
}

pub(super) fn open_root_handle_sqlite_source_snapshot_with_policy(
    authority: &SqliteSourceDirectoryAuthority,
    database_name: &OsStr,
    policy: SqliteSourceSnapshotPolicy,
    limits: SqliteSourceSnapshotLimits,
) -> SqliteSourceAccessResult<SqliteSourceReadSnapshot> {
    match open_root_handle_sqlite_source_snapshot_with_progress_and_hooks(
        authority,
        database_name,
        SqliteSourceSnapshotOptions::new(policy, limits),
        || {},
        || {},
        || {},
        &mut |_| Ok::<(), std::convert::Infallible>(()),
    ) {
        Ok(snapshot) => Ok(snapshot),
        Err(SqliteSourceProgressError::Source(error)) => Err(error),
        Err(SqliteSourceProgressError::Progress(never)) => match never {},
        Err(SqliteSourceProgressError::ProgressAndFinalization { primary, .. }) => match primary {},
    }
}

pub(super) fn open_root_handle_sqlite_source_snapshot_with_progress<E>(
    authority: &SqliteSourceDirectoryAuthority,
    database_name: &OsStr,
    policy: SqliteSourceSnapshotPolicy,
    limits: SqliteSourceSnapshotLimits,
    report_progress: &mut impl FnMut(SqliteSourceProgress) -> Result<(), E>,
) -> Result<SqliteSourceReadSnapshot, SqliteSourceProgressError<E>> {
    open_root_handle_sqlite_source_snapshot_with_progress_and_hooks(
        authority,
        database_name,
        SqliteSourceSnapshotOptions::new(policy, limits),
        || {},
        || {},
        || {},
        report_progress,
    )
}

fn open_root_handle_sqlite_source_snapshot_with_progress_and_hooks<E>(
    authority: &SqliteSourceDirectoryAuthority,
    database_name: &OsStr,
    options: SqliteSourceSnapshotOptions,
    after_parent_retention: impl FnOnce(),
    after_database_copy: impl FnOnce(),
    before_source_revalidation: impl FnOnce(),
    report_progress: &mut impl FnMut(SqliteSourceProgress) -> Result<(), E>,
) -> Result<SqliteSourceReadSnapshot, SqliteSourceProgressError<E>> {
    let family = SqliteSourceFamily::open(authority, database_name, after_parent_retention)
        .map_err(|error| {
            let artifact = error.acquisition_artifact();
            SqliteSourceProgressError::Source(error.with_diagnostic(
                SqliteFailurePhase::SourceAcquisition,
                artifact,
                0,
                0,
                SqliteCleanupStatus::NotRequired,
            ))
        })?;
    let native_evidence = match options.policy {
        SqliteSourceSnapshotPolicy::PinnedReadOnlyWal
        | SqliteSourceSnapshotPolicy::SelectivePrivateCopy(_) => {
            family.capture_revision_evidence()?
        }
        SqliteSourceSnapshotPolicy::ExactRevision
        | SqliteSourceSnapshotPolicy::StablePrivateCopy => family.capture_evidence()?,
    };
    let mut acquired = acquire_sqlite_connection_with_progress(
        &authority.snapshot_context,
        &family,
        &native_evidence,
        options,
        after_database_copy,
        report_progress,
    )?;

    let validation: SqliteSourceAccessResult<SqliteSnapshotEvidence> = (|| {
        verify_connection_read_only(&acquired.connection)?;
        configure_and_pin_snapshot(&acquired.connection)?;
        before_source_revalidation();

        // The copy and SQLite view become authoritative together. Revalidate
        // the exact source family on both sides of the bounded connection
        // evidence read so a concurrent write cannot escape acquisition.
        match options.policy {
            SqliteSourceSnapshotPolicy::PinnedReadOnlyWal
            | SqliteSourceSnapshotPolicy::SelectivePrivateCopy(_) => {
                family.revalidate_database_identity(&native_evidence)?
            }
            SqliteSourceSnapshotPolicy::ExactRevision
            | SqliteSourceSnapshotPolicy::StablePrivateCopy => {
                family.revalidate(&native_evidence)?
            }
        }
        let sqlite_evidence = capture_sqlite_evidence(&acquired.connection)?;
        match options.policy {
            SqliteSourceSnapshotPolicy::PinnedReadOnlyWal
            | SqliteSourceSnapshotPolicy::SelectivePrivateCopy(_) => {
                family.revalidate_database_identity(&native_evidence)?
            }
            SqliteSourceSnapshotPolicy::ExactRevision
            | SqliteSourceSnapshotPolicy::StablePrivateCopy => {
                family.revalidate(&native_evidence)?
            }
        }
        acquired.finalize_accounting(&authority.snapshot_context)?;
        Ok(sqlite_evidence)
    })();
    let sqlite_evidence = match validation {
        Ok(evidence) => evidence,
        Err(error) => {
            let error = acquired.diagnose_validation_error(error);
            return match acquired.cleanup() {
                Ok(()) => Err(error
                    .with_cleanup_status(SqliteCleanupStatus::Succeeded)
                    .into()),
                Err(cleanup) => Err(SqliteSourceAccessError::Finalization {
                    primary: Box::new(error),
                    cleanup: Box::new(cleanup),
                }
                .into()),
            };
        }
    };

    // A transaction may pin later than the initial physical observation. Only
    // advertise exact replay if that physical revision survived the capture.
    let replay_safe = match options.policy {
        SqliteSourceSnapshotPolicy::PinnedReadOnlyWal => false,
        SqliteSourceSnapshotPolicy::SelectivePrivateCopy(_) => {
            family.revalidate_revision(&native_evidence).is_ok()
        }
        _ => true,
    };
    let policy = match options.policy {
        SqliteSourceSnapshotPolicy::SelectivePrivateCopy(_) => {
            SqliteSourceSnapshotPolicy::StablePrivateCopy
        }
        policy => policy,
    };
    let evidence = SqliteSourceEvidence::from_snapshot(&native_evidence, &sqlite_evidence);
    let AcquiredSqliteConnection {
        connection,
        strategy,
        copied_bytes,
        snapshot_directory,
        live_authority_handle,
        snapshot_activity,
        scratch,
    } = acquired;
    Ok(SqliteSourceReadSnapshot {
        connection: Some(connection),
        family: Some(family),
        native_evidence,
        sqlite_evidence,
        evidence,
        policy,
        admitted_revision_is_replay_safe: replay_safe,
        strategy,
        copied_bytes,
        _snapshot_directory: snapshot_directory,
        _live_authority_handle: live_authority_handle,
        _scratch: scratch,
        snapshot_activity,
        snapshot_context: Arc::clone(&authority.snapshot_context),
        terminal_fence_slot: Arc::default(),
        explicitly_completed: false,
        #[cfg(any(test, feature = "test-support"))]
        fail_next_cleanup: take_opened_snapshot_cleanup_failure_for_test(),
    })
}

#[cfg(any(test, feature = "test-support"))]
pub(super) use test_api::{
    open_root_handle_sqlite_source_snapshot_before_revalidation_for_test,
    open_root_handle_sqlite_source_snapshot_with_limit_for_test,
    open_root_handle_sqlite_source_stable_snapshot_after_database_copy_for_test,
    open_root_handle_sqlite_source_stable_snapshot_before_revalidation_for_test,
    planned_snapshot_copy_bytes_for_test,
};
