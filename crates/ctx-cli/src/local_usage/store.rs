use std::{
    ffi::OsString,
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime},
};

use chrono::{DateTime, Days, Utc};
use ctx_history_core::platform_security::{
    establish_private_data_root, restrict_private_file_handle, verify_private_file,
};
#[cfg(test)]
use rusqlite::params;
use rusqlite::{Connection, OpenFlags, TransactionBehavior};

use super::{CompletedOperation, CTX_VERSION, RETENTION_DAYS};

mod file_family;
mod migration;
mod write;

#[cfg(all(test, windows))]
pub(super) use file_family::{assert_single_link_for_test, verify_same_file_for_test};
use file_family::{
    capture_checkpointed_image, deserialize_read_only, open_nofollow, preflight_auxiliaries,
    preflight_existing_family, protect_sqlite_files, reopen_same_file, verify_file_owner,
    verify_metadata_owner, verify_private_directory_and_owner, verify_same_file,
    verify_single_link, FamilyGuard,
};
pub(crate) use migration::verify_report_dates;
use migration::{initialize_schema, migrate_to_current, reject_future_daily_dates, verify_schema};
#[cfg(test)]
use migration::{legacy_daily_usage_schema_v1, DAILY_USAGE_SCHEMA_V1, LEGACY_MAINTENANCE_SCHEMA};
pub(super) use migration::{v1_uses_legacy_blame_schema, verify_supported_schema};
use write::record_at as write_record_at;

pub(crate) const USAGE_FILE: &str = "usage.sqlite";
const APPLICATION_ID: i64 = 0x4354_5855;
const LEGACY_SCHEMA_VERSION: i64 = 1;
const SCHEMA_VERSION: i64 = 2;
const BUSY_TIMEOUT: Duration = Duration::from_millis(25);
const PAGE_SIZE_BYTES: i64 = 4 * 1024;
const MAX_DATABASE_BYTES: i64 = 6 * 1024 * 1024;
const MAX_PAGE_COUNT: i64 = MAX_DATABASE_BYTES / PAGE_SIZE_BYTES;
const WAL_AUTOCHECKPOINT_PAGES: i64 = 64;
const JOURNAL_SIZE_LIMIT_BYTES: i64 = 1024 * 1024;
const STALE_INIT_AGE: Duration = Duration::from_secs(60 * 60);
const INIT_SLOT_COUNT: usize = 8;

#[derive(Debug, thiserror::Error)]
pub(crate) enum UsageStoreError {
    #[error("usage store I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("usage store SQLite error: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("usage store has an unsupported application ID")]
    ApplicationId,
    #[error("usage store has unsupported schema version {0}")]
    SchemaVersion(i64),
    #[error("usage store schema does not match its declared version")]
    SchemaIdentity,
    #[error("usage store exceeds its size limit")]
    GrowthLimit,
    #[error("usage store contains inconsistent aggregates")]
    Integrity,
    #[error("usage store date is ahead of the current UTC day")]
    FutureDate,
    #[error("usage store cannot be reported without changing its SQLite file family")]
    UnsafeReadState,
}

impl UsageStoreError {
    pub(crate) const fn public_message(&self) -> &'static str {
        match self {
            Self::ApplicationId
            | Self::SchemaVersion(_)
            | Self::SchemaIdentity
            | Self::Integrity => "local usage store format is not supported",
            Self::FutureDate => "local usage store date is ahead of the current UTC day",
            Self::GrowthLimit => "local usage store exceeds its size limit",
            Self::Io(_) | Self::Sql(_) | Self::UnsafeReadState => {
                "local usage store could not be read"
            }
        }
    }
}

pub(crate) fn usage_path(data_root: &Path) -> PathBuf {
    data_root.join(USAGE_FILE)
}

