use super::*;

pub(super) struct OnlineBackupSource {
    pub(super) connection: Connection,
    copied_source_directory: Option<TempDir>,
    pub(super) copied_source_bytes: u64,
    pub(super) artifact: SqliteArtifactKind,
}

pub(super) fn close_online_backup_source(
    source: OnlineBackupSource,
    pinned: bool,
    copied_pages: u64,
    copied_bytes: u64,
) -> SqliteSourceAccessResult<SqliteCleanupStatus> {
    let OnlineBackupSource {
        connection,
        copied_source_directory,
        artifact,
        ..
    } = source;
    let end_snapshot = if pinned {
        end_pinned_read_snapshot(&connection).map_err(|source| {
            SqliteSourceAccessError::CleanupUnavailable {
                operation: "ending the private SQLite source-copy transaction",
                source: Box::new(source),
            }
            .with_diagnostic(
                SqliteFailurePhase::Cleanup,
                artifact,
                copied_pages,
                copied_bytes,
                SqliteCleanupStatus::Failed,
            )
        })
    } else {
        Ok(())
    };
    let close_connection = close_private_sqlite_connection(
        connection,
        "closing the private SQLite source-copy connection",
        artifact,
        copied_pages,
        copied_bytes,
    );
    let cleanup_status = if copied_source_directory.is_some() {
        SqliteCleanupStatus::Succeeded
    } else {
        SqliteCleanupStatus::NotRequired
    };
    let close_directory = copied_source_directory.map_or(Ok(()), |directory| {
        close_private_snapshot_directory(directory, artifact, copied_pages, copied_bytes)
    });
    match (end_snapshot, close_connection, close_directory) {
        (_, _, Err(error)) | (_, Err(error), Ok(())) | (Err(error), Ok(()), Ok(())) => Err(error),
        (Ok(()), Ok(()), Ok(())) => Ok(cleanup_status),
    }
}

pub(super) fn acquire_online_backup_source<E>(
    data_root: &Path,
    snapshot_context: &Arc<SqliteSourceSnapshotContext>,
    family: &SqliteSourceFamily,
    evidence: &SqliteFamilyEvidence,
    copy_hooks: (impl FnOnce(), impl FnOnce(&Path)),
    scratch_limit: u64,
    report_progress: &mut impl FnMut(SourceBackedCurrentSourceProgress) -> Result<(), E>,
) -> Result<OnlineBackupSource, SqliteSourceProgressError<E>> {
    let (after_database_copy, after_private_source_copy) = copy_hooks;
    let committed_wal = evidence.wal.as_ref().is_some_and(|state| state.length != 0);
    if !committed_wal {
        #[cfg(target_os = "linux")]
        if immutable_procfd_available(family.database.file()) {
            return Ok(OnlineBackupSource {
                connection: open_immutable_main(&family.database)?,
                copied_source_directory: None,
                copied_source_bytes: 0,
                artifact: SqliteArtifactKind::ProviderDatabase,
            });
        }

        let copied_bytes =
            enforce_snapshot_copy_bounds_with_limit(family, evidence, scratch_limit)?;
        let (snapshot_directory, snapshot_path, integrity) =
            copy_sqlite_family_to_ctx_with_progress(
                data_root,
                family,
                evidence,
                after_database_copy,
                report_progress,
            )?;
        if let Err(error) = snapshot_context.record_source_bytes_copied(copied_bytes) {
            return match close_private_snapshot_directory(
                snapshot_directory,
                SqliteArtifactKind::PrivateSourceCopy,
                0,
                copied_bytes,
            ) {
                Ok(()) => Err(error.into()),
                Err(cleanup) => Err(cleanup.into()),
            };
        }
        after_private_source_copy(&snapshot_path);
        if let Err(error) = certify_private_source_copy(&snapshot_path, &integrity, copied_bytes) {
            return match close_private_snapshot_directory(
                snapshot_directory,
                SqliteArtifactKind::PrivateSourceCopy,
                0,
                copied_bytes,
            ) {
                Ok(()) => Err(error.into()),
                Err(cleanup) => Err(cleanup.into()),
            };
        }
        return finish_copied_online_backup_source(
            snapshot_directory,
            snapshot_path,
            copied_bytes,
            "opening the exact copied logical-backup source",
        )
        .map_err(Into::into);
    }

    let copied_bytes = enforce_snapshot_copy_bounds_with_limit(family, evidence, scratch_limit)?;
    let (snapshot_directory, snapshot_path, integrity) = copy_sqlite_family_to_ctx_with_progress(
        data_root,
        family,
        evidence,
        after_database_copy,
        report_progress,
    )?;
    if let Err(error) = snapshot_context.record_source_bytes_copied(copied_bytes) {
        return match close_private_snapshot_directory(
            snapshot_directory,
            SqliteArtifactKind::PrivateSourceCopy,
            0,
            copied_bytes,
        ) {
            Ok(()) => Err(error
                .with_cleanup_status(SqliteCleanupStatus::Succeeded)
                .into()),
            Err(cleanup) => Err(cleanup.into()),
        };
    }
    after_private_source_copy(&snapshot_path);
    if let Err(error) = certify_private_source_copy(&snapshot_path, &integrity, copied_bytes) {
        return match close_private_snapshot_directory(
            snapshot_directory,
            SqliteArtifactKind::PrivateSourceCopy,
            0,
            copied_bytes,
        ) {
            Ok(()) => Err(error
                .with_cleanup_status(SqliteCleanupStatus::Succeeded)
                .into()),
            Err(cleanup) => Err(cleanup.into()),
        };
    }
    finish_copied_online_backup_source(
        snapshot_directory,
        snapshot_path,
        copied_bytes,
        "opening the ctx-owned online-backup source",
    )
    .map_err(Into::into)
}

