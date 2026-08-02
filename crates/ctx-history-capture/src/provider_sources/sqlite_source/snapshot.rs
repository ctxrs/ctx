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
const SQLITE_SOURCE_FAMILY_COPY_PROGRESS_BYTES: u64 = 8 * 1024 * 1024;

mod scratch;

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
    let family = SqliteSourceFamily::open(authority, database_name, || {})?;
    open_logical_online_backup_snapshot_with_progress(
        authority,
        family,
        || {},
        || {},
        SQLITE_SNAPSHOT_MAX_TOTAL_BYTES,
        report_progress,
    )
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
        policy,
        admitted_revision_is_replay_safe: true,
        #[cfg(test)]
        strategy: acquired.strategy,
        #[cfg(test)]
        copied_bytes: acquired.copied_bytes,
        _snapshot_directory: acquired.snapshot_directory,
        snapshot_activity: Some(acquired.snapshot_activity),
        snapshot_context: Arc::clone(&authority.snapshot_context),
        terminal_fence_slot: Arc::default(),
    })
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
        after_database_copy,
        before_source_revalidation,
        scratch_limit,
        &mut |_| Ok::<(), std::convert::Infallible>(()),
    ) {
        Ok(snapshot) => Ok(snapshot),
        Err(SqliteSourceProgressError::Source(error)) => Err(error),
        Err(SqliteSourceProgressError::Progress(never)) => match never {},
    }
}