pub(crate) fn usage_store_exists(data_root: &Path) -> Result<bool, UsageStoreError> {
    let path = usage_path(data_root);
    let Some(parent) = path.parent() else {
        return Err(UsageStoreError::SchemaIdentity);
    };
    match parent.symlink_metadata() {
        Ok(_) => verify_private_directory_and_owner(parent)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    }
    match path.symlink_metadata() {
        Ok(_) => {
            let _guard = preflight_existing_family(&path, true)?;
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            preflight_auxiliaries(&path, false)?;
            Ok(false)
        }
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn record(
    data_root: &Path,
    operation: CompletedOperation,
) -> Result<(), UsageStoreError> {
    record_at(data_root, operation, SystemTime::now(), BUSY_TIMEOUT)
}

fn record_at(
    data_root: &Path,
    operation: CompletedOperation,
    now: SystemTime,
    busy_timeout: Duration,
) -> Result<(), UsageStoreError> {
    record_at_with_ctx_version(data_root, operation, now, busy_timeout, CTX_VERSION)
}

fn record_at_with_ctx_version(
    data_root: &Path,
    operation: CompletedOperation,
    now: SystemTime,
    busy_timeout: Duration,
    ctx_version: &str,
) -> Result<(), UsageStoreError> {
    write_record_at(data_root, operation, now, busy_timeout, ctx_version)
}

#[cfg(test)]
pub(super) fn record_at_for_test(
    data_root: &Path,
    operation: CompletedOperation,
    now: SystemTime,
    busy_timeout: Duration,
) -> Result<(), UsageStoreError> {
    record_at(data_root, operation, now, busy_timeout)
}

#[cfg(test)]
pub(super) fn create_mixed_v1_fixture_for_test(data_root: &Path) -> Result<(), UsageStoreError> {
    create_v1_fixture_for_test(data_root, DAILY_USAGE_SCHEMA_V1, "none")
}

#[cfg(test)]
pub(super) fn create_legacy_impossible_blame_v1_fixture_for_test(
    data_root: &Path,
) -> Result<(), UsageStoreError> {
    let legacy_schema = legacy_daily_usage_schema_v1();
    create_v1_fixture_for_test(data_root, &legacy_schema, "possible")
}

#[cfg(test)]
fn create_v1_fixture_for_test(
    data_root: &Path,
    daily_schema: &str,
    empty_blame_outcome: &str,
) -> Result<(), UsageStoreError> {
    establish_private_data_root(data_root)?;
    verify_private_directory_and_owner(data_root)?;
    let path = usage_path(data_root);
    if path.exists() {
        return Err(UsageStoreError::SchemaIdentity);
    }
    let mut conn = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    conn.pragma_update(None, "page_size", PAGE_SIZE_BYTES)?;
    let day = utc_day(SystemTime::now());
    let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute_batch(daily_schema)?;
    transaction.execute_batch(LEGACY_MAINTENANCE_SCHEMA)?;
    let insert = r#"
        INSERT INTO daily_usage (
            day_utc, definition_version, ctx_version, surface, operation,
            outcome, value_class, duration_bucket, target_type, pro_outcome,
            calls, result_count, citation_count, response_bytes
        ) VALUES (
            ?1, 1, '0.25.0-legacy', ?2, ?3, ?4, ?5, ?6, ?7, ?8,
            ?9, ?10, ?11, ?12
        )
    "#;
    for row in [
        (
            "cli",
            "doctor",
            "success",
            "not_applicable",
            "under_10_ms",
            "not_applicable",
            "not_applicable",
            2_i64,
            0_i64,
            0_i64,
            0_i64,
        ),
        (
            "mcp",
            "search",
            "success",
            "result_bearing",
            "50_to_249_ms",
            "not_applicable",
            "not_applicable",
            3,
            6,
            0,
            900,
        ),
        (
            "mcp",
            "show_session",
            "success",
            "result_bearing",
            "10_to_49_ms",
            "not_applicable",
            "not_applicable",
            1,
            2,
            0,
            300,
        ),
        (
            "cli",
            "blame",
            "success",
            "result_bearing",
            "250_to_999_ms",
            "commit",
            "produced",
            1,
            1,
            1,
            0,
        ),
        (
            "mcp",
            "blame",
            "success",
            "empty",
            "250_to_999_ms",
            "file",
            empty_blame_outcome,
            1,
            0,
            0,
            200,
        ),
        (
            "mcp",
            "search",
            "failure",
            "not_applicable",
            "10_to_49_ms",
            "not_applicable",
            "not_applicable",
            1,
            0,
            0,
            100,
        ),
    ] {
        transaction.execute(
            insert,
            params![
                day, row.0, row.1, row.2, row.3, row.4, row.5, row.6, row.7, row.8, row.9, row.10
            ],
        )?;
    }
    transaction.execute(
        "INSERT INTO maintenance(singleton, last_retention_day) VALUES (1, ?1)",
        [day],
    )?;
    transaction.pragma_update(None, "application_id", APPLICATION_ID)?;
    transaction.pragma_update(None, "user_version", LEGACY_SCHEMA_VERSION)?;
    transaction.commit()?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
        ))
    });
    drop(conn);
    protect_sqlite_files(&path)?;
    Ok(())
}

