use super::*;

/// A pinned read-only SQLite view over a snapshot already owned by ctx.
///
/// The caller is responsible for proving that `path` lives in private
/// ctx-owned storage. SQLite may create or update sidecars beside this path;
/// it can never reach the provider directory through this API.
#[must_use = "call finish() after complete-content queries"]
#[derive(Debug)]
pub(crate) struct CtxOwnedSqliteReadSnapshot {
    connection: Option<Connection>,
}

impl CtxOwnedSqliteReadSnapshot {
    pub(crate) fn finish(mut self) -> SqliteSourceAccessResult<()> {
        let connection = self
            .connection
            .as_ref()
            .ok_or(SqliteSourceAccessError::SnapshotNotActive)?;
        clear_snapshot_authorizer(connection)?;
        connection
            .execute_batch("ROLLBACK")
            .map_err(|source| sqlite_error("ending the ctx-owned SQLite snapshot", source))?;
        self.connection.take();
        Ok(())
    }
}

impl Deref for CtxOwnedSqliteReadSnapshot {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        match self.connection.as_ref() {
            Some(connection) => connection,
            None => std::process::abort(),
        }
    }
}

impl Drop for CtxOwnedSqliteReadSnapshot {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.as_ref() {
            let _ = clear_snapshot_authorizer(connection);
            let _ = connection.execute_batch("ROLLBACK");
        }
        self.connection.take();
    }
}

pub(crate) fn open_ctx_owned_sqlite_read_snapshot(
    path: &Path,
) -> SqliteSourceAccessResult<CtxOwnedSqliteReadSnapshot> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|source| sqlite_error("opening a ctx-owned SQLite snapshot", source))?;
    verify_connection_read_only(&connection)?;
    configure_and_pin_snapshot(&connection)?;
    Ok(CtxOwnedSqliteReadSnapshot {
        connection: Some(connection),
    })
}

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
    open_root_handle_sqlite_source_snapshot_inner(authority, database_name, || {}, || {})
}

pub(crate) fn open_root_handle_sqlite_source_physical_revision(
    authority: &SqliteSourceDirectoryAuthority,
    database_name: &OsStr,
) -> SqliteSourceAccessResult<SqliteSourcePhysicalRevision> {
    SqliteSourcePhysicalRevision::open(authority, database_name)
}

fn open_root_handle_sqlite_source_snapshot_inner(
    authority: &SqliteSourceDirectoryAuthority,
    database_name: &OsStr,
    after_parent_certification: impl FnOnce(),
    before_source_revalidation: impl FnOnce(),
) -> SqliteSourceAccessResult<SqliteSourceReadSnapshot> {
    let family = SqliteSourceFamily::open(authority, database_name, after_parent_certification)?;
    let (native_evidence, database_bytes_read, wal_bytes_read) = family.capture_evidence()?;
    authority
        .snapshot_context
        .record_physical_revision_capture(database_bytes_read, wal_bytes_read)?;

    let acquired = acquire_sqlite_connection(
        authority.data_root(),
        &authority.snapshot_context,
        &family,
        &native_evidence,
    )?;
    verify_connection_read_only(&acquired.connection)?;
    configure_and_pin_snapshot(&acquired.connection)?;
    before_source_revalidation();

    // The source family is checked only after SQLite has pinned the selected
    // view. No provider observation may escape if acquisition raced a commit,
    // rewrite, truncation, replacement, or sidecar transition.
    family.revalidate(&native_evidence)?;
    let sqlite_evidence = capture_sqlite_evidence(&acquired.connection)?;
    family.revalidate(&native_evidence)?;
    let evidence = SqliteSourceEvidence::from_snapshot(&native_evidence, &sqlite_evidence);
    Ok(SqliteSourceReadSnapshot {
        connection: Some(acquired.connection),
        family: Some(family),
        native_evidence,
        sqlite_evidence,
        evidence,
        strategy: acquired.strategy,
        copied_bytes: acquired.copied_bytes,
        _snapshot_directory: acquired.snapshot_directory,
        snapshot_activity: Some(acquired.snapshot_activity),
        snapshot_context: Arc::clone(&authority.snapshot_context),
        terminal_fence_slot: Arc::default(),
    })
}

