use super::*;

pub(in crate::provider_sources::sqlite_source) fn open_root_handle_sqlite_source_snapshot_for_test(
    authority: &SqliteSourceDirectoryAuthority,
    database_name: &OsStr,
    before_sqlite_open: impl FnOnce(),
) -> SqliteSourceAccessResult<SqliteSourceReadSnapshot> {
    open_root_handle_sqlite_source_snapshot_inner(
        authority,
        database_name,
        SqliteSourceSnapshotPolicy::StrictPhysicalFamily,
        || {},
        || {},
        before_sqlite_open,
    )
}

pub(in crate::provider_sources::sqlite_source) fn open_root_handle_sqlite_source_snapshot_after_database_copy_for_test(
    authority: &SqliteSourceDirectoryAuthority,
    database_name: &OsStr,
    after_database_copy: impl FnOnce(),
) -> SqliteSourceAccessResult<SqliteSourceReadSnapshot> {
    open_root_handle_sqlite_source_snapshot_inner(
        authority,
        database_name,
        SqliteSourceSnapshotPolicy::StrictPhysicalFamily,
        || {},
        after_database_copy,
        || {},
    )
}

pub(in crate::provider_sources::sqlite_source) fn open_root_handle_sqlite_source_snapshot_after_parent_certification_for_test(
    authority: &SqliteSourceDirectoryAuthority,
    database_name: &OsStr,
    after_parent_certification: impl FnOnce(),
) -> SqliteSourceAccessResult<SqliteSourceReadSnapshot> {
    open_root_handle_sqlite_source_snapshot_inner(
        authority,
        database_name,
        SqliteSourceSnapshotPolicy::StrictPhysicalFamily,
        after_parent_certification,
        || {},
        || {},
    )
}

pub(in crate::provider_sources::sqlite_source) fn open_root_handle_sqlite_source_online_backup_before_identity_check_for_test(
    authority: &SqliteSourceDirectoryAuthority,
    database_name: &OsStr,
    after_source_pin: impl FnOnce(),
) -> SqliteSourceAccessResult<SqliteSourceReadSnapshot> {
    open_root_handle_sqlite_source_snapshot_inner(
        authority,
        database_name,
        SqliteSourceSnapshotPolicy::LogicalOnlineBackup,
        || {},
        || {},
        after_source_pin,
    )
}

pub(in crate::provider_sources::sqlite_source) fn open_root_handle_sqlite_source_online_backup_after_database_copy_for_test(
    authority: &SqliteSourceDirectoryAuthority,
    database_name: &OsStr,
    after_database_copy: impl FnOnce(),
) -> SqliteSourceAccessResult<SqliteSourceReadSnapshot> {
    open_root_handle_sqlite_source_snapshot_inner(
        authority,
        database_name,
        SqliteSourceSnapshotPolicy::LogicalOnlineBackup,
        || {},
        after_database_copy,
        || {},
    )
}

pub(in crate::provider_sources::sqlite_source) fn open_root_handle_sqlite_source_online_backup_after_backup_for_test(
    authority: &SqliteSourceDirectoryAuthority,
    database_name: &OsStr,
    after_online_backup: impl FnOnce(&Path),
) -> SqliteSourceAccessResult<SqliteSourceReadSnapshot> {
    let family = SqliteSourceFamily::open(authority, database_name, || {})?;
    match open_logical_online_backup_snapshot_with_progress(
        authority,
        family,
        LogicalSnapshotHooks {
            after_database_copy: || {},
            after_private_source_copy: ignore_snapshot_path,
            before_source_revalidation: || {},
            after_online_backup,
        },
        SQLITE_SNAPSHOT_MAX_TOTAL_BYTES,
        &mut |_| Ok::<(), std::convert::Infallible>(()),
    ) {
        Ok(snapshot) => Ok(snapshot),
        Err(SqliteSourceProgressError::Source(error)) => Err(error),
        Err(SqliteSourceProgressError::Progress(never)) => match never {},
    }
}

pub(crate) fn open_root_handle_sqlite_source_online_backup_after_private_source_copy_for_test(
    authority: &SqliteSourceDirectoryAuthority,
    database_name: &OsStr,
    after_private_source_copy: impl FnOnce(&Path),
) -> SqliteSourceAccessResult<SqliteSourceReadSnapshot> {
    let family = SqliteSourceFamily::open(authority, database_name, || {})?;
    match open_logical_online_backup_snapshot_with_progress(
        authority,
        family,
        LogicalSnapshotHooks {
            after_database_copy: || {},
            after_private_source_copy,
            before_source_revalidation: || {},
            after_online_backup: ignore_snapshot_path,
        },
        SQLITE_SNAPSHOT_MAX_TOTAL_BYTES,
        &mut |_| Ok::<(), std::convert::Infallible>(()),
    ) {
        Ok(snapshot) => Ok(snapshot),
        Err(SqliteSourceProgressError::Source(error)) => Err(error),
        Err(SqliteSourceProgressError::Progress(never)) => match never {},
    }
}

pub(in crate::provider_sources::sqlite_source) fn run_online_backup_with_deadline_for_test(
    source: &Connection,
    destination: &Connection,
    deadline: Instant,
) -> SqliteSourceAccessResult<()> {
    super::backup_handle::run_online_backup_until(source, destination, deadline)
}

pub(in crate::provider_sources::sqlite_source) fn open_root_handle_sqlite_source_online_backup_with_scratch_limit_for_test(
    authority: &SqliteSourceDirectoryAuthority,
    database_name: &OsStr,
    scratch_limit: u64,
) -> SqliteSourceAccessResult<SqliteSourceReadSnapshot> {
    let family = SqliteSourceFamily::open(authority, database_name, || {})?;
    open_logical_online_backup_snapshot(authority, family, || {}, || {}, scratch_limit)
}

pub(in crate::provider_sources::sqlite_source) fn certify_root_handle_sqlite_source_snapshot_copy_budget_for_test(
    authority: &SqliteSourceDirectoryAuthority,
    database_name: &OsStr,
) -> SqliteSourceAccessResult<u64> {
    let family = SqliteSourceFamily::open(authority, database_name, || {})?;
    let evidence = family.capture_evidence()?;
    let copied_bytes = enforce_snapshot_copy_bounds(&family, &evidence)?;
    family.revalidate(&evidence)?;
    Ok(copied_bytes)
}