#[cfg(test)]
pub(super) fn fail_v1_migration_before_commit_for_test(
    data_root: &Path,
) -> Result<(), UsageStoreError> {
    let path = usage_path(data_root);
    open_writable_with_migration_hook(&path, false, BUSY_TIMEOUT, || {
        Err(UsageStoreError::Integrity)
    })
    .map(|_| ())
}

pub(crate) struct ReadOnlyStore {
    conn: Connection,
    family_guard: FamilyGuard,
    path: PathBuf,
}

impl ReadOnlyStore {
    pub(crate) fn connection_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    pub(crate) fn verify_unchanged(&self) -> Result<(), UsageStoreError> {
        self.family_guard.recheck_unchanged(&self.path)
    }
}

pub(crate) fn open_read_only(path: &Path) -> Result<ReadOnlyStore, UsageStoreError> {
    let guard = preflight_existing_family(path, true)?;
    if guard.has_nonempty_auxiliary()? {
        return Err(UsageStoreError::UnsafeReadState);
    }
    let image = capture_checkpointed_image(path, &guard, || {})?;
    let mut conn = deserialize_read_only(image)?;
    migrate_to_current(&mut conn, || Ok(()))?;
    configure_report_connection(&conn)?;
    guard.recheck_unchanged(path)?;
    Ok(ReadOnlyStore {
        conn,
        family_guard: guard,
        path: path.to_path_buf(),
    })
}

pub(crate) fn reset(data_root: &Path) -> Result<bool, UsageStoreError> {
    reset_with_post_commit(data_root, |_| ())
}

fn reset_with_post_commit<T>(
    data_root: &Path,
    after_commit: impl FnOnce(&Path) -> T,
) -> Result<bool, UsageStoreError> {
    let path = usage_path(data_root);
    let Some(WritableStore {
        mut conn,
        family_guard,
    }) = open_writable(&path, false, BUSY_TIMEOUT)?
    else {
        return Ok(false);
    };
    let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    verify_schema(&transaction)?;
    super::report::validate_rows(&transaction)?;
    transaction.execute("DELETE FROM daily_usage", [])?;
    transaction.execute("DELETE FROM maintenance", [])?;
    family_guard.recheck(&path)?;
    let commit_guard = preflight_existing_family(&path, true)?;
    verify_schema(&transaction)?;
    super::report::validate_rows(&transaction)?;
    transaction.commit()?;
    drop(commit_guard);
    let _post_commit_guard = after_commit(&path);
    // Reset promises logical deletion, not forensic erasure. Truncate the WAL
    // when no reader prevents it, but do not turn a completed logical reset
    // into an error if this best-effort checkpoint is busy.
    let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
        ))
    });
    let _ = protect_sqlite_files(&path);
    Ok(true)
}

enum PreparedFile {
    Missing,
    NewInitialized(FamilyGuard),
    Existing(FamilyGuard),
}

struct WritableStore {
    conn: Connection,
    family_guard: FamilyGuard,
}

fn open_writable(
    path: &Path,
    create: bool,
    busy_timeout: Duration,
) -> Result<Option<WritableStore>, UsageStoreError> {
    open_writable_with_migration_hook(path, create, busy_timeout, || Ok(()))
}

