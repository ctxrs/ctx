use super::*;

use std::{
    thread,
    time::{Duration, Instant},
};

// The admitted source is size-bounded, but a 2 GiB backup can legitimately
// take longer than the connection's five-second lock timeout on a busy disk.
// Keep an outer fail-safe without making normal large local histories fail.
const SQLITE_ONLINE_BACKUP_DEADLINE: Duration = Duration::from_secs(5 * 60);
const SQLITE_ONLINE_BACKUP_PAGES_PER_STEP: i32 = 256;
const SQLITE_CERTIFICATION_DEADLINE: Duration = Duration::from_secs(5 * 60);
const SQLITE_CERTIFICATION_PROGRESS_OPS: i32 = 4_096;
const SQLITE_SOURCE_TRANSITION_ATTEMPTS: usize = 2;

struct LogicalSnapshotHooks<
    AfterDatabaseCopy,
    AfterPrivateSourceCopy,
    BeforeSourceRevalidation,
    AfterOnlineBackup,
> {
    after_database_copy: AfterDatabaseCopy,
    after_private_source_copy: AfterPrivateSourceCopy,
    before_source_revalidation: BeforeSourceRevalidation,
    after_online_backup: AfterOnlineBackup,
}

fn ignore_snapshot_path(_: &Path) {}
mod acquisition;
mod backup_handle;
mod certification;
mod copy_progress;
mod scratch;
mod source_copy;
#[cfg(test)]
mod test_api;
#[cfg(test)]
pub(crate) use test_api::open_root_handle_sqlite_source_online_backup_after_private_source_copy_for_test;
#[cfg(test)]
pub(super) use test_api::{
    certify_root_handle_sqlite_source_snapshot_copy_budget_for_test,
    online_backup_contention_deadline_error_for_test,
    open_root_handle_sqlite_source_online_backup_after_backup_for_test,
    open_root_handle_sqlite_source_online_backup_after_database_copy_for_test,
    open_root_handle_sqlite_source_online_backup_before_identity_check_for_test,
    open_root_handle_sqlite_source_online_backup_with_scratch_limit_for_test,
    open_root_handle_sqlite_source_snapshot_after_database_copy_for_test,
    open_root_handle_sqlite_source_snapshot_after_parent_certification_for_test,
    open_root_handle_sqlite_source_snapshot_for_test, retained_online_backup_retry_code_for_test,
    run_online_backup_with_deadline_for_test,
};

#[cfg(test)]
pub(crate) use acquisition::fail_next_opened_snapshot_cleanup_for_test;
pub(super) use acquisition::{close_private_snapshot_directory, close_private_sqlite_connection};
use copy_progress::{copy_sqlite_member_with_progress, report_source_family_copy_progress};
use {acquisition::*, backup_handle::*, certification::*, source_copy::*};

/// Retains an approved parent-directory handle together with the pathname that
/// stock SQLite is allowed to open beneath it.
pub(crate) fn retain_sqlite_source_directory_authority(
    data_root: &Path,
    authorized_parent: &File,
    approved_parent_path: &Path,
) -> SqliteSourceAccessResult<SqliteSourceDirectoryAuthority> {
    SqliteSourceDirectoryAuthority::retain(data_root, authorized_parent, approved_parent_path)
}

/// Opens one approved SQLite leaf through stock rusqlite/SQLite behavior.
pub(crate) fn open_root_handle_sqlite_source_snapshot(
    authority: &SqliteSourceDirectoryAuthority,
    database_name: &OsStr,
) -> SqliteSourceAccessResult<SqliteSourceReadSnapshot> {
    open_root_handle_sqlite_source_snapshot_inner(
        authority,
        database_name,
        SqliteSourceSnapshotPolicy::StrictPhysicalFamily,
        || {},
        || {},
        || {},
    )
}
#[cfg(test)]
pub(super) fn open_root_handle_sqlite_source_snapshot_with_policy(
    authority: &SqliteSourceDirectoryAuthority,
    database_name: &OsStr,
    policy: SqliteSourceSnapshotPolicy,
) -> SqliteSourceAccessResult<SqliteSourceReadSnapshot> {
    open_root_handle_sqlite_source_snapshot_inner(
        authority,
        database_name,
        policy,
        || {},
        || {},
        || {},
    )
}

