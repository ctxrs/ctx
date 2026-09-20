use super::*;

pub(super) struct AcquiredSqliteConnection {
    pub(super) connection: Connection,
    pub(super) strategy: SqliteSourceSnapshotStrategy,
    pub(super) copied_bytes: u64,
    pub(super) snapshot_directory: Option<TempDir>,
    pub(super) live_authority_handle: Option<File>,
    pub(super) snapshot_activity: Option<SqliteSourceSnapshotActivity>,
    pub(super) scratch: Arc<SqliteRouteScratch>,
}

impl AcquiredSqliteConnection {
    pub(super) fn finalize_accounting(
        &mut self,
        snapshot_context: &Arc<SqliteSourceSnapshotContext>,
    ) -> SqliteSourceAccessResult<u64> {
        let retained_bytes = self
            .snapshot_directory
            .as_ref()
            .map_or(Ok(0), |directory| {
                measure_private_snapshot_bytes(directory.path(), self.scratch.maximum_bytes)
            })?;
        self.scratch.set_retained_bytes(retained_bytes)?;
        if self.copied_bytes != 0 {
            snapshot_context.record_source_bytes_copied(self.copied_bytes)?;
        }
        self.snapshot_activity = Some(snapshot_context.record_open(self.strategy, retained_bytes)?);
        Ok(retained_bytes)
    }

    pub(super) fn cleanup(mut self) -> SqliteSourceAccessResult<()> {
        let artifact = if self.snapshot_directory.is_some() {
            SqliteArtifactKind::PrivateSourceCopy
        } else {
            SqliteArtifactKind::ProviderDatabase
        };
        let close = close_private_sqlite_connection(
            self.connection,
            "closing a rejected SQLite source snapshot",
            artifact,
            0,
            self.copied_bytes,
        );
        let cleanup = self.snapshot_directory.take().map_or(Ok(()), |directory| {
            close_private_snapshot_directory(directory, artifact, 0, self.copied_bytes)
        });
        drop(self.snapshot_activity.take());
        combine_sqlite_source_cleanup(close, cleanup)
    }

    pub(super) fn diagnose_validation_error(
        &self,
        error: SqliteSourceAccessError,
    ) -> SqliteSourceAccessError {
        let artifact = if self.snapshot_directory.is_some() {
            SqliteArtifactKind::PrivateSourceCopy
        } else {
            SqliteArtifactKind::ProviderDatabase
        };
        error
            .with_diagnostic(
                SqliteFailurePhase::SourceValidation,
                artifact,
                0,
                self.copied_bytes,
                SqliteCleanupStatus::NotRequired,
            )
            .with_exact_provider_content_provenance()
    }
}

impl SqliteSourceReadSnapshot {
    /// Explicitly closes a snapshot that cannot proceed to publication.
    /// Drop remains a defensive second attempt for unwind safety.
    pub fn abort(mut self) -> SqliteSourceAccessResult<()> {
        self.snapshot_context.record_explicit_abort();
        self.explicitly_completed = true;
        self.cleanup_snapshot_storage()
    }
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static FAIL_NEXT_OPENED_SNAPSHOT_CLEANUP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FORCE_NEXT_PINNED_WAL_UNAVAILABLE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FAIL_NEXT_SNAPSHOT_OPEN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static FAIL_NEXT_SCRATCH_WRITE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(any(test, feature = "test-support"))]
pub fn fail_next_opened_snapshot_cleanup_for_test() {
    FAIL_NEXT_OPENED_SNAPSHOT_CLEANUP.with(|fail| fail.set(true));
}

#[cfg(any(test, feature = "test-support"))]
pub fn force_next_pinned_wal_unavailable_for_test() {
    FORCE_NEXT_PINNED_WAL_UNAVAILABLE.with(|force| force.set(true));
}

#[cfg(any(test, feature = "test-support"))]
pub fn fail_next_snapshot_write_enospc_for_test() {
    FAIL_NEXT_SCRATCH_WRITE.with(|fail| fail.set(true));
}

#[cfg(any(test, feature = "test-support"))]
pub fn fail_next_snapshot_open_for_test() {
    FAIL_NEXT_SNAPSHOT_OPEN.with(|fail| fail.set(true));
}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn take_opened_snapshot_cleanup_failure_for_test() -> bool {
    FAIL_NEXT_OPENED_SNAPSHOT_CLEANUP.with(|fail| fail.replace(false))
}