fn open_writable_with_migration_hook(
    path: &Path,
    create: bool,
    busy_timeout: Duration,
    before_migration_commit: impl FnOnce() -> Result<(), UsageStoreError>,
) -> Result<Option<WritableStore>, UsageStoreError> {
    let prepared = prepare_file(path, create)?;
    let newly_created = matches!(prepared, PreparedFile::NewInitialized(_));
    let guard = match prepared {
        PreparedFile::Missing => return Ok(None),
        PreparedFile::NewInitialized(guard) | PreparedFile::Existing(guard) => guard,
    };
    if !newly_created {
        // A nonempty WAL may contain changes absent from the main image, while
        // a nonempty SHM cannot be proven source-stable portably. Reject either
        // from retained native handles before SQLite can open the source
        // pathname and checkpoint or remove any family member.
        if guard.has_nonempty_auxiliary()? {
            return Err(UsageStoreError::UnsafeReadState);
        }
        let image = capture_checkpointed_image(path, &guard, || {})?;
        let detached = deserialize_read_only(image)?;
        let schema_version = verify_supported_schema(&detached)?;
        super::report::validate_rows_for_schema(&detached, schema_version)?;
        drop(detached);
        guard.recheck_unchanged(path)?;
        cleanup_stale_initializer_slots(path, SystemTime::now())?;
        guard.recheck_unchanged(path)?;
    }
    let mut flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    if newly_created {
        flags |= OpenFlags::SQLITE_OPEN_CREATE;
    }
    let mut conn = Connection::open_with_flags(path, flags)?;
    verify_same_file(path, guard.main_file())?;
    verify_single_link(guard.main_file())?;
    let schema_version = verify_supported_schema(&conn)?;
    super::report::validate_rows_for_schema(&conn, schema_version)?;
    configure_transient(&conn, busy_timeout)?;
    if schema_version == LEGACY_SCHEMA_VERSION {
        // A quiescent v1 store can have a WAL-mode main header without
        // auxiliaries. Opening it natively creates fresh WAL/SHM files, which
        // cannot be part of the pre-open family guard. Return to rollback
        // journal mode before migration so the guarded family is main-only
        // again; v2 persistent configuration restores WAL after commit.
        let journal_mode: String =
            conn.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
        match journal_mode.as_str() {
            "wal" => {
                conn.pragma_update(None, "journal_mode", "DELETE")?;
                let journal_mode: String =
                    conn.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
                if journal_mode != "delete" {
                    return Err(UsageStoreError::UnsafeReadState);
                }
            }
            // The WAL-to-rollback transition is durable independently of the
            // schema transaction. A prior attempt can therefore fail after
            // this transition while leaving a valid v1 main image in DELETE
            // mode. Treat that exact state as the retry continuation.
            "delete" => {}
            _ => return Err(UsageStoreError::SchemaIdentity),
        }
        guard.recheck(&path)?;
    }
    migrate_to_current(&mut conn, || {
        guard.recheck(path)?;
        let commit_guard = preflight_existing_family(path, true)?;
        before_migration_commit()?;
        Ok(commit_guard)
    })?;
    configure_persistent(&conn)?;
    verify_schema(&conn)?;
    super::report::validate_rows(&conn)?;
    drop(guard);
    protect_sqlite_files(path)?;
    let family_guard = preflight_existing_family(path, true)?;
    Ok(Some(WritableStore { conn, family_guard }))
}

fn configure_persistent(conn: &Connection) -> Result<(), UsageStoreError> {
    conn.pragma_update(None, "max_page_count", MAX_PAGE_COUNT)?;
    let max_page_count: i64 = conn.pragma_query_value(None, "max_page_count", |row| row.get(0))?;
    if max_page_count > MAX_PAGE_COUNT {
        return Err(UsageStoreError::GrowthLimit);
    }
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "wal_autocheckpoint", WAL_AUTOCHECKPOINT_PAGES)?;
    conn.pragma_update(None, "journal_size_limit", JOURNAL_SIZE_LIMIT_BYTES)?;
    Ok(())
}