fn open_logical_online_backup_snapshot_with_progress<E>(
    authority: &SqliteSourceDirectoryAuthority,
    family: SqliteSourceFamily,
    after_database_copy: impl FnOnce(),
    before_source_revalidation: impl FnOnce(),
    scratch_limit: u64,
    report_progress: &mut impl FnMut(SourceBackedCurrentSourceProgress) -> Result<(), E>,
) -> Result<SqliteSourceReadSnapshot, SqliteSourceProgressError<E>> {
    let opening_evidence = family.capture_revision_evidence()?;
    enforce_snapshot_copy_bounds_with_limit(&family, &opening_evidence, scratch_limit)?;
    let source = acquire_online_backup_source(
        authority.data_root(),
        &authority.snapshot_context,
        &family,
        &opening_evidence,
        after_database_copy,
        scratch_limit,
        report_progress,
    )?;
    verify_connection_read_only(&source.connection)?;
    source
        .connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(|source| {
            sqlite_error(
                "configuring the provider online-backup busy timeout",
                source,
            )
        })?;
    configure_and_pin_snapshot(&source.connection)?;
    before_source_revalidation();

    // Once SQLite has pinned its source view, content changes on the same
    // approved database object are ordinary writer progress. The object and
    // root identities remain authoritative, while this bounded evidence is a
    // conservative routing key for the exact admitted view.
    let (native_evidence, admitted_revision_is_replay_safe) = if source.requires_live_family_fence {
        family.revalidate_logical_identity(&opening_evidence)?;
        let native_evidence = family.capture_revision_evidence()?;
        let replay_safe = opening_evidence.revision_token() == native_evidence.revision_token();
        (native_evidence, replay_safe)
    } else {
        // The exact physical family was fenced around its private copy.
        // Later sidecar lifecycle belongs to the next eventual refresh.
        family.revalidate_logical_database_identity(&opening_evidence)?;
        (opening_evidence.clone(), true)
    };
    let source_sqlite_evidence = capture_sqlite_evidence(&source.connection)?;
    let online_backup_bounds =
        enforce_online_backup_bounds(&source.connection, &family.database.path, scratch_limit)?;

    // A ctx-owned exact family is already the one certified private logical
    // snapshot. Retain that pinned view instead of allocating a second full
    // database through the backup API. Source copies and online backups are
    // therefore mutually exclusive scratch strategies.
    if source.copied_source_directory.is_some() {
        family.revalidate_logical_database_identity(&native_evidence)?;
        let OnlineBackupSource {
            connection,
            copied_source_directory,
            copied_source_bytes,
            ..
        } = source;
        let snapshot_directory = copied_source_directory.ok_or_else(|| {
            SqliteSourceAccessError::SnapshotUnavailable {
                reason: "the certified private SQLite source copy was not retained".to_owned(),
            }
        })?;
        let sqlite_evidence = source_sqlite_evidence;
        family.revalidate_logical_database_identity(&native_evidence)?;
        let evidence = SqliteSourceEvidence::from_snapshot(&native_evidence, &sqlite_evidence);
        let snapshot_activity = authority.snapshot_context.record_open(
            SqliteSourceSnapshotStrategy::LogicalOnlineBackup,
            copied_source_bytes,
        )?;
        return Ok(SqliteSourceReadSnapshot {
            connection: Some(connection),
            family: Some(family),
            native_evidence,
            sqlite_evidence,
            evidence,
            policy: SqliteSourceSnapshotPolicy::LogicalOnlineBackup,
            admitted_revision_is_replay_safe,
            #[cfg(test)]
            strategy: SqliteSourceSnapshotStrategy::LogicalOnlineBackup,
            #[cfg(test)]
            copied_bytes: copied_source_bytes,
            _snapshot_directory: Some(snapshot_directory),
            snapshot_activity: Some(snapshot_activity),
            snapshot_context: Arc::clone(&authority.snapshot_context),
            terminal_fence_slot: Arc::default(),
        });
    }

    let (snapshot_directory, snapshot_path, snapshot_bytes) = online_backup_to_ctx(
        authority.data_root(),
        &source.connection,
        scratch_limit,
        online_backup_bounds,
        report_progress,
    )?;
    family.revalidate_logical_database_identity(&native_evidence)?;
    end_pinned_read_snapshot(&source.connection)?;
    drop(source);

    let connection = Connection::open_with_flags(
        &snapshot_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|source| sqlite_error("opening the private logical SQLite backup", source))?;
    verify_connection_read_only(&connection)?;
    configure_and_pin_snapshot(&connection)?;
    let sqlite_evidence = capture_sqlite_evidence(&connection)?;
    if !sqlite_evidence.same_database_view(&source_sqlite_evidence) {
        return Err(SqliteSourceAccessError::SnapshotUnavailable {
            reason: "the private SQLite backup does not match its pinned source view".to_owned(),
        }
        .into());
    }
    family.revalidate_logical_database_identity(&native_evidence)?;
    let evidence = SqliteSourceEvidence::from_snapshot(&native_evidence, &sqlite_evidence);
    authority
        .snapshot_context
        .record_logical_online_backup_bytes(snapshot_bytes)?;
    let snapshot_activity = authority.snapshot_context.record_open(
        SqliteSourceSnapshotStrategy::LogicalOnlineBackup,
        snapshot_bytes,
    )?;
    Ok(SqliteSourceReadSnapshot {
        connection: Some(connection),
        family: Some(family),
        native_evidence,
        sqlite_evidence,
        evidence,
        policy: SqliteSourceSnapshotPolicy::LogicalOnlineBackup,
        admitted_revision_is_replay_safe,
        #[cfg(test)]
        strategy: SqliteSourceSnapshotStrategy::LogicalOnlineBackup,
        #[cfg(test)]
        copied_bytes: snapshot_bytes,
        _snapshot_directory: Some(snapshot_directory),
        snapshot_activity: Some(snapshot_activity),
        snapshot_context: Arc::clone(&authority.snapshot_context),
        terminal_fence_slot: Arc::default(),
    })
}

struct OnlineBackupSource {
    connection: Connection,
    requires_live_family_fence: bool,
    copied_source_directory: Option<TempDir>,
    copied_source_bytes: u64,
}