#[cfg(any(test, feature = "test-support"))]
fn take_snapshot_write_failure_for_test() -> bool {
    FAIL_NEXT_SCRATCH_WRITE.with(|fail| fail.replace(false))
}

#[cfg(any(test, feature = "test-support"))]
fn take_snapshot_open_failure_for_test() -> bool {
    FAIL_NEXT_SNAPSHOT_OPEN.with(|fail| fail.replace(false))
}

#[cfg(any(test, feature = "test-support"))]
fn take_forced_pinned_wal_unavailable_for_test() -> bool {
    FORCE_NEXT_PINNED_WAL_UNAVAILABLE.with(|force| force.replace(false))
}

pub(super) fn acquire_sqlite_connection_with_progress<E>(
    snapshot_context: &Arc<SqliteSourceSnapshotContext>,
    family: &SqliteSourceFamily,
    evidence: &SqliteFamilyEvidence,
    options: SqliteSourceSnapshotOptions,
    after_database_copy: impl FnOnce(),
    report_progress: &mut impl FnMut(SqliteSourceProgress) -> Result<(), E>,
) -> Result<AcquiredSqliteConnection, SqliteSourceProgressError<E>> {
    if let SqliteSourceSnapshotPolicy::SelectivePrivateCopy(tables) = options.policy {
        return super::selective::acquire(
            snapshot_context,
            family,
            evidence,
            options.limits,
            tables,
            report_progress,
        );
    }
    let source_limit = options.limits.maximum_source_bytes();
    let scratch_limit = options.limits.maximum_scratch_limit();
    let scratch = SqliteRouteScratch::new(snapshot_context, scratch_limit);
    if options.policy == SqliteSourceSnapshotPolicy::PinnedReadOnlyWal {
        #[cfg(any(test, feature = "test-support"))]
        if take_forced_pinned_wal_unavailable_for_test() {
            return Err(SqliteSourceAccessError::SnapshotUnavailable {
                reason: "pinned read-only WAL snapshots are unavailable on the simulated platform"
                    .to_owned(),
            }
            .into());
        }
        #[cfg(not(target_os = "linux"))]
        return Err(SqliteSourceAccessError::SnapshotUnavailable {
            reason: "pinned read-only WAL snapshots require the Linux unix VFS".to_owned(),
        }
        .into());
        #[cfg(target_os = "linux")]
        let (connection, live_authority_handle) = open_pinned_read_only_wal(family)?;
        #[cfg(target_os = "linux")]
        return Ok(AcquiredSqliteConnection {
            connection,
            strategy: SqliteSourceSnapshotStrategy::PinnedReadOnlyWal,
            copied_bytes: 0,
            snapshot_directory: None,
            live_authority_handle: Some(live_authority_handle),
            snapshot_activity: None,
            scratch,
        });
    }
    let copied_bytes = enforce_snapshot_copy_bounds_with_limit(family, evidence, source_limit)?;
    if options.policy == SqliteSourceSnapshotPolicy::ExactRevision
        && family.wal.is_none()
        && family.shared_memory.is_none()
    {
        #[cfg(target_os = "linux")]
        if immutable_procfd_available(family.database.file()) {
            return Ok(AcquiredSqliteConnection {
                connection: open_immutable_main(&family.database)?,
                strategy: SqliteSourceSnapshotStrategy::ImmutableMain,
                copied_bytes: 0,
                snapshot_directory: None,
                live_authority_handle: None,
                snapshot_activity: None,
                scratch,
            });
        }
    }

    let coordination_reserve = if evidence.wal.as_ref().is_some_and(|wal| wal.length != 0) {
        SQLITE_SHM_MAX_BYTES
    } else {
        0
    };
    let admitted_capacity = copied_bytes
        .checked_add(coordination_reserve)
        .ok_or_else(|| SqliteSourceAccessError::SnapshotUnavailable {
            reason: "SQLite source-copy capacity accounting overflowed".to_owned(),
        })?;
    scratch.admit_capacity(admitted_capacity)?;
    let (snapshot_directory, snapshot_path) = copy_sqlite_family_to_ctx_with_progress(
        snapshot_context.data_root.as_path(),
        family,
        evidence,
        scratch_limit,
        after_database_copy,
        report_progress,
    )?;
    let open_connection = || {
        Connection::open_with_flags(
            &snapshot_path,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(|source| {
            sqlite_error("opening the ctx-owned provider snapshot", source)
                .with_exact_provider_content_provenance()
                .with_diagnostic(
                    SqliteFailurePhase::SourceValidation,
                    SqliteArtifactKind::PrivateSourceCopy,
                    0,
                    copied_bytes,
                    SqliteCleanupStatus::NotRequired,
                )
        })
    };
    #[cfg(any(test, feature = "test-support"))]
    let connection = if take_snapshot_open_failure_for_test() {
        Err(SqliteSourceAccessError::Sqlite {
            operation: "opening the ctx-owned provider snapshot",
            source: rusqlite::Error::InvalidQuery,
        }
        .with_diagnostic(
            SqliteFailurePhase::SourceValidation,
            SqliteArtifactKind::PrivateSourceCopy,
            0,
            copied_bytes,
            SqliteCleanupStatus::NotRequired,
        ))
    } else {
        open_connection()
    };
    #[cfg(not(any(test, feature = "test-support")))]
    let connection = open_connection();
    let connection = match connection {
        Ok(connection) => connection,
        Err(error) => {
            return match close_private_snapshot_directory(
                snapshot_directory,
                SqliteArtifactKind::PrivateSourceCopy,
                0,
                copied_bytes,
            ) {
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
    Ok(AcquiredSqliteConnection {
        connection,
        strategy: SqliteSourceSnapshotStrategy::CopiedFamily,
        copied_bytes,
        snapshot_directory: Some(snapshot_directory),
        live_authority_handle: None,
        snapshot_activity: None,
        scratch,
    })
}

#[cfg(target_os = "linux")]
pub(in crate::sqlite_source) fn open_pinned_read_only_wal(
    family: &SqliteSourceFamily,
) -> SqliteSourceAccessResult<(Connection, File)> {
    const MINIMUM_READONLY_SHM_SQLITE_VERSION: i32 = 3_046_000;
    admit_pinned_read_only_wal(
        unsafe { libc::geteuid() },
        unsafe { ffi::sqlite3_libversion_number() },
        !unsafe { ffi::sqlite3_vfs_find(c"unix".as_ptr()) }.is_null(),
        MINIMUM_READONLY_SHM_SQLITE_VERSION,
    )?;
    let authority_handle = family.retain_parent_handle()?;
    let previous_directory = File::open(".").map_err(|source| SqliteSourceAccessError::Io {
        operation: "retaining the caller directory before a direct SQLite open",
        path: PathBuf::from("."),
        source,
    })?;
    if unsafe { libc::unshare(libc::CLONE_FS) } != 0 {
        return Err(SqliteSourceAccessError::SnapshotUnavailable {
            reason: format!(
                "could not isolate the SQLite opener filesystem context: {}",
                std::io::Error::last_os_error()
            ),
        });
    }
    if unsafe { libc::fchdir(authority_handle.as_raw_fd()) } != 0 {
        return Err(SqliteSourceAccessError::Io {
            operation: "binding the SQLite opener to its retained parent authority",
            path: family.approved_parent_path().to_path_buf(),
            source: std::io::Error::last_os_error(),
        });
    }
    let encoded_leaf =
        url::form_urlencoded::byte_serialize(family.database_name().as_bytes()).collect::<String>();
    let uri = format!("file:{encoded_leaf}?mode=ro&readonly_shm=1&vfs=unix");
    let opened = Connection::open_with_flags(
        &uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    );
    let restore = unsafe { libc::fchdir(previous_directory.as_raw_fd()) };
    if restore != 0 {
        drop(opened);
        return Err(SqliteSourceAccessError::Io {
            operation: "restoring the caller directory after a direct SQLite open",
            path: PathBuf::from("."),
            source: std::io::Error::last_os_error(),
        });
    }
    let connection = opened.map_err(|source| {
        sqlite_error("opening the pinned read-only provider WAL snapshot", source)
    })?;
    Ok((connection, authority_handle))
}

#[cfg(target_os = "linux")]
pub(in crate::sqlite_source) fn admit_pinned_read_only_wal(
    effective_uid: libc::uid_t,
    sqlite_version: i32,
    unix_vfs_available: bool,
    minimum_sqlite_version: i32,
) -> SqliteSourceAccessResult<()> {
    // The unix VFS calls robustFchown() while opening an existing SHM file.
    // readonly_shm keeps that file descriptor O_RDONLY, but root can still
    // alter SHM ownership metadata. Refuse the primitive before SQLite open so
    // the provider family remains byte-for-byte and metadata-for-metadata
    // untouched.
    if effective_uid == 0 {
        return Err(SqliteSourceAccessError::SnapshotUnavailable {
            reason: "pinned read-only WAL snapshots are disabled for effective UID 0".to_owned(),
        });
    }
    if sqlite_version < minimum_sqlite_version || !unix_vfs_available {
        return Err(SqliteSourceAccessError::SnapshotUnavailable {
            reason: "bundled SQLite unix VFS readonly_shm support is unavailable".to_owned(),
        });
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) fn immutable_procfd_available(database: &File) -> bool {
    PathBuf::from(format!("/proc/self/fd/{}", database.as_raw_fd())).exists()
}

#[cfg(target_os = "linux")]
pub(super) fn open_immutable_main(
    database: &SqliteFamilyMember,
) -> SqliteSourceAccessResult<Connection> {
    let procfd_path = PathBuf::from(format!("/proc/self/fd/{}", database.file().as_raw_fd()));
    let mut uri = Url::from_file_path(&procfd_path).map_err(|()| {
        SqliteSourceAccessError::SnapshotUnavailable {
            reason: "the retained SQLite main handle cannot be represented as a file URI"
                .to_owned(),
        }
    })?;
    uri.query_pairs_mut()
        .append_pair("mode", "ro")
        .append_pair("immutable", "1");
    Connection::open_with_flags(
        uri.as_str(),
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE
            | OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|source| sqlite_error("opening the retained immutable provider database", source))
}

pub(super) fn enforce_snapshot_copy_bounds_with_limit(
    family: &SqliteSourceFamily,
    evidence: &SqliteFamilyEvidence,
    scratch_limit: Option<u64>,
) -> SqliteSourceAccessResult<u64> {
    let mut total = evidence.database.length;
    match (family.wal.as_ref(), evidence.wal.as_ref()) {
        (Some(_), Some(state)) => {
            total = total.checked_add(state.length).ok_or_else(|| {
                SqliteSourceAccessError::SnapshotUnavailable {
                    reason: "SQLite source-family length accounting overflowed".to_owned(),
                }
            })?;
        }
        (None, None) => {}
        _ => return Err(SqliteSourceAccessError::SourceChanged),
    }
    if let Some(maximum) = scratch_limit {
        if total > maximum {
            return Err(SqliteSourceAccessError::SnapshotTooLarge {
                path: family.database.path.clone(),
                length: total,
                maximum,
            });
        }
    }
    Ok(total)
}

pub(super) fn copy_sqlite_family_to_ctx_with_progress<E>(
    data_root: &Path,
    family: &SqliteSourceFamily,
    evidence: &SqliteFamilyEvidence,
    scratch_limit: Option<u64>,
    after_database_copy: impl FnOnce(),
    report_progress: &mut impl FnMut(SqliteSourceProgress) -> Result<(), E>,
) -> Result<(TempDir, PathBuf), SqliteSourceProgressError<E>> {
    let total_bytes = enforce_snapshot_copy_bounds_with_limit(family, evidence, scratch_limit)?;
    let mut completed_bytes = 0;
    let mut last_reported_bytes = 0;
    report_source_family_copy_progress(report_progress, completed_bytes, total_bytes)?;
    let directory = create_snapshot_directory(data_root, "provider-sqlite-snapshot-")?;
    let snapshot_path = directory.path().join("source.sqlite");
    let operation = (|| {
        #[cfg(any(test, feature = "test-support"))]
        if take_snapshot_write_failure_for_test() {
            return Err(SqliteSourceAccessError::ScratchIoUnavailable {
                operation: "writing a private SQLite source-family copy",
                path: snapshot_path.clone(),
                source: injected_storage_full_error(),
            }
            .into());
        }
        copy_sqlite_member_with_progress(
            &family.database,
            &snapshot_path,
            evidence.database.length,
            &mut completed_bytes,
            &mut last_reported_bytes,
            total_bytes,
            report_progress,
        )?;
        after_database_copy();
        family.revalidate(evidence)?;
        match (family.wal.as_ref(), evidence.wal.as_ref()) {
            (Some(wal), Some(state)) => copy_sqlite_member_with_progress(
                wal,
                &directory.path().join("source.sqlite-wal"),
                state.length,
                &mut completed_bytes,
                &mut last_reported_bytes,
                total_bytes,
                report_progress,
            )?,
            (None, None) => {}
            _ => return Err(SqliteSourceAccessError::SourceChanged.into()),
        }
        if completed_bytes != total_bytes {
            return Err(SqliteSourceAccessError::SourceChanged.into());
        }
        family.revalidate(evidence)?;
        Ok(())
    })();
    match operation {
        Ok(()) => Ok((directory, snapshot_path)),
        Err(error) => match close_private_snapshot_directory(
            directory,
            SqliteArtifactKind::PrivateSourceCopy,
            0,
            completed_bytes,
        ) {
            Ok(()) => Err(error),
            Err(cleanup) => Err(error.with_finalization(cleanup)),
        },
    }
}

#[cfg(any(test, feature = "test-support"))]
fn injected_storage_full_error() -> std::io::Error {
    std::io::Error::from(std::io::ErrorKind::StorageFull)
}

fn measure_private_snapshot_bytes(
    directory: &Path,
    maximum: Option<u64>,
) -> SqliteSourceAccessResult<u64> {
    let mut total = 0_u64;
    for name in [
        "source.sqlite",
        "source.sqlite-wal",
        "source.sqlite-shm",
        "source.sqlite-journal",
    ] {
        let path = directory.join(name);
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(SqliteSourceAccessError::ScratchIoUnavailable {
                    operation: "measuring private SQLite route scratch",
                    path,
                    source,
                });
            }
        };
        if !metadata.file_type().is_file() {
            return Err(SqliteSourceAccessError::UnsafeFile {
                path,
                reason: "private SQLite scratch artifact must be a regular file",
            });
        }
        total = total.checked_add(metadata.len()).ok_or_else(|| {
            SqliteSourceAccessError::SnapshotUnavailable {
                reason: "SQLite private snapshot measurement overflowed".to_owned(),
            }
        })?;
    }
    if let Some(maximum) = maximum {
        if total > maximum {
            return Err(SqliteSourceAccessError::SnapshotTooLarge {
                path: directory.to_path_buf(),
                length: total,
                maximum,
            });
        }
    }
    Ok(total)
}

pub(super) fn create_snapshot_directory(
    data_root: &Path,
    prefix: &str,
) -> SqliteSourceAccessResult<TempDir> {
    let staging_root = data_root.join("tmp").join("provider-sqlite");
    create_private_directory_all(&staging_root).map_err(|source| {
        SqliteSourceAccessError::ScratchIoUnavailable {
            operation: "creating the private provider SQLite staging root",
            path: staging_root.clone(),
            source,
        }
    })?;
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(&staging_root)
        .map_err(|source| SqliteSourceAccessError::ScratchIoUnavailable {
            operation: "creating a private provider SQLite snapshot",
            path: staging_root,
            source,
        })
}

pub fn close_private_snapshot_directory(
    directory: TempDir,
    artifact: SqliteArtifactKind,
    copied_pages: u64,
    copied_bytes: u64,
) -> SqliteSourceAccessResult<()> {
    let path = directory.path().to_path_buf();
    #[cfg(any(test, feature = "test-support"))]
    if take_private_directory_cleanup_failure_for_test() {
        drop(directory);
        return Err(SqliteSourceAccessError::ScratchIoUnavailable {
            operation: "removing a ctx-owned SQLite snapshot directory",
            path,
            source: std::io::Error::other("injected private SQLite directory cleanup failure"),
        }
        .with_diagnostic(
            SqliteFailurePhase::Cleanup,
            artifact,
            copied_pages,
            copied_bytes,
            SqliteCleanupStatus::Failed,
        ));
    }
    directory.close().map_err(|source| {
        SqliteSourceAccessError::ScratchIoUnavailable {
            operation: "removing a ctx-owned SQLite snapshot directory",
            path,
            source,
        }
        .with_diagnostic(
            SqliteFailurePhase::Cleanup,
            artifact,
            copied_pages,
            copied_bytes,
            SqliteCleanupStatus::Failed,
        )
    })
}

pub fn close_private_sqlite_connection(
    connection: Connection,
    operation: &'static str,
    artifact: SqliteArtifactKind,
    copied_pages: u64,
    copied_bytes: u64,
) -> SqliteSourceAccessResult<()> {
    connection.close().map_err(|(_, source)| {
        SqliteSourceAccessError::ScratchSqliteUnavailable { operation, source }.with_diagnostic(
            SqliteFailurePhase::Cleanup,
            artifact,
            copied_pages,
            copied_bytes,
            SqliteCleanupStatus::Failed,
        )
    })
}