fn prepare_file(path: &Path, create: bool) -> Result<PreparedFile, UsageStoreError> {
    let Some(parent) = path.parent() else {
        return Err(UsageStoreError::SchemaIdentity);
    };
    match parent.symlink_metadata() {
        Ok(_) => verify_private_directory_and_owner(parent)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound && create => {
            establish_private_data_root(parent)?;
            verify_private_directory_and_owner(parent)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(PreparedFile::Missing);
        }
        Err(error) => return Err(error.into()),
    }
    match path.symlink_metadata() {
        Ok(_) => {
            return Ok(PreparedFile::Existing(preflight_existing_family(
                path, true,
            )?));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    preflight_auxiliaries(path, false)?;
    if !create {
        return Ok(PreparedFile::Missing);
    }
    cleanup_stale_initializer_slots(path, SystemTime::now())?;
    initialize_and_publish(path)
}

static INIT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct RemoveTemporary(PathBuf);

impl Drop for RemoveTemporary {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn initialize_and_publish(path: &Path) -> Result<PreparedFile, UsageStoreError> {
    let start = usize::try_from(
        INIT_SEQUENCE.fetch_add(1, Ordering::Relaxed) % u64::try_from(INIT_SLOT_COUNT).unwrap_or(1),
    )
    .unwrap_or(0);
    let (temporary, file) = (0..INIT_SLOT_COUNT)
        .find_map(|offset| {
            let slot = (start + offset) % INIT_SLOT_COUNT;
            let temporary = initializer_slot_path(path, slot);
            match create_initializer_slot(&temporary) {
                Ok(file) => Some(Ok((temporary, file))),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => None,
                Err(error) => Some(Err(UsageStoreError::Io(error))),
            }
        })
        .transpose()?
        .ok_or_else(|| {
            UsageStoreError::Io(io::Error::new(
                io::ErrorKind::WouldBlock,
                "local usage initialization slots are busy",
            ))
        })?;
    let _cleanup = RemoveTemporary(temporary.clone());
    initialize_slot(path, temporary, file)
}

fn create_initializer_slot(path: &Path) -> io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    options.open(path)
}

fn initialize_slot(
    path: &Path,
    temporary: PathBuf,
    file: File,
) -> Result<PreparedFile, UsageStoreError> {
    restrict_private_file_handle(&file)?;
    verify_file_owner(&file)?;
    verify_single_link(&file)?;
    let mut conn = Connection::open_with_flags(
        &temporary,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    verify_same_file(&temporary, &file)?;
    configure_transient(&conn, BUSY_TIMEOUT)?;
    conn.pragma_update(None, "page_size", PAGE_SIZE_BYTES)?;
    initialize_schema(&mut conn)?;
    conn.pragma_update(None, "max_page_count", MAX_PAGE_COUNT)?;
    verify_schema(&conn)?;
    drop(conn);
    restrict_private_file_handle(&file)?;

    match fs::hard_link(&temporary, path) {
        Ok(()) => {
            fs::remove_file(&temporary)?;
            let published = reopen_same_file(path, &file)?;
            verify_single_link(&published)?;
            Ok(PreparedFile::NewInitialized(FamilyGuard::main_only(
                published,
            )?))
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            fs::remove_file(&temporary)?;
            let existing = preflight_existing_family(path, true)?;
            Ok(PreparedFile::Existing(existing))
        }
        Err(error) => Err(error.into()),
    }
}

fn initializer_slot_path(path: &Path, slot: usize) -> PathBuf {
    let mut value = OsString::from(path.as_os_str());
    value.push(format!(".init-{slot}"));
    PathBuf::from(value)
}

fn cleanup_stale_initializer_slots(path: &Path, now: SystemTime) -> Result<usize, UsageStoreError> {
    let mut removed = 0;
    for slot in 0..INIT_SLOT_COUNT {
        let candidate = initializer_slot_path(path, slot);
        if verify_private_file(&candidate).is_err() {
            continue;
        }
        let metadata = match candidate.symlink_metadata() {
            Ok(metadata)
                if metadata.is_file()
                    && !metadata.file_type().is_symlink()
                    && verify_metadata_owner(&metadata).is_ok() =>
            {
                metadata
            }
            Ok(_) => continue,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let candidate_handle = match open_nofollow(&candidate, true) {
            Ok(file)
                if verify_same_file(&candidate, &file).is_ok()
                    && verify_file_owner(&file).is_ok()
                    && verify_single_link(&file).is_ok() =>
            {
                file
            }
            Ok(_) | Err(_) => continue,
        };
        let stale = metadata
            .modified()
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= STALE_INIT_AGE);
        if stale {
            verify_same_file(&candidate, &candidate_handle)?;
            // Windows pathname deletion cannot proceed while the hardened
            // no-delete-sharing handle is retained.
            #[cfg(windows)]
            drop(candidate_handle);
            fs::remove_file(candidate)?;
            removed += 1;
        }
    }
    Ok(removed)
}

fn configure_transient(conn: &Connection, busy_timeout: Duration) -> Result<(), UsageStoreError> {
    conn.busy_timeout(busy_timeout)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "trusted_schema", "OFF")?;
    Ok(())
}

fn configure_report_connection(conn: &Connection) -> Result<(), UsageStoreError> {
    configure_transient(conn, BUSY_TIMEOUT)?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    conn.pragma_update(None, "query_only", "ON")?;
    Ok(())
}

fn utc_day(now: SystemTime) -> String {
    let now: DateTime<Utc> = now.into();
    now.date_naive().format("%Y-%m-%d").to_string()
}

fn retention_cutoff(now: SystemTime) -> String {
    let now: DateTime<Utc> = now.into();
    let retained_prior_days = u64::try_from(RETENTION_DAYS.saturating_sub(1)).unwrap_or(0);
    now.date_naive()
        .checked_sub_days(Days::new(retained_prior_days))
        .unwrap_or(now.date_naive())
        .format("%Y-%m-%d")
        .to_string()
}
