use super::*;

fn open_with_hooks(
    authority: &SqliteSourceDirectoryAuthority,
    database_name: &OsStr,
    policy: SqliteSourceSnapshotPolicy,
    limits: SqliteSourceSnapshotLimits,
    after_parent_retention: impl FnOnce(),
    after_database_copy: impl FnOnce(),
    before_source_revalidation: impl FnOnce(),
) -> SqliteSourceAccessResult<SqliteSourceReadSnapshot> {
    match open_root_handle_sqlite_source_snapshot_with_progress_and_hooks(
        authority,
        database_name,
        SqliteSourceSnapshotOptions::new(policy, limits),
        after_parent_retention,
        after_database_copy,
        before_source_revalidation,
        &mut |_| Ok::<(), std::convert::Infallible>(()),
    ) {
        Ok(snapshot) => Ok(snapshot),
        Err(SqliteSourceProgressError::Source(error)) => Err(error),
        Err(SqliteSourceProgressError::Progress(never)) => match never {},
        Err(SqliteSourceProgressError::ProgressAndFinalization { primary, .. }) => match primary {},
    }
}

#[cfg(unix)]
pub(in crate::sqlite_source) fn open_root_handle_sqlite_source_snapshot_before_revalidation_for_test(
    authority: &SqliteSourceDirectoryAuthority,
    database_name: &OsStr,
    before_source_revalidation: impl FnOnce(),
) -> SqliteSourceAccessResult<SqliteSourceReadSnapshot> {
    open_with_hooks(
        authority,
        database_name,
        SqliteSourceSnapshotPolicy::ExactRevision,
        SqliteSourceSnapshotLimits::default(),
        || {},
        || {},
        before_source_revalidation,
    )
}

pub(in crate::sqlite_source) fn open_root_handle_sqlite_source_stable_snapshot_before_revalidation_for_test(
    authority: &SqliteSourceDirectoryAuthority,
    database_name: &OsStr,
    before_source_revalidation: impl FnOnce(),
) -> SqliteSourceAccessResult<SqliteSourceReadSnapshot> {
    open_with_hooks(
        authority,
        database_name,
        SqliteSourceSnapshotPolicy::StablePrivateCopy,
        SqliteSourceSnapshotLimits::default(),
        || {},
        || {},
        before_source_revalidation,
    )
}

pub(in crate::sqlite_source) fn open_root_handle_sqlite_source_snapshot_with_limit_for_test(
    authority: &SqliteSourceDirectoryAuthority,
    database_name: &OsStr,
    maximum_scratch_bytes: u64,
) -> SqliteSourceAccessResult<SqliteSourceReadSnapshot> {
    open_with_hooks(
        authority,
        database_name,
        SqliteSourceSnapshotPolicy::StablePrivateCopy,
        SqliteSourceSnapshotLimits::new(maximum_scratch_bytes),
        || {},
        || {},
        || {},
    )
}

#[cfg(target_os = "linux")]
pub(in crate::sqlite_source) fn planned_snapshot_copy_bytes_for_test(
    authority: &SqliteSourceDirectoryAuthority,
    database_name: &OsStr,
) -> SqliteSourceAccessResult<u64> {
    let family = SqliteSourceFamily::open(authority, database_name, || {})?;
    let evidence = family.capture_evidence()?;
    enforce_snapshot_copy_bounds_with_limit(
        &family,
        &evidence,
        SqliteSourceSnapshotLimits::default().maximum_source_bytes(),
    )
}

pub(in crate::sqlite_source) fn open_root_handle_sqlite_source_stable_snapshot_after_database_copy_for_test(
    authority: &SqliteSourceDirectoryAuthority,
    database_name: &OsStr,
    after_database_copy: impl FnOnce(),
) -> SqliteSourceAccessResult<SqliteSourceReadSnapshot> {
    open_with_hooks(
        authority,
        database_name,
        SqliteSourceSnapshotPolicy::StablePrivateCopy,
        SqliteSourceSnapshotLimits::default(),
        || {},
        after_database_copy,
        || {},
    )
}