pub(super) fn open_root_handle_sqlite_source_logical_snapshot_with_progress<E>(
    authority: &SqliteSourceDirectoryAuthority,
    database_name: &OsStr,
    report_progress: &mut impl FnMut(SourceBackedCurrentSourceProgress) -> Result<(), E>,
) -> Result<SqliteSourceReadSnapshot, SqliteSourceProgressError<E>> {
    for attempt in 0..SQLITE_SOURCE_TRANSITION_ATTEMPTS {
        let family =
            SqliteSourceFamily::open(authority, database_name, || {}).map_err(|error| {
                let artifact = error.acquisition_artifact();
                SqliteSourceProgressError::Source(error.with_diagnostic(
                    SqliteFailurePhase::SourceAcquisition,
                    artifact,
                    0,
                    0,
                    SqliteCleanupStatus::NotRequired,
                ))
            })?;
        match open_logical_online_backup_snapshot_with_progress(
            authority,
            family,
            LogicalSnapshotHooks {
                after_database_copy: || {},
                after_private_source_copy: ignore_snapshot_path,
                before_source_revalidation: || {},
                after_online_backup: ignore_snapshot_path,
            },
            SQLITE_SNAPSHOT_MAX_TOTAL_BYTES,
            report_progress,
        ) {
            Err(SqliteSourceProgressError::Source(error))
                if error.is_source_changed() && attempt + 1 < SQLITE_SOURCE_TRANSITION_ATTEMPTS =>
            {
                #[cfg(test)]
                authority
                    .snapshot_context
                    .record_logical_source_transition_retry()?;
                continue;
            }
            result => return result,
        }
    }
    Err(SqliteSourceAccessError::SourceChanged
        .with_diagnostic(
            SqliteFailurePhase::SourceAcquisition,
            SqliteArtifactKind::ProviderDatabase,
            0,
            0,
            SqliteCleanupStatus::NotRequired,
        )
        .into())
}

fn open_root_handle_sqlite_source_snapshot_inner(
    authority: &SqliteSourceDirectoryAuthority,
    database_name: &OsStr,
    policy: SqliteSourceSnapshotPolicy,
    after_parent_certification: impl FnOnce(),
    after_database_copy: impl FnOnce(),
    before_source_revalidation: impl FnOnce(),
) -> SqliteSourceAccessResult<SqliteSourceReadSnapshot> {
    let family = SqliteSourceFamily::open(authority, database_name, after_parent_certification)?;
    if policy == SqliteSourceSnapshotPolicy::LogicalOnlineBackup {
        return open_logical_online_backup_snapshot(
            authority,
            family,
            after_database_copy,
            before_source_revalidation,
            SQLITE_SNAPSHOT_MAX_TOTAL_BYTES,
        );
    }
    let native_evidence = family.capture_evidence()?;

    let acquired = acquire_sqlite_connection(
        authority.data_root(),
        &authority.snapshot_context,
        &family,
        &native_evidence,
        after_database_copy,
    )?;
    let validation: SqliteSourceAccessResult<SqliteSnapshotEvidence> = (|| {
        verify_connection_read_only(&acquired.connection)?;
        configure_and_pin_snapshot(&acquired.connection)?;
        before_source_revalidation();

        // The source family is checked only after SQLite has pinned the
        // selected view. No provider observation may escape if acquisition
        // raced a write or sidecar transition.
        family.revalidate(&native_evidence)?;
        let sqlite_evidence = capture_sqlite_evidence(&acquired.connection)?;
        family.revalidate(&native_evidence)?;
        Ok(sqlite_evidence)
    })();
    let sqlite_evidence = match validation {
        Ok(evidence) => evidence,
        Err(error) => {
            let copied_bytes = native_evidence
                .database
                .length
                .saturating_add(native_evidence.wal.as_ref().map_or(0, |wal| wal.length));
            let error = acquired.diagnose_validation_error(error, copied_bytes);
            return match acquired.cleanup() {
                Ok(()) => Err(error.with_cleanup_status(SqliteCleanupStatus::Succeeded)),
                Err(cleanup) => Err(cleanup),
            };
        }
    };
    let evidence = SqliteSourceEvidence::from_snapshot(&native_evidence, &sqlite_evidence);
    let AcquiredSqliteConnection {
        connection,
        #[cfg(test)]
        strategy,
        #[cfg(test)]
        copied_bytes,
        snapshot_directory,
        snapshot_activity,
    } = acquired;
    let snapshot = SqliteSourceReadSnapshot {
        connection: Some(connection),
        family: Some(family),
        native_evidence,
        sqlite_evidence,
        evidence,
        policy,
        admitted_revision_is_replay_safe: true,
        #[cfg(test)]
        certification: None,
        #[cfg(test)]
        strategy,
        #[cfg(test)]
        copied_bytes,
        _snapshot_directory: snapshot_directory,
        snapshot_activity: Some(snapshot_activity),
        snapshot_context: Arc::clone(&authority.snapshot_context),
        terminal_fence_slot: Arc::default(),
        #[cfg(test)]
        fail_next_cleanup: take_opened_snapshot_cleanup_failure_for_test(),
    };
    Ok(snapshot)
}