fn finish_copied_online_backup_source(
    snapshot_directory: TempDir,
    snapshot_path: PathBuf,
    copied_bytes: u64,
    operation: &'static str,
) -> SqliteSourceAccessResult<OnlineBackupSource> {
    let result = Connection::open_with_flags(
        &snapshot_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|source| {
        sqlite_error(operation, source).with_diagnostic(
            SqliteFailurePhase::SourceValidation,
            SqliteArtifactKind::PrivateSourceCopy,
            0,
            copied_bytes,
            SqliteCleanupStatus::NotRequired,
        )
    });
    match result {
        Ok(connection) => Ok(OnlineBackupSource {
            connection,
            copied_source_directory: Some(snapshot_directory),
            copied_source_bytes: copied_bytes,
            artifact: SqliteArtifactKind::ProviderDatabase,
        }),
        Err(error) => match close_private_snapshot_directory(
            snapshot_directory,
            SqliteArtifactKind::PrivateSourceCopy,
            0,
            copied_bytes,
        ) {
            Ok(()) => Err(error),
            Err(cleanup) => Err(cleanup),
        },
    }
}

pub(super) fn certify_private_source_copy(
    snapshot_path: &Path,
    integrity: &CopiedFamilyIntegrity,
    copied_bytes: u64,
) -> SqliteSourceAccessResult<()> {
    let database_digest = private_snapshot_file_digest(snapshot_path, copied_bytes)?;
    let wal_path = snapshot_path.with_file_name("source.sqlite-wal");
    let wal_digest = if integrity.wal_digest.is_some() {
        Some(private_snapshot_file_digest(&wal_path, copied_bytes)?)
    } else {
        None
    };
    if database_digest == integrity.database_digest && wal_digest == integrity.wal_digest {
        Ok(())
    } else {
        Err(SqliteSourceAccessError::SqliteControl {
            operation: "certifying the exact private SQLite source-family copy",
            code: ffi::SQLITE_CORRUPT,
        }
        .with_diagnostic(
            SqliteFailurePhase::SourceValidation,
            SqliteArtifactKind::PrivateSourceCopy,
            0,
            copied_bytes,
            SqliteCleanupStatus::NotRequired,
        ))
    }
}

fn private_snapshot_file_digest(
    path: &Path,
    copied_bytes: u64,
) -> SqliteSourceAccessResult<[u8; 32]> {
    let mut file = File::open(path).map_err(|source| {
        SqliteSourceAccessError::ScratchIoUnavailable {
            operation: "opening a private SQLite source component for certification",
            path: path.to_path_buf(),
            source,
        }
        .with_diagnostic(
            SqliteFailurePhase::SourceValidation,
            SqliteArtifactKind::PrivateSourceCopy,
            0,
            copied_bytes,
            SqliteCleanupStatus::NotRequired,
        )
    })?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; SQLITE_COPY_BUFFER_BYTES];
    loop {
        let read = file.read(&mut buffer).map_err(|source| {
            SqliteSourceAccessError::ScratchIoUnavailable {
                operation: "reading a private SQLite source component for certification",
                path: path.to_path_buf(),
                source,
            }
            .with_diagnostic(
                SqliteFailurePhase::SourceValidation,
                SqliteArtifactKind::PrivateSourceCopy,
                0,
                copied_bytes,
                SqliteCleanupStatus::NotRequired,
            )
        })?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(digest.finalize().into())
}