struct AcquiredSqliteConnection {
    connection: Connection,
    strategy: SqliteSourceSnapshotStrategy,
    copied_bytes: u64,
    snapshot_directory: Option<TempDir>,
    snapshot_activity: SqliteSourceSnapshotActivity,
}

fn acquire_sqlite_connection(
    data_root: &Path,
    snapshot_context: &Arc<SqliteSourceSnapshotContext>,
    family: &SqliteSourceFamily,
    evidence: &SqliteFamilyEvidence,
) -> SqliteSourceAccessResult<AcquiredSqliteConnection> {
    if family.wal.is_none() && family.shared_memory.is_none() {
        #[cfg(target_os = "linux")]
        if immutable_procfd_available(family.database.file()) {
            return Ok(AcquiredSqliteConnection {
                connection: open_immutable_main(&family.database)?,
                strategy: SqliteSourceSnapshotStrategy::ImmutableMain,
                copied_bytes: 0,
                snapshot_directory: None,
                snapshot_activity: snapshot_context
                    .record_open(SqliteSourceSnapshotStrategy::ImmutableMain, 0)?,
            });
        }
    }

    enforce_snapshot_copy_bounds(family, evidence)?;
    let (snapshot_directory, snapshot_path, copied_bytes) =
        copy_sqlite_family_to_ctx(data_root, family, evidence)?;
    snapshot_context.record_source_bytes_copied(copied_bytes)?;
    let connection = Connection::open_with_flags(
        &snapshot_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|source| sqlite_error("opening the ctx-owned provider snapshot", source))?;
    Ok(AcquiredSqliteConnection {
        connection,
        strategy: SqliteSourceSnapshotStrategy::CopiedFamily,
        copied_bytes,
        snapshot_directory: Some(snapshot_directory),
        snapshot_activity: snapshot_context
            .record_open(SqliteSourceSnapshotStrategy::CopiedFamily, copied_bytes)?,
    })
}

#[cfg(target_os = "linux")]
fn immutable_procfd_available(database: &File) -> bool {
    PathBuf::from(format!("/proc/self/fd/{}", database.as_raw_fd())).exists()
}