fn open_logical_online_backup_snapshot(
    authority: &SqliteSourceDirectoryAuthority,
    family: SqliteSourceFamily,
    after_database_copy: impl FnOnce(),
    before_source_revalidation: impl FnOnce(),
    scratch_limit: u64,
) -> SqliteSourceAccessResult<SqliteSourceReadSnapshot> {
    match open_logical_online_backup_snapshot_with_progress(
        authority,
        family,
        LogicalSnapshotHooks {
            after_database_copy,
            after_private_source_copy: ignore_snapshot_path,
            before_source_revalidation,
            after_online_backup: ignore_snapshot_path,
        },
        scratch_limit,
        &mut |_| Ok::<(), std::convert::Infallible>(()),
    ) {
        Ok(snapshot) => Ok(snapshot),
        Err(SqliteSourceProgressError::Source(error)) => Err(error),
        Err(SqliteSourceProgressError::Progress(never)) => match never {},
    }
}

fn open_logical_online_backup_snapshot_with_progress<
    E,
    AfterDatabaseCopy,
    AfterPrivateSourceCopy,
    BeforeSourceRevalidation,
    AfterOnlineBackup,
>(
    authority: &SqliteSourceDirectoryAuthority,
    family: SqliteSourceFamily,
    hooks: LogicalSnapshotHooks<
        AfterDatabaseCopy,
        AfterPrivateSourceCopy,
        BeforeSourceRevalidation,
        AfterOnlineBackup,
    >,
    scratch_limit: u64,
    report_progress: &mut impl FnMut(SourceBackedCurrentSourceProgress) -> Result<(), E>,
) -> Result<SqliteSourceReadSnapshot, SqliteSourceProgressError<E>>
where
    AfterDatabaseCopy: FnOnce(),
    AfterPrivateSourceCopy: FnOnce(&Path),
    BeforeSourceRevalidation: FnOnce(),
    AfterOnlineBackup: FnOnce(&Path),
{
    let LogicalSnapshotHooks {
        after_database_copy,
        after_private_source_copy,
        before_source_revalidation,
        after_online_backup,
    } = hooks;
    let opening_evidence = family.capture_evidence()?;
    enforce_snapshot_copy_bounds_with_limit(&family, &opening_evidence, scratch_limit)?;
    let mut source = Some(
        acquire_online_backup_source(
            authority.data_root(),
            &authority.snapshot_context,
            &family,
            &opening_evidence,
            (after_database_copy, after_private_source_copy),
            scratch_limit,
            report_progress,
        )
        .map_err(|error| match error {
            SqliteSourceProgressError::Source(error) => {
                if error.diagnostic().is_some() {
                    SqliteSourceProgressError::Source(error)
                } else {
                    let artifact = error.acquisition_artifact();
                    SqliteSourceProgressError::Source(error.with_diagnostic(
                        SqliteFailurePhase::SourceAcquisition,
                        artifact,
                        0,
                        0,
                        SqliteCleanupStatus::NotRequired,
                    ))
                }
            }
            SqliteSourceProgressError::Progress(error) => {
                SqliteSourceProgressError::Progress(error)
            }
        })?,
    );
    let mut source_pinned = false;
    let prepared = (|| {
        let source = source
            .as_ref()
            .ok_or(SqliteSourceAccessError::SnapshotNotActive)?;
        verify_connection_read_only(&source.connection).map_err(|error| {
            error.with_diagnostic(
                SqliteFailurePhase::SourceValidation,
                source.artifact,
                0,
                source.copied_source_bytes,
                SqliteCleanupStatus::NotRequired,
            )
        })?;
        source
            .connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(|sqlite| {
                sqlite_error(
                    "configuring the provider online-backup busy timeout",
                    sqlite,
                )
                .with_diagnostic(
                    SqliteFailurePhase::SourceValidation,
                    source.artifact,
                    0,
                    source.copied_source_bytes,
                    SqliteCleanupStatus::NotRequired,
                )
            })?;
        configure_and_pin_snapshot(&source.connection).map_err(|error| {
            error.with_diagnostic(
                SqliteFailurePhase::SourceValidation,
                source.artifact,
                0,
                source.copied_source_bytes,
                SqliteCleanupStatus::NotRequired,
            )
        })?;
        source_pinned = true;
        before_source_revalidation();

        // The source transaction is always pinned in ctx-owned storage when a
        // WAL family exists. Only provider identities and revision tokens are
        // re-observed; SQLite itself never opens provider coordination files.
        let logical_identity_stable = match family.revalidate_logical_identity(&opening_evidence) {
            Ok(()) => true,
            Err(error) if error.is_source_changed() && opening_evidence.wal.is_none() => {
                family.revalidate_logical_database_identity(&opening_evidence)?;
                false
            }
            Err(error) => return Err(error),
        };
        let admitted_revision_is_replay_safe = logical_identity_stable
            && match family.revalidate_revision(&opening_evidence) {
                Ok(()) => true,
                Err(error) if error.is_source_changed() => false,
                Err(error) => return Err(error),
            };
        let source_sqlite_evidence =
            capture_sqlite_evidence(&source.connection).map_err(|error| {
                error.with_diagnostic(
                    SqliteFailurePhase::SourceValidation,
                    source.artifact,
                    0,
                    source.copied_source_bytes,
                    SqliteCleanupStatus::NotRequired,
                )
            })?;
        let bounds =
            enforce_online_backup_bounds(&source.connection, &family.database.path, scratch_limit)
                .map_err(|error| {
                    error.with_diagnostic(
                        SqliteFailurePhase::SourceValidation,
                        source.artifact,
                        0,
                        source.copied_source_bytes,
                        SqliteCleanupStatus::NotRequired,
                    )
                })?;
        let peak_private_limit = scratch_limit.saturating_mul(2);
        let peak_private_bytes = source
            .copied_source_bytes
            .checked_add(bounds.bytes)
            .ok_or_else(|| SqliteSourceAccessError::SnapshotTooLarge {
                path: family.database.path.clone(),
                length: u64::MAX,
                maximum: peak_private_limit,
            })?;
        if peak_private_bytes > peak_private_limit {
            return Err(SqliteSourceAccessError::SnapshotTooLarge {
                path: family.database.path.clone(),
                length: peak_private_bytes,
                maximum: peak_private_limit,
            }
            .with_diagnostic(
                SqliteFailurePhase::SourceAcquisition,
                SqliteArtifactKind::PrivateSourceCopy,
                0,
                source.copied_source_bytes,
                SqliteCleanupStatus::NotRequired,
            ));
        }
        let certification = certify_sqlite_snapshot(
            &source.connection,
            bounds,
            SqliteFailurePhase::SourceValidation,
            source.artifact,
            0,
            source.copied_source_bytes,
        )?;
        Ok((
            admitted_revision_is_replay_safe,
            source_sqlite_evidence,
            bounds,
            certification,
        ))
    })();
    let (
        admitted_revision_is_replay_safe,
        source_sqlite_evidence,
        online_backup_bounds,
        source_certification,
    ) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            let cleanup = close_online_backup_source(
                source
                    .take()
                    .ok_or(SqliteSourceAccessError::SnapshotNotActive)?,
                source_pinned,
                0,
                0,
            );
            return match cleanup {
                Ok(status) => Err(error.with_cleanup_status(status).into()),
                Err(cleanup) => Err(cleanup.into()),
            };
        }
    };
    let backup = online_backup_to_ctx(
        authority.data_root(),
        &source
            .as_ref()
            .ok_or(SqliteSourceAccessError::SnapshotNotActive)?
            .connection,
        scratch_limit,
        online_backup_bounds,
        report_progress,
    );
    let (snapshot_directory, snapshot_path, snapshot_bytes) = match backup {
        Ok(backup) => backup,
        Err(error) => {
            let cleanup = close_online_backup_source(
                source
                    .take()
                    .ok_or(SqliteSourceAccessError::SnapshotNotActive)?,
                source_pinned,
                online_backup_bounds.page_count,
                online_backup_bounds.bytes,
            );
            return match cleanup {
                Ok(status) => Err(match error {
                    SqliteSourceProgressError::Source(error) => {
                        SqliteSourceProgressError::Source(error.with_cleanup_status(status))
                    }
                    SqliteSourceProgressError::Progress(error) => {
                        SqliteSourceProgressError::Progress(error)
                    }
                }),
                Err(cleanup) => Err(cleanup.into()),
            };
        }
    };
    let source_cleanup_status = match close_online_backup_source(
        source
            .take()
            .ok_or(SqliteSourceAccessError::SnapshotNotActive)?,
        source_pinned,
        online_backup_bounds.page_count,
        snapshot_bytes,
    ) {
        Ok(status) => status,
        Err(error) => {
            return match close_private_snapshot_directory(
                snapshot_directory,
                SqliteArtifactKind::PrivateBackup,
                online_backup_bounds.page_count,
                snapshot_bytes,
            ) {
                Ok(()) => Err(error
                    .with_cleanup_status(SqliteCleanupStatus::Succeeded)
                    .into()),
                Err(cleanup) => Err(cleanup.into()),
            };
        }
    };
    after_online_backup(&snapshot_path);
    if let Err(error) = family.revalidate_logical_database_identity(&opening_evidence) {
        return match close_private_snapshot_directory(
            snapshot_directory,
            SqliteArtifactKind::PrivateBackup,
            online_backup_bounds.page_count,
            snapshot_bytes,
        ) {
            Ok(()) => Err(error
                .with_cleanup_status(SqliteCleanupStatus::Succeeded)
                .into()),
            Err(cleanup) => Err(cleanup.into()),
        };
    }
    let native_evidence = opening_evidence.clone();

    let connection = Connection::open_with_flags(
        &snapshot_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|source| {
        sqlite_error("opening the private logical SQLite backup", source).with_diagnostic(
            SqliteFailurePhase::BackupValidation,
            SqliteArtifactKind::PrivateBackup,
            online_backup_bounds.page_count,
            snapshot_bytes,
            source_cleanup_status,
        )
    });
    let connection = match connection {
        Ok(connection) => connection,
        Err(error) => {
            return match close_private_snapshot_directory(
                snapshot_directory,
                SqliteArtifactKind::PrivateBackup,
                online_backup_bounds.page_count,
                snapshot_bytes,
            ) {
                Ok(()) => Err(error.into()),
                Err(cleanup) => Err(cleanup.into()),
            };
        }
    };
    let validation = (|| {
        verify_connection_read_only(&connection).map_err(|error| {
            error.with_diagnostic(
                SqliteFailurePhase::BackupValidation,
                SqliteArtifactKind::PrivateBackup,
                online_backup_bounds.page_count,
                snapshot_bytes,
                source_cleanup_status,
            )
        })?;
        configure_and_pin_snapshot(&connection).map_err(|error| {
            error.with_diagnostic(
                SqliteFailurePhase::BackupValidation,
                SqliteArtifactKind::PrivateBackup,
                online_backup_bounds.page_count,
                snapshot_bytes,
                source_cleanup_status,
            )
        })?;
        let backup_certification = certify_sqlite_snapshot(
            &connection,
            online_backup_bounds,
            SqliteFailurePhase::BackupValidation,
            SqliteArtifactKind::PrivateBackup,
            online_backup_bounds.page_count,
            snapshot_bytes,
        )?;
        if source_certification.pages != backup_certification.pages
            || source_certification.bytes != backup_certification.bytes
        {
            return Err(SqliteSourceAccessError::SnapshotUnavailable {
                reason: "the certified private SQLite backup size differs from its source view"
                    .to_owned(),
            });
        }
        let sqlite_evidence = capture_sqlite_evidence(&connection).map_err(|error| {
            error.with_diagnostic(
                SqliteFailurePhase::BackupValidation,
                SqliteArtifactKind::PrivateBackup,
                online_backup_bounds.page_count,
                snapshot_bytes,
                source_cleanup_status,
            )
        })?;
        if !sqlite_evidence.same_database_view(&source_sqlite_evidence) {
            return Err(SqliteSourceAccessError::SnapshotUnavailable {
                reason: "the private SQLite backup does not match its pinned source view"
                    .to_owned(),
            });
        }
        family.revalidate_logical_database_identity(&native_evidence)?;
        authority
            .snapshot_context
            .record_logical_online_backup_bytes(snapshot_bytes)?;
        let snapshot_activity = authority.snapshot_context.record_open(
            SqliteSourceSnapshotStrategy::LogicalOnlineBackup,
            snapshot_bytes,
        )?;
        Ok((sqlite_evidence, backup_certification, snapshot_activity))
    })();
    let (sqlite_evidence, backup_certification, snapshot_activity) = match validation {
        Ok(validated) => validated,
        Err(error) => {
            let close = close_private_sqlite_connection(
                connection,
                "closing a rejected private logical SQLite backup",
                SqliteArtifactKind::PrivateBackup,
                online_backup_bounds.page_count,
                snapshot_bytes,
            );
            let cleanup = close_private_snapshot_directory(
                snapshot_directory,
                SqliteArtifactKind::PrivateBackup,
                online_backup_bounds.page_count,
                snapshot_bytes,
            );
            return match (close, cleanup) {
                (_, Err(cleanup)) | (Err(cleanup), Ok(())) => Err(cleanup.into()),
                (Ok(()), Ok(())) => Err(error
                    .with_cleanup_status(SqliteCleanupStatus::Succeeded)
                    .into()),
            };
        }
    };
    #[cfg(not(test))]
    let _ = backup_certification;
    let evidence = SqliteSourceEvidence::from_snapshot(&native_evidence, &sqlite_evidence);
    let snapshot = SqliteSourceReadSnapshot {
        connection: Some(connection),
        family: Some(family),
        native_evidence,
        sqlite_evidence,
        evidence,
        policy: SqliteSourceSnapshotPolicy::LogicalOnlineBackup,
        admitted_revision_is_replay_safe,
        #[cfg(test)]
        certification: Some(SqliteSnapshotCertification {
            source: source_certification,
            backup: backup_certification,
        }),
        #[cfg(test)]
        strategy: SqliteSourceSnapshotStrategy::LogicalOnlineBackup,
        #[cfg(test)]
        copied_bytes: snapshot_bytes,
        _snapshot_directory: Some(snapshot_directory),
        snapshot_activity: Some(snapshot_activity),
        snapshot_context: Arc::clone(&authority.snapshot_context),
        terminal_fence_slot: Arc::default(),
        #[cfg(test)]
        fail_next_cleanup: take_opened_snapshot_cleanup_failure_for_test(),
    };
    Ok(snapshot)
}