fn acquire_online_backup_source<E>(
    data_root: &Path,
    snapshot_context: &Arc<SqliteSourceSnapshotContext>,
    family: &SqliteSourceFamily,
    evidence: &SqliteFamilyEvidence,
    after_database_copy: impl FnOnce(),
    scratch_limit: u64,
    report_progress: &mut impl FnMut(SourceBackedCurrentSourceProgress) -> Result<(), E>,
) -> Result<OnlineBackupSource, SqliteSourceProgressError<E>> {
    let committed_wal = evidence.wal.as_ref().is_some_and(|state| state.length != 0);
    if !committed_wal {
        #[cfg(target_os = "linux")]
        if immutable_procfd_available(family.database.file()) {
            return Ok(OnlineBackupSource {
                connection: open_immutable_main(&family.database)?,
                requires_live_family_fence: false,
                copied_source_directory: None,
                copied_source_bytes: 0,
            });
        }

        let copied_bytes =
            enforce_snapshot_copy_bounds_with_limit(family, evidence, scratch_limit)?;
        let (snapshot_directory, snapshot_path) = copy_sqlite_family_to_ctx_with_progress(
            data_root,
            family,
            evidence,
            after_database_copy,
            report_progress,
        )?;
        snapshot_context.record_source_bytes_copied(copied_bytes)?;
        return Ok(OnlineBackupSource {
            connection: Connection::open_with_flags(
                &snapshot_path,
                OpenFlags::SQLITE_OPEN_READ_ONLY
                    | OpenFlags::SQLITE_OPEN_NO_MUTEX
                    | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE
                    | OpenFlags::SQLITE_OPEN_NOFOLLOW,
            )
            .map_err(|source| {
                sqlite_error("opening the exact copied logical-backup source", source)
            })?,
            requires_live_family_fence: false,
            copied_source_directory: Some(snapshot_directory),
            copied_source_bytes: copied_bytes,
        });
    }

    if committed_wal && family.shared_memory.is_some() {
        let connection = open_live_authorized_source(family)?;
        return Ok(OnlineBackupSource {
            connection,
            requires_live_family_fence: true,
            copied_source_directory: None,
            copied_source_bytes: 0,
        });
    }

    let copied_bytes = enforce_snapshot_copy_bounds_with_limit(family, evidence, scratch_limit)?;
    let (snapshot_directory, snapshot_path) = copy_sqlite_family_to_ctx_with_progress(
        data_root,
        family,
        evidence,
        after_database_copy,
        report_progress,
    )?;
    snapshot_context.record_source_bytes_copied(copied_bytes)?;
    let connection = Connection::open_with_flags(
        &snapshot_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|source| sqlite_error("opening the ctx-owned online-backup source", source))?;
    Ok(OnlineBackupSource {
        connection,
        requires_live_family_fence: false,
        copied_source_directory: Some(snapshot_directory),
        copied_source_bytes: copied_bytes,
    })
}

fn open_live_authorized_source(
    family: &SqliteSourceFamily,
) -> SqliteSourceAccessResult<Connection> {
    // The retained family has already opened every admitted leaf without
    // following links. SQLite repeats that protection across the full named
    // path, after which the retained root and main-object identities are
    // checked around the pinned backup operation.
    Connection::open_with_flags(
        &family.database.path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|source| sqlite_error("opening the retained live provider database", source))
}

#[derive(Clone, Copy)]
struct OnlineBackupBounds {
    page_count: u64,
    page_size: u64,
    bytes: u64,
}

