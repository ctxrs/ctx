use super::*;

pub(super) struct AcquiredSqliteConnection {
    pub(super) connection: Connection,
    #[cfg(test)]
    pub(super) strategy: SqliteSourceSnapshotStrategy,
    #[cfg(test)]
    pub(super) copied_bytes: u64,
    pub(super) snapshot_directory: Option<TempDir>,
    pub(super) snapshot_activity: SqliteSourceSnapshotActivity,
}

pub(super) struct CopiedFamilyIntegrity {
    pub(super) database_digest: [u8; 32],
    pub(super) wal_digest: Option<[u8; 32]>,
}

impl AcquiredSqliteConnection {
    pub(super) fn cleanup(self) -> SqliteSourceAccessResult<()> {
        let artifact = if self.snapshot_directory.is_some() {
            SqliteArtifactKind::PrivateSourceCopy
        } else {
            SqliteArtifactKind::PrivateScratch
        };
        let close = close_private_sqlite_connection(
            self.connection,
            "closing a rejected SQLite source snapshot",
            artifact,
            0,
            0,
        );
        let cleanup = self.snapshot_directory.map_or(Ok(()), |directory| {
            close_private_snapshot_directory(directory, artifact, 0, 0)
        });
        drop(self.snapshot_activity);
        match (close, cleanup) {
            (_, Err(error)) | (Err(error), Ok(())) => Err(error),
            (Ok(()), Ok(())) => Ok(()),
        }
    }
}