#[cfg(target_os = "linux")]
fn open_immutable_main(database: &SqliteFamilyMember) -> SqliteSourceAccessResult<Connection> {
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

fn enforce_snapshot_copy_bounds(
    family: &SqliteSourceFamily,
    evidence: &SqliteFamilyEvidence,
) -> SqliteSourceAccessResult<()> {
    let mut total = bounded_snapshot_component(&family.database.path, evidence.database.length)?;
    if let (Some(wal), Some(state)) = (family.wal.as_ref(), evidence.wal.as_ref()) {
        total = total
            .checked_add(bounded_snapshot_component(&wal.path, state.length)?)
            .ok_or_else(|| SqliteSourceAccessError::SnapshotTooLarge {
                path: family.database.path.clone(),
                length: u64::MAX,
                maximum: SQLITE_SNAPSHOT_MAX_TOTAL_BYTES,
            })?;
    }
    if total > SQLITE_SNAPSHOT_MAX_TOTAL_BYTES {
        return Err(SqliteSourceAccessError::SnapshotTooLarge {
            path: family.database.path.clone(),
            length: total,
            maximum: SQLITE_SNAPSHOT_MAX_TOTAL_BYTES,
        });
    }
    Ok(())
}

fn bounded_snapshot_component(path: &Path, length: u64) -> SqliteSourceAccessResult<u64> {
    if length > SQLITE_SNAPSHOT_MAX_COMPONENT_BYTES {
        Err(SqliteSourceAccessError::SnapshotTooLarge {
            path: path.to_path_buf(),
            length,
            maximum: SQLITE_SNAPSHOT_MAX_COMPONENT_BYTES,
        })
    } else {
        Ok(length)
    }
}

fn copy_sqlite_family_to_ctx(
    data_root: &Path,
    family: &SqliteSourceFamily,
    evidence: &SqliteFamilyEvidence,
) -> SqliteSourceAccessResult<(TempDir, PathBuf, u64)> {
    let staging_root = data_root.join("tmp").join("provider-sqlite");
    create_private_directory_all(&staging_root).map_err(|source| SqliteSourceAccessError::Io {
        operation: "creating the private provider SQLite staging root",
        path: staging_root.clone(),
        source,
    })?;
    let directory = tempfile::Builder::new()
        .prefix("provider-sqlite-snapshot-")
        .tempdir_in(&staging_root)
        .map_err(|source| SqliteSourceAccessError::Io {
            operation: "creating a private provider SQLite snapshot",
            path: staging_root,
            source,
        })?;
    let snapshot_path = directory.path().join("source.sqlite");
    copy_sqlite_member(&family.database, &snapshot_path, evidence.database.length)?;
    let mut copied_bytes = evidence.database.length;
    if let (Some(wal), Some(state)) = (family.wal.as_ref(), evidence.wal.as_ref()) {
        copy_sqlite_member(
            wal,
            &directory.path().join("source.sqlite-wal"),
            state.length,
        )?;
        copied_bytes = copied_bytes
            .checked_add(state.length)
            .ok_or(SqliteSourceAccessError::SourceChanged)?;
    }
    // SHM is lock coordination, not provider content. Copying it would retain
    // volatile reader marks. Stock SQLite rebuilds it only in this ctx-owned
    // directory from the certified DB/WAL pair.
    Ok((directory, snapshot_path, copied_bytes))
}

fn copy_sqlite_member(
    member: &SqliteFamilyMember,
    destination: &Path,
    expected_length: u64,
) -> SqliteSourceAccessResult<()> {
    let mut source_file =
        member
            .file()
            .try_clone()
            .map_err(|source| SqliteSourceAccessError::Io {
                operation: "retaining a provider SQLite component for snapshot copy",
                path: member.path.clone(),
                source,
            })?;
    source_file
        .seek(SeekFrom::Start(0))
        .map_err(|source| SqliteSourceAccessError::Io {
            operation: "seeking a provider SQLite component for snapshot copy",
            path: member.path.clone(),
            source,
        })?;
    let mut destination_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|source| SqliteSourceAccessError::Io {
            operation: "creating a ctx-owned SQLite snapshot component",
            path: destination.to_path_buf(),
            source,
        })?;
    let mut remaining = expected_length;
    let mut buffer = [0_u8; SQLITE_COPY_BUFFER_BYTES];
    while remaining > 0 {
        let requested = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| SqliteSourceAccessError::SourceChanged)?;
        let read = source_file
            .read(&mut buffer[..requested])
            .map_err(|source| SqliteSourceAccessError::Io {
                operation: "reading a provider SQLite snapshot component",
                path: member.path.clone(),
                source,
            })?;
        if read == 0 {
            return Err(SqliteSourceAccessError::SourceChanged);
        }
        destination_file
            .write_all(&buffer[..read])
            .map_err(|source| SqliteSourceAccessError::Io {
                operation: "writing a ctx-owned SQLite snapshot component",
                path: destination.to_path_buf(),
                source,
            })?;
        remaining -= read as u64;
    }
    let mut extra = [0_u8; 1];
    if source_file
        .read(&mut extra)
        .map_err(|source| SqliteSourceAccessError::Io {
            operation: "certifying a provider SQLite snapshot component length",
            path: member.path.clone(),
            source,
        })?
        != 0
    {
        return Err(SqliteSourceAccessError::SourceChanged);
    }
    destination_file
        .flush()
        .map_err(|source| SqliteSourceAccessError::Io {
            operation: "flushing a ctx-owned SQLite snapshot component",
            path: destination.to_path_buf(),
            source,
        })?;
    Ok(())
}

#[cfg(test)]
pub(super) fn open_root_handle_sqlite_source_snapshot_for_test(
    authority: &SqliteSourceDirectoryAuthority,
    database_name: &OsStr,
    before_sqlite_open: impl FnOnce(),
) -> SqliteSourceAccessResult<SqliteSourceReadSnapshot> {
    open_root_handle_sqlite_source_snapshot_inner(
        authority,
        database_name,
        || {},
        before_sqlite_open,
    )
}

#[cfg(test)]
pub(super) fn open_root_handle_sqlite_source_snapshot_after_parent_certification_for_test(
    authority: &SqliteSourceDirectoryAuthority,
    database_name: &OsStr,
    after_parent_certification: impl FnOnce(),
) -> SqliteSourceAccessResult<SqliteSourceReadSnapshot> {
    open_root_handle_sqlite_source_snapshot_inner(
        authority,
        database_name,
        after_parent_certification,
        || {},
    )
}