fn enforce_online_backup_bounds(
    connection: &Connection,
    path: &Path,
    scratch_limit: u64,
) -> SqliteSourceAccessResult<OnlineBackupBounds> {
    let page_count: i64 = connection
        .pragma_query_value(None, "page_count", |row| row.get(0))
        .map_err(|source| sqlite_error("reading online-backup page count", source))?;
    let page_size: i64 = connection
        .pragma_query_value(None, "page_size", |row| row.get(0))
        .map_err(|source| sqlite_error("reading online-backup page size", source))?;
    let page_count =
        u64::try_from(page_count).map_err(|_| SqliteSourceAccessError::SnapshotUnavailable {
            reason: "the provider SQLite page count is negative".to_owned(),
        })?;
    let page_size =
        u64::try_from(page_size).map_err(|_| SqliteSourceAccessError::SnapshotUnavailable {
            reason: "the provider SQLite page size is negative".to_owned(),
        })?;
    let bytes = page_count.checked_mul(page_size).ok_or_else(|| {
        SqliteSourceAccessError::SnapshotTooLarge {
            path: path.to_path_buf(),
            length: u64::MAX,
            maximum: scratch_limit,
        }
    })?;
    if bytes > scratch_limit {
        return Err(SqliteSourceAccessError::SnapshotTooLarge {
            path: path.to_path_buf(),
            length: bytes,
            maximum: scratch_limit,
        });
    }
    Ok(OnlineBackupBounds {
        page_count,
        page_size,
        bytes,
    })
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
    .map_err(|source| sqlite_error("creating the private logical SQLite backup", source))?;
    run_online_backup_with_progress(source, &destination, bounds, report_progress)?;
    destination
        .query_row("PRAGMA journal_mode=DELETE", [], |row| {
            row.get::<_, String>(0)
        })
        .map_err(|source| sqlite_error("normalizing private backup journal mode", source))?;
    drop(destination);
    let snapshot_bytes = std::fs::metadata(&snapshot_path)
        .map_err(|source| SqliteSourceAccessError::Io {
            operation: "measuring the private logical SQLite backup",
            path: snapshot_path.clone(),
            source,
        })?
        .len();
    if snapshot_bytes > scratch_limit {
        return Err(SqliteSourceAccessError::SnapshotTooLarge {
            path: snapshot_path,
            length: snapshot_bytes,
            maximum: scratch_limit,
        }
        .into());
    }
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
        .into());
    }
    let mut backup = OnlineBackupHandle(Some(backup));
    let mut completed_pages = 0;
    report_progress(online_backup_progress(0, bounds)?)
        .map_err(SqliteSourceProgressError::Progress)?;
    loop {
        if Instant::now() >= deadline {
            return Err(online_backup_deadline_error().into());
        }
        let code = unsafe {
            ffi::sqlite3_backup_step(backup.pointer(), SQLITE_ONLINE_BACKUP_PAGES_PER_STEP)
        };
        if Instant::now() >= deadline {
            return Err(online_backup_deadline_error().into());
        }
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
                .into());
            }
        }
    }
    if completed_pages != bounds.page_count {
        return Err(SqliteSourceAccessError::SnapshotUnavailable {
            reason: "the logical SQLite backup did not reach its terminal page total".to_owned(),
        }
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

#[cfg(test)]
fn run_online_backup_until(
    source: &Connection,
    destination: &Connection,
    deadline: Instant,
) -> SqliteSourceAccessResult<()> {
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
        });
    }
    let mut backup = OnlineBackupHandle(Some(backup));
    loop {
        if Instant::now() >= deadline {
            return Err(online_backup_deadline_error());
        }
        let code = unsafe {
            ffi::sqlite3_backup_step(backup.pointer(), SQLITE_ONLINE_BACKUP_PAGES_PER_STEP)
        };
        if Instant::now() >= deadline {
            return Err(online_backup_deadline_error());
        }
        match code {
            ffi::SQLITE_DONE => break,
            ffi::SQLITE_OK => continue,
            ffi::SQLITE_BUSY | ffi::SQLITE_LOCKED if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(10));
            }
            code => {
                return Err(SqliteSourceAccessError::SqliteControl {
                    operation: "copying the pinned logical SQLite snapshot",
                    code,
                });
            }
        }
    }
    let code = backup.finish();
    if code == ffi::SQLITE_OK {
        Ok(())
    } else {
        Err(SqliteSourceAccessError::SqliteControl {
            operation: "finishing the logical SQLite online backup",
            code,
        })
    }
}

fn online_backup_deadline_error() -> SqliteSourceAccessError {
    SqliteSourceAccessError::SnapshotUnavailable {
        reason: "the logical SQLite online backup exceeded its five-minute deadline".to_owned(),
    }
}

struct OnlineBackupHandle(Option<*mut ffi::sqlite3_backup>);

impl OnlineBackupHandle {
    fn pointer(&self) -> *mut ffi::sqlite3_backup {
        self.0.unwrap_or(ptr::null_mut())
    }

    fn finish(&mut self) -> i32 {
        self.0.take().map_or(ffi::SQLITE_OK, |backup| unsafe {
            ffi::sqlite3_backup_finish(backup)
        })
    }
}