pub(super) fn acquire_sqlite_connection(
    data_root: &Path,
    snapshot_context: &Arc<SqliteSourceSnapshotContext>,
    family: &SqliteSourceFamily,
    evidence: &SqliteFamilyEvidence,
    after_database_copy: impl FnOnce(),
) -> SqliteSourceAccessResult<AcquiredSqliteConnection> {
    if family.wal.is_none() && family.shared_memory.is_none() {
        #[cfg(target_os = "linux")]
        if immutable_procfd_available(family.database.file()) {
            let connection = open_immutable_main(&family.database)?;
            let snapshot_activity = match snapshot_context
                .record_open(SqliteSourceSnapshotStrategy::ImmutableMain, 0)
            {
                Ok(activity) => activity,
                Err(error) => {
                    return match close_private_sqlite_connection(
                        connection,
                        "closing an untracked immutable SQLite snapshot",
                        SqliteArtifactKind::PrivateScratch,
                        0,
                        0,
                    ) {
                        Ok(()) => Err(error),
                        Err(cleanup) => Err(cleanup),
                    };
                }
            };
            return Ok(AcquiredSqliteConnection {
                connection,
                #[cfg(test)]
                strategy: SqliteSourceSnapshotStrategy::ImmutableMain,
                #[cfg(test)]
                copied_bytes: 0,
                snapshot_directory: None,
                snapshot_activity,
            });
        }
    }

    let copied_bytes = enforce_snapshot_copy_bounds(family, evidence)?;
    let (snapshot_directory, snapshot_path) =
        copy_sqlite_family_to_ctx(data_root, family, evidence, after_database_copy)?;
    let connection = Connection::open_with_flags(
        &snapshot_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|source| sqlite_error("opening the ctx-owned provider snapshot", source));
    let connection = match connection {
        Ok(connection) => connection,
        Err(error) => {
            return match close_private_snapshot_directory(
                snapshot_directory,
                SqliteArtifactKind::PrivateSourceCopy,
                0,
                copied_bytes,
            ) {
                Ok(()) => Err(error),
                Err(cleanup) => Err(cleanup),
            };
        }
    };
    if let Err(error) = snapshot_context.record_source_bytes_copied(copied_bytes) {
        let close = close_private_sqlite_connection(
            connection,
            "closing an unaccounted SQLite source snapshot",
            SqliteArtifactKind::PrivateSourceCopy,
            0,
            copied_bytes,
        );
        let cleanup = close_private_snapshot_directory(
            snapshot_directory,
            SqliteArtifactKind::PrivateSourceCopy,
            0,
            copied_bytes,
        );
        return match (close, cleanup) {
            (_, Err(cleanup)) | (Err(cleanup), Ok(())) => Err(cleanup),
            (Ok(()), Ok(())) => Err(error),
        };
    }
    let snapshot_activity = match snapshot_context
        .record_open(SqliteSourceSnapshotStrategy::CopiedFamily, copied_bytes)
    {
        Ok(activity) => activity,
        Err(error) => {
            let close = close_private_sqlite_connection(
                connection,
                "closing an untracked SQLite source snapshot",
                SqliteArtifactKind::PrivateSourceCopy,
                0,
                copied_bytes,
            );
            let cleanup = close_private_snapshot_directory(
                snapshot_directory,
                SqliteArtifactKind::PrivateSourceCopy,
                0,
                copied_bytes,
            );
            return match (close, cleanup) {
                (_, Err(cleanup)) | (Err(cleanup), Ok(())) => Err(cleanup),
                (Ok(()), Ok(())) => Err(error),
            };
        }
    };
    Ok(AcquiredSqliteConnection {
        connection,
        #[cfg(test)]
        strategy: SqliteSourceSnapshotStrategy::CopiedFamily,
        #[cfg(test)]
        copied_bytes,
        snapshot_directory: Some(snapshot_directory),
        snapshot_activity,
    })
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

pub(super) fn enforce_snapshot_copy_bounds(
    family: &SqliteSourceFamily,
    evidence: &SqliteFamilyEvidence,
) -> SqliteSourceAccessResult<u64> {
    enforce_snapshot_copy_bounds_with_limit(family, evidence, SQLITE_SNAPSHOT_MAX_TOTAL_BYTES)
}

pub(super) fn enforce_snapshot_copy_bounds_with_limit(
    family: &SqliteSourceFamily,
    evidence: &SqliteFamilyEvidence,
    scratch_limit: u64,
) -> SqliteSourceAccessResult<u64> {
    // The family total is authoritative: main and WAL may consume any share,
    // but every observed byte from both members must fit and be copied.
    let mut total = evidence.database.length;
    match (family.wal.as_ref(), evidence.wal.as_ref()) {
        (Some(_wal), Some(state)) => {
            total = total.checked_add(state.length).ok_or_else(|| {
                SqliteSourceAccessError::SnapshotTooLarge {
                    path: family.database.path.clone(),
                    length: u64::MAX,
                    maximum: scratch_limit,
                }
            })?;
        }
        (None, None) => {}
        _ => return Err(SqliteSourceAccessError::SourceChanged),
    }
    if total > scratch_limit {
        return Err(SqliteSourceAccessError::SnapshotTooLarge {
            path: family.database.path.clone(),
            length: total,
            maximum: scratch_limit,
        });
    }
    Ok(total)
}

fn copy_sqlite_family_to_ctx(
    data_root: &Path,
    family: &SqliteSourceFamily,
    evidence: &SqliteFamilyEvidence,
    after_database_copy: impl FnOnce(),
) -> SqliteSourceAccessResult<(TempDir, PathBuf)> {
    match copy_sqlite_family_to_ctx_with_progress(
        data_root,
        family,
        evidence,
        after_database_copy,
        &mut |_| Ok::<(), std::convert::Infallible>(()),
    ) {
        Ok((directory, path, _)) => Ok((directory, path)),
        Err(SqliteSourceProgressError::Source(error)) => Err(error),
        Err(SqliteSourceProgressError::Progress(never)) => match never {},
    }
}

pub(super) fn copy_sqlite_family_to_ctx_with_progress<E>(
    data_root: &Path,
    family: &SqliteSourceFamily,
    evidence: &SqliteFamilyEvidence,
    after_database_copy: impl FnOnce(),
    report_progress: &mut impl FnMut(SourceBackedCurrentSourceProgress) -> Result<(), E>,
) -> Result<(TempDir, PathBuf, CopiedFamilyIntegrity), SqliteSourceProgressError<E>> {
    let total_bytes = enforce_snapshot_copy_bounds(family, evidence)?;
    let mut completed_bytes = 0;
    let mut last_reported_bytes = 0;
    report_source_family_copy_progress(report_progress, completed_bytes, total_bytes)?;
    let directory = create_snapshot_directory(data_root, "provider-sqlite-snapshot-")?;
    let snapshot_path = directory.path().join("source.sqlite");
    let operation = (|| {
        let database_digest = copy_sqlite_member_with_progress(
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
        let wal_digest = match (family.wal.as_ref(), evidence.wal.as_ref()) {
            (Some(wal), Some(state)) => Some(copy_sqlite_member_with_progress(
                wal,
                &directory.path().join("source.sqlite-wal"),
                state.length,
                &mut completed_bytes,
                &mut last_reported_bytes,
                total_bytes,
                report_progress,
            )?),
            (None, None) => None,
            _ => return Err(SqliteSourceAccessError::SourceChanged.into()),
        };
        if completed_bytes != total_bytes {
            return Err(SqliteSourceAccessError::SourceChanged.into());
        }
        family.revalidate(evidence)?;
        Ok(CopiedFamilyIntegrity {
            database_digest,
            wal_digest,
        })
    })();
    let integrity = match operation {
        Ok(integrity) => integrity,
        Err(error) => {
            return match close_private_snapshot_directory(
                directory,
                SqliteArtifactKind::PrivateSourceCopy,
                0,
                completed_bytes,
            ) {
                Ok(()) => Err(error),
                Err(cleanup) => Err(cleanup.into()),
            }
        }
    };
    // SHM is lock coordination, not provider content. Copying it would retain
    // volatile reader marks. Stock SQLite rebuilds it only in this ctx-owned
    // directory from the certified DB/WAL pair.
    Ok((directory, snapshot_path, integrity))
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
    let directory = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(&staging_root)
        .map_err(|source| SqliteSourceAccessError::ScratchIoUnavailable {
            operation: "creating a private provider SQLite snapshot",
            path: staging_root,
            source,
        })?;
    Ok(directory)
}

pub(crate) fn close_private_snapshot_directory(
    directory: TempDir,
    artifact: SqliteArtifactKind,
    copied_pages: u64,
    copied_bytes: u64,
) -> SqliteSourceAccessResult<()> {
    let path = directory.path().to_path_buf();
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

pub(crate) fn close_private_sqlite_connection(
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