fn online_backup_to_ctx<E>(
    data_root: &Path,
    source: &Connection,
    scratch_limit: u64,
    bounds: OnlineBackupBounds,
    report_progress: &mut impl FnMut(SourceBackedCurrentSourceProgress) -> Result<(), E>,
) -> Result<(TempDir, PathBuf, u64), SqliteSourceProgressError<E>> {
    let directory = create_snapshot_directory(data_root, "provider-sqlite-online-backup-")?;
    let snapshot_path = directory.path().join("source.sqlite");
    let destination = Connection::open_with_flags(
        &snapshot_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|source| {
        SqliteSourceAccessError::private_scratch_sqlite(
            "creating the private logical SQLite backup",
            source,
        )
        .with_diagnostic(
            SqliteFailurePhase::OnlineBackup,
            SqliteArtifactKind::PrivateBackup,
            0,
            0,
            SqliteCleanupStatus::NotRequired,
        )
    });
    let destination = match destination {
        Ok(destination) => destination,
        Err(error) => {
            return match close_private_snapshot_directory(
                directory,
                SqliteArtifactKind::PrivateBackup,
                0,
                0,
            ) {
                Ok(()) => Err(error
                    .with_cleanup_status(SqliteCleanupStatus::Succeeded)
                    .into()),
                Err(cleanup) => Err(cleanup.into()),
            };
        }
    };
    let operation = (|| {
        run_online_backup_with_progress(source, &destination, bounds, report_progress)?;
        destination
            .query_row("PRAGMA journal_mode=DELETE", [], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|source| {
                SqliteSourceAccessError::private_scratch_sqlite(
                    "normalizing private backup journal mode",
                    source,
                )
                .with_diagnostic(
                    SqliteFailurePhase::OnlineBackup,
                    SqliteArtifactKind::PrivateBackup,
                    bounds.page_count,
                    bounds.bytes,
                    SqliteCleanupStatus::NotRequired,
                )
            })?;
        Ok(())
    })();
    let close = close_private_sqlite_connection(
        destination,
        "closing the private logical SQLite backup writer",
        SqliteArtifactKind::PrivateBackup,
        bounds.page_count,
        bounds.bytes,
    );
    let operation = match (operation, close) {
        (_, Err(close)) => Err(SqliteSourceProgressError::Source(close)),
        (operation, Ok(())) => operation,
    };
    let snapshot_bytes = match operation {
        Ok(()) => std::fs::metadata(&snapshot_path)
            .map_err(|source| SqliteSourceAccessError::ScratchIoUnavailable {
                operation: "measuring the private logical SQLite backup",
                path: snapshot_path.clone(),
                source,
            })
            .map(|metadata| metadata.len())
            .map_err(SqliteSourceProgressError::Source),
        Err(error) => Err(error),
    };
    let snapshot_bytes = match snapshot_bytes {
        Ok(snapshot_bytes) if snapshot_bytes <= scratch_limit => snapshot_bytes,
        Ok(snapshot_bytes) => {
            let error = SqliteSourceAccessError::SnapshotTooLarge {
                path: snapshot_path.clone(),
                length: snapshot_bytes,
                maximum: scratch_limit,
            };
            return match close_private_snapshot_directory(
                directory,
                SqliteArtifactKind::PrivateBackup,
                bounds.page_count,
                snapshot_bytes,
            ) {
                Ok(()) => Err(error
                    .with_cleanup_status(SqliteCleanupStatus::Succeeded)
                    .into()),
                Err(cleanup) => Err(cleanup.into()),
            };
        }
        Err(error) => {
            return match close_private_snapshot_directory(
                directory,
                SqliteArtifactKind::PrivateBackup,
                bounds.page_count,
                bounds.bytes,
            ) {
                Ok(()) => Err(match error {
                    SqliteSourceProgressError::Source(error) => SqliteSourceProgressError::Source(
                        error.with_cleanup_status(SqliteCleanupStatus::Succeeded),
                    ),
                    SqliteSourceProgressError::Progress(error) => {
                        SqliteSourceProgressError::Progress(error)
                    }
                }),
                Err(cleanup) => Err(cleanup.into()),
            };
        }
    };
    Ok((directory, snapshot_path, snapshot_bytes))
}