impl Drop for OnlineBackupHandle {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

fn end_pinned_read_snapshot(connection: &Connection) -> SqliteSourceAccessResult<()> {
    clear_snapshot_authorizer(connection)?;
    connection
        .execute_batch("ROLLBACK")
        .map_err(|source| sqlite_error("ending the provider online-backup snapshot", source))
}

struct AcquiredSqliteConnection {
    connection: Connection,
    #[cfg(test)]
    strategy: SqliteSourceSnapshotStrategy,
    #[cfg(test)]
    copied_bytes: u64,
    snapshot_directory: Option<TempDir>,
    snapshot_activity: SqliteSourceSnapshotActivity,
}

fn acquire_sqlite_connection(
    data_root: &Path,
    snapshot_context: &Arc<SqliteSourceSnapshotContext>,
    family: &SqliteSourceFamily,
    evidence: &SqliteFamilyEvidence,
    after_database_copy: impl FnOnce(),
) -> SqliteSourceAccessResult<AcquiredSqliteConnection> {
    if family.wal.is_none() && family.shared_memory.is_none() {
        #[cfg(target_os = "linux")]
        if immutable_procfd_available(family.database.file()) {
            return Ok(AcquiredSqliteConnection {
                connection: open_immutable_main(&family.database)?,
                #[cfg(test)]
                strategy: SqliteSourceSnapshotStrategy::ImmutableMain,
                #[cfg(test)]
                copied_bytes: 0,
                snapshot_directory: None,
                snapshot_activity: snapshot_context
                    .record_open(SqliteSourceSnapshotStrategy::ImmutableMain, 0)?,
            });
        }
    }

    let copied_bytes = enforce_snapshot_copy_bounds(family, evidence)?;
    let (snapshot_directory, snapshot_path) =
        copy_sqlite_family_to_ctx(data_root, family, evidence, after_database_copy)?;
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
        #[cfg(test)]
        strategy: SqliteSourceSnapshotStrategy::CopiedFamily,
        #[cfg(test)]
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
) -> SqliteSourceAccessResult<u64> {
    enforce_snapshot_copy_bounds_with_limit(family, evidence, SQLITE_SNAPSHOT_MAX_TOTAL_BYTES)
}

fn enforce_snapshot_copy_bounds_with_limit(
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
        Ok(snapshot) => Ok(snapshot),
        Err(SqliteSourceProgressError::Source(error)) => Err(error),
        Err(SqliteSourceProgressError::Progress(never)) => match never {},
    }
}

fn copy_sqlite_family_to_ctx_with_progress<E>(
    data_root: &Path,
    family: &SqliteSourceFamily,
    evidence: &SqliteFamilyEvidence,
    after_database_copy: impl FnOnce(),
    report_progress: &mut impl FnMut(SourceBackedCurrentSourceProgress) -> Result<(), E>,
) -> Result<(TempDir, PathBuf), SqliteSourceProgressError<E>> {
    let total_bytes = enforce_snapshot_copy_bounds(family, evidence)?;
    let mut completed_bytes = 0;
    let mut last_reported_bytes = 0;
    report_source_family_copy_progress(report_progress, completed_bytes, total_bytes)?;
    let directory = create_snapshot_directory(data_root, "provider-sqlite-snapshot-")?;
    let snapshot_path = directory.path().join("source.sqlite");
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
    // SHM is lock coordination, not provider content. Copying it would retain
    // volatile reader marks. Stock SQLite rebuilds it only in this ctx-owned
    // directory from the certified DB/WAL pair.
    Ok((directory, snapshot_path))
}

fn create_snapshot_directory(data_root: &Path, prefix: &str) -> SqliteSourceAccessResult<TempDir> {
    let staging_root = data_root.join("tmp").join("provider-sqlite");
    create_private_directory_all(&staging_root).map_err(|source| SqliteSourceAccessError::Io {
        operation: "creating the private provider SQLite staging root",
        path: staging_root.clone(),
        source,
    })?;
    let directory = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(&staging_root)
        .map_err(|source| SqliteSourceAccessError::Io {
            operation: "creating a private provider SQLite snapshot",
            path: staging_root,
            source,
        })?;
    Ok(directory)
}