fn run_online_backup_with_progress<E>(
    source: &Connection,
    destination: &Connection,
    bounds: OnlineBackupBounds,
    report_progress: &mut impl FnMut(SourceBackedCurrentSourceProgress) -> Result<(), E>,
) -> Result<(), SqliteSourceProgressError<E>> {
    let deadline = Instant::now()
        .checked_add(SQLITE_ONLINE_BACKUP_DEADLINE)
        .ok_or_else(|| SqliteSourceAccessError::SnapshotUnavailable {
            reason: "the logical SQLite online-backup deadline overflowed".to_owned(),
        })?;
    let backup = unsafe {
        ffi::sqlite3_backup_init(
            destination.handle(),
            c"main".as_ptr(),
            source.handle(),
            c"main".as_ptr(),
        )
    };
    if backup.is_null() {
        return Err(SqliteSourceAccessError::SqliteControl {
            operation: "initializing the logical SQLite online backup",
            code: unsafe { ffi::sqlite3_extended_errcode(destination.handle()) },
        }
        .with_diagnostic(
            SqliteFailurePhase::OnlineBackup,
            SqliteArtifactKind::PrivateBackup,
            0,
            0,
            SqliteCleanupStatus::NotRequired,
        )
        .into());
    }
    let mut backup = OnlineBackupHandle(Some(backup));
    let mut completed_pages = 0;
    let mut last_retry_code = None;
    report_progress(online_backup_progress(0, bounds)?)
        .map_err(SqliteSourceProgressError::Progress)?;
    loop {
        if Instant::now() >= deadline {
            return Err(online_backup_deadline_diagnostic(
                completed_pages,
                bounds,
                last_retry_code,
            )
            .into());
        }
        let code = unsafe {
            ffi::sqlite3_backup_step(backup.pointer(), SQLITE_ONLINE_BACKUP_PAGES_PER_STEP)
        };
        let observed_retry_code = retain_online_backup_retry_code(last_retry_code, code);
        if Instant::now() >= deadline {
            return Err(online_backup_deadline_diagnostic(
                completed_pages,
                bounds,
                observed_retry_code,
            )
            .into());
        }
        last_retry_code = observed_retry_code;
        match code {
            ffi::SQLITE_DONE => {
                completed_pages = report_online_backup_step(
                    &backup,
                    bounds,
                    completed_pages,
                    true,
                    report_progress,
                )?;
                break;
            }
            ffi::SQLITE_OK => {
                completed_pages = report_online_backup_step(
                    &backup,
                    bounds,
                    completed_pages,
                    false,
                    report_progress,
                )?;
            }
            ffi::SQLITE_BUSY | ffi::SQLITE_LOCKED if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(10));
            }
            code => {
                return Err(SqliteSourceAccessError::SqliteControl {
                    operation: "copying the pinned logical SQLite snapshot",
                    code,
                }
                .with_diagnostic(
                    SqliteFailurePhase::OnlineBackup,
                    SqliteArtifactKind::PrivateBackup,
                    completed_pages,
                    completed_pages.saturating_mul(bounds.page_size),
                    SqliteCleanupStatus::NotRequired,
                )
                .into());
            }
        }
    }
    if completed_pages != bounds.page_count {
        return Err(SqliteSourceAccessError::SnapshotUnavailable {
            reason: "the logical SQLite backup did not reach its terminal page total".to_owned(),
        }
        .with_diagnostic(
            SqliteFailurePhase::OnlineBackup,
            SqliteArtifactKind::PrivateBackup,
            completed_pages,
            completed_pages.saturating_mul(bounds.page_size),
            SqliteCleanupStatus::NotRequired,
        )
        .into());
    }
    let code = backup.finish();
    if code == ffi::SQLITE_OK {
        Ok(())
    } else {
        Err(SqliteSourceAccessError::SqliteControl {
            operation: "finishing the logical SQLite online backup",
            code,
        }
        .with_diagnostic(
            SqliteFailurePhase::OnlineBackup,
            SqliteArtifactKind::PrivateBackup,
            completed_pages,
            completed_pages.saturating_mul(bounds.page_size),
            SqliteCleanupStatus::NotRequired,
        )
        .into())
    }
}

fn report_online_backup_step<E>(
    backup: &OnlineBackupHandle,
    bounds: OnlineBackupBounds,
    previous_completed: u64,
    terminal: bool,
    report_progress: &mut impl FnMut(SourceBackedCurrentSourceProgress) -> Result<(), E>,
) -> Result<u64, SqliteSourceProgressError<E>> {
    let total = unsafe { ffi::sqlite3_backup_pagecount(backup.pointer()) };
    let remaining = unsafe { ffi::sqlite3_backup_remaining(backup.pointer()) };
    let total = u64::try_from(total).map_err(|_| SqliteSourceAccessError::SnapshotUnavailable {
        reason: "the logical SQLite backup reported a negative page count".to_owned(),
    })?;
    let remaining =
        u64::try_from(remaining).map_err(|_| SqliteSourceAccessError::SnapshotUnavailable {
            reason: "the logical SQLite backup reported a negative remaining page count".to_owned(),
        })?;
    if total != bounds.page_count || remaining > total {
        return Err(SqliteSourceAccessError::SnapshotUnavailable {
            reason: "the logical SQLite backup page totals changed during its pinned copy"
                .to_owned(),
        }
        .into());
    }
    let completed = total - remaining;
    if completed < previous_completed || (terminal && completed != total) {
        return Err(SqliteSourceAccessError::SnapshotUnavailable {
            reason: "the logical SQLite backup reported non-monotonic page progress".to_owned(),
        }
        .into());
    }
    report_progress(online_backup_progress(completed, bounds)?)
        .map_err(SqliteSourceProgressError::Progress)?;
    Ok(completed)
}

fn online_backup_progress(
    completed_pages: u64,
    bounds: OnlineBackupBounds,
) -> SqliteSourceAccessResult<SourceBackedCurrentSourceProgress> {
    let completed_bytes = completed_pages
        .checked_mul(bounds.page_size)
        .ok_or_else(|| SqliteSourceAccessError::SnapshotUnavailable {
            reason: "the logical SQLite backup progress byte count overflowed".to_owned(),
        })?;
    let mut progress = SourceBackedCurrentSourceProgress::new(
        SourceBackedCurrentSourceProgressStage::OnlineBackup,
    );
    progress.snapshot_pages_completed = Some(completed_pages);
    progress.snapshot_pages_total = Some(bounds.page_count);
    progress.snapshot_bytes_completed = Some(completed_bytes);
    progress.snapshot_bytes_total = Some(bounds.bytes);
    Ok(progress)
}