fn copy_sqlite_member_with_progress<E>(
    member: &SqliteFamilyMember,
    destination: &Path,
    expected_length: u64,
    completed_bytes: &mut u64,
    last_reported_bytes: &mut u64,
    total_bytes: u64,
    report_progress: &mut impl FnMut(SourceBackedCurrentSourceProgress) -> Result<(), E>,
) -> Result<(), SqliteSourceProgressError<E>> {
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
            return Err(SqliteSourceAccessError::SourceChanged.into());
        }
        destination_file
            .write_all(&buffer[..read])
            .map_err(|source| SqliteSourceAccessError::Io {
                operation: "writing a ctx-owned SQLite snapshot component",
                path: destination.to_path_buf(),
                source,
            })?;
        remaining -= read as u64;
        *completed_bytes = completed_bytes.checked_add(read as u64).ok_or_else(|| {
            SqliteSourceAccessError::SnapshotUnavailable {
                reason: "the SQLite source-family copy progress count overflowed".to_owned(),
            }
        })?;
        if *completed_bytes == total_bytes
            || completed_bytes.saturating_sub(*last_reported_bytes)
                >= SQLITE_SOURCE_FAMILY_COPY_PROGRESS_BYTES
        {
            report_source_family_copy_progress(report_progress, *completed_bytes, total_bytes)?;
            *last_reported_bytes = *completed_bytes;
        }
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
        return Err(SqliteSourceAccessError::SourceChanged.into());
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

fn report_source_family_copy_progress<E>(
    report_progress: &mut impl FnMut(SourceBackedCurrentSourceProgress) -> Result<(), E>,
    completed_bytes: u64,
    total_bytes: u64,
) -> Result<(), SqliteSourceProgressError<E>> {
    let mut progress = SourceBackedCurrentSourceProgress::new(
        SourceBackedCurrentSourceProgressStage::SourceFamilyCopy,
    );
    progress.snapshot_bytes_completed = Some(completed_bytes);
    progress.snapshot_bytes_total = Some(total_bytes);
    report_progress(progress).map_err(SqliteSourceProgressError::Progress)
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
        SqliteSourceSnapshotPolicy::StrictPhysicalFamily,
        || {},
        || {},
        before_sqlite_open,
    )
}

#[cfg(test)]
pub(super) fn open_root_handle_sqlite_source_snapshot_after_database_copy_for_test(
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

#[cfg(test)]
pub(super) fn open_root_handle_sqlite_source_snapshot_after_parent_certification_for_test(
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

#[cfg(test)]
pub(super) fn open_root_handle_sqlite_source_online_backup_before_identity_check_for_test(
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

#[cfg(test)]
pub(super) fn open_root_handle_sqlite_source_online_backup_after_database_copy_for_test(
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

#[cfg(test)]
pub(super) fn run_online_backup_with_deadline_for_test(
    source: &Connection,
    destination: &Connection,
    deadline: Instant,
) -> SqliteSourceAccessResult<()> {
    run_online_backup_until(source, destination, deadline)
}

#[cfg(test)]
pub(super) fn open_root_handle_sqlite_source_online_backup_with_scratch_limit_for_test(
    authority: &SqliteSourceDirectoryAuthority,
    database_name: &OsStr,
    scratch_limit: u64,
) -> SqliteSourceAccessResult<SqliteSourceReadSnapshot> {
    let family = SqliteSourceFamily::open(authority, database_name, || {})?;
    open_logical_online_backup_snapshot(authority, family, || {}, || {}, scratch_limit)
}

#[cfg(test)]
pub(super) fn certify_root_handle_sqlite_source_snapshot_copy_budget_for_test(
    authority: &SqliteSourceDirectoryAuthority,
    database_name: &OsStr,
) -> SqliteSourceAccessResult<u64> {
    let family = SqliteSourceFamily::open(authority, database_name, || {})?;
    let evidence = family.capture_evidence()?;
    let copied_bytes = enforce_snapshot_copy_bounds(&family, &evidence)?;
    family.revalidate(&evidence)?;
    Ok(copied_bytes)
}
