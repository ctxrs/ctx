use std::{
    ffi::OsString,
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    ptr::NonNull,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime},
};

use chrono::{DateTime, Days, Utc};
use ctx_history_core::platform_security::{
    create_private_directory_all, restrict_private_file_handle, verify_private_directory,
    verify_private_file, verify_private_file_handle,
};
use rusqlite::{
    params, serialize::OwnedData, Connection, DatabaseName, OpenFlags, OptionalExtension,
    TransactionBehavior,
};

use super::{CompletedOperation, CTX_VERSION, DEFINITION_VERSION, RETENTION_DAYS};

pub(crate) const USAGE_FILE: &str = "usage.sqlite";
const APPLICATION_ID: i64 = 0x4354_5855;
const SCHEMA_VERSION: i64 = 1;
const BUSY_TIMEOUT: Duration = Duration::from_millis(25);
const PAGE_SIZE_BYTES: i64 = 4 * 1024;
const MAX_DATABASE_BYTES: i64 = 6 * 1024 * 1024;
const MAX_FAMILY_BYTES: u64 = 8 * 1024 * 1024;
const MAX_PAGE_COUNT: i64 = MAX_DATABASE_BYTES / PAGE_SIZE_BYTES;
const WAL_AUTOCHECKPOINT_PAGES: i64 = 64;
const JOURNAL_SIZE_LIMIT_BYTES: i64 = 1024 * 1024;
const STALE_INIT_AGE: Duration = Duration::from_secs(60 * 60);
const INIT_SLOT_COUNT: usize = 8;

const DAILY_USAGE_SCHEMA: &str = r#"
CREATE TABLE daily_usage (
    day_utc TEXT NOT NULL
        CHECK (
            length(day_utc) = 10
            AND day_utc GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'
            AND date(day_utc) IS NOT NULL
            AND date(day_utc) = day_utc
        ),
    definition_version INTEGER NOT NULL CHECK (definition_version = 1),
    ctx_version TEXT NOT NULL
        CHECK (
            length(ctx_version) BETWEEN 1 AND 64
            AND ctx_version NOT GLOB '*[^0-9A-Za-z.+-]*'
        ),
    surface TEXT NOT NULL CHECK (surface IN ('cli', 'mcp')),
    operation TEXT NOT NULL CHECK (
        (
            surface = 'cli'
            AND operation IN (
                'setup', 'index', 'sources', 'import', 'show',
                'locate', 'search', 'pro_setup', 'pro_manage', 'pro_uninstall',
                'blame', 'sql', 'docs', 'integrations', 'daemon_status',
                'daemon_enable', 'daemon_disable', 'upgrade', 'doctor'
            )
        )
        OR
        (
            surface = 'mcp'
            AND operation IN (
                'status', 'sources', 'search', 'sql', 'show_session',
                'show_event', 'pro_status', 'blame'
            )
        )
    ),
    outcome TEXT NOT NULL CHECK (outcome IN ('success', 'failure')),
    value_class TEXT NOT NULL
        CHECK (value_class IN ('result_bearing', 'empty', 'not_applicable')),
    duration_bucket TEXT NOT NULL
        CHECK (duration_bucket IN (
            'under_10_ms', '10_to_49_ms', '50_to_249_ms', '250_to_999_ms',
            '1_to_4_s', '5_to_29_s', '30_s_or_more'
        )),
    target_type TEXT NOT NULL
        CHECK (target_type IN ('file', 'commit', 'pull_request', 'not_applicable')),
    pro_outcome TEXT NOT NULL
        CHECK (
            (
                operation = 'blame'
                AND (
                    (outcome = 'failure' AND pro_outcome = 'error')
                    OR
                    (
                        outcome = 'success'
                        AND pro_outcome IN ('produced', 'possible', 'none')
                    )
                )
            )
            OR (operation != 'blame' AND pro_outcome = 'not_applicable')
        ),
    calls INTEGER NOT NULL CHECK (calls > 0),
    result_count INTEGER NOT NULL CHECK (result_count >= 0),
    citation_count INTEGER NOT NULL
        CHECK (citation_count >= 0 AND (operation = 'blame' OR citation_count = 0)),
    response_bytes INTEGER NOT NULL
        CHECK (
            (surface = 'cli' AND response_bytes = 0)
            OR (surface = 'mcp' AND response_bytes > 0)
        ),
    CHECK (
        (
            outcome = 'failure'
            AND value_class = 'not_applicable'
            AND result_count = 0
            AND citation_count = 0
        )
        OR outcome = 'success'
    ),
    CHECK (
        (value_class = 'result_bearing' AND result_count >= calls)
        OR (
            value_class IN ('empty', 'not_applicable')
            AND result_count = 0
            AND citation_count = 0
        )
    ),
    CHECK (
        operation = 'blame'
        OR (
            target_type = 'not_applicable'
            AND pro_outcome = 'not_applicable'
            AND citation_count = 0
        )
    ),
    CHECK (
        operation != 'blame'
        OR (
            target_type IN ('file', 'commit', 'pull_request')
            OR (outcome = 'failure' AND target_type = 'not_applicable')
        )
    ),
    CHECK (
        outcome = 'failure'
        OR (
            surface = 'cli'
            AND (
                (operation = 'blame' AND value_class IN ('result_bearing', 'empty'))
                OR (operation != 'blame' AND value_class = 'not_applicable')
            )
        )
        OR (
            surface = 'mcp'
            AND (
                (
                    operation IN (
                        'sources', 'search', 'sql', 'show_session', 'show_event', 'blame'
                    )
                    AND value_class IN ('result_bearing', 'empty')
                )
                OR (
                    operation IN ('status', 'pro_status')
                    AND value_class = 'not_applicable'
                )
            )
        )
    ),
    PRIMARY KEY (
        day_utc, definition_version, ctx_version, surface, operation, outcome,
        value_class, duration_bucket, target_type, pro_outcome
    )
) WITHOUT ROWID, STRICT;
"#;

const MAINTENANCE_SCHEMA: &str = r#"
CREATE TABLE maintenance (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    last_retention_day TEXT NOT NULL CHECK (
        length(last_retention_day) = 10
        AND last_retention_day GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'
        AND date(last_retention_day) IS NOT NULL
        AND date(last_retention_day) = last_retention_day
    )
) WITHOUT ROWID, STRICT;
"#;

const EXPECTED_DAILY_COLUMNS: &[&str] = &[
    "day_utc",
    "definition_version",
    "ctx_version",
    "surface",
    "operation",
    "outcome",
    "value_class",
    "duration_bucket",
    "target_type",
    "pro_outcome",
    "calls",
    "result_count",
    "citation_count",
    "response_bytes",
];

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
    #[error("usage store schema does not match version 1")]
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
    let path = usage_path(data_root);
    let WritableStore {
        mut conn,
        family_guard,
    } = open_writable(&path, true, busy_timeout)?.ok_or(UsageStoreError::SchemaIdentity)?;
    let day = utc_day(now);
    let cutoff = retention_cutoff(now);
    let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    verify_schema(&transaction)?;
    super::report::validate_rows(&transaction)?;
    reject_future_daily_dates(&transaction, &day)?;
    transaction.execute(
        r#"
        INSERT INTO daily_usage (
            day_utc, definition_version, ctx_version, surface, operation, outcome,
            value_class, duration_bucket, target_type, pro_outcome, calls,
            result_count, citation_count, response_bytes
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 1, ?11, ?12, ?13)
        ON CONFLICT (
            day_utc, definition_version, ctx_version, surface, operation, outcome,
            value_class, duration_bucket, target_type, pro_outcome
        ) DO UPDATE SET
            calls = calls + 1,
            result_count = result_count + excluded.result_count,
            citation_count = citation_count + excluded.citation_count,
            response_bytes = response_bytes + excluded.response_bytes
        "#,
        params![
            day,
            DEFINITION_VERSION,
            ctx_version,
            operation.surface.as_str(),
            operation.operation,
            operation.outcome.as_str(),
            operation.value_class.as_str(),
            operation.duration.as_str(),
            operation.target_type.as_str(),
            operation.pro_outcome.as_str(),
            operation.result_count,
            operation.citation_count,
            operation.response_bytes,
        ],
    )?;
    let last_retention_day = transaction
        .query_row(
            "SELECT last_retention_day FROM maintenance WHERE singleton = 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if last_retention_day.as_deref() != Some(day.as_str()) {
        transaction.execute(
            r#"
            INSERT INTO maintenance (singleton, last_retention_day)
            VALUES (1, ?1)
            ON CONFLICT (singleton) DO UPDATE SET last_retention_day = excluded.last_retention_day
            "#,
            [day.as_str()],
        )?;
        transaction.execute("DELETE FROM daily_usage WHERE day_utc < ?1", [cutoff])?;
    }
    family_guard.recheck(&path)?;
    let commit_guard = preflight_existing_family(&path, true)?;
    verify_schema(&transaction)?;
    super::report::validate_rows(&transaction)?;
    transaction.commit()?;
    drop(commit_guard);
    let _ = protect_sqlite_files(&path);
    Ok(())
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
pub(super) fn growth_policy_for_test(
    data_root: &Path,
) -> Result<(i64, i64, i64, i64), UsageStoreError> {
    let path = usage_path(data_root);
    let opened =
        open_writable(&path, true, BUSY_TIMEOUT)?.ok_or(UsageStoreError::SchemaIdentity)?;
    let conn = &opened.conn;
    Ok((
        conn.pragma_query_value(None, "page_size", |row| row.get(0))?,
        conn.pragma_query_value(None, "max_page_count", |row| row.get(0))?,
        conn.pragma_query_value(None, "wal_autocheckpoint", |row| row.get(0))?,
        conn.pragma_query_value(None, "journal_size_limit", |row| row.get(0))?,
    ))
}

#[cfg(test)]
pub(super) fn fill_to_capacity_for_test(data_root: &Path) -> Result<String, UsageStoreError> {
    let path = usage_path(data_root);
    let WritableStore { mut conn, .. } =
        open_writable(&path, true, BUSY_TIMEOUT)?.ok_or(UsageStoreError::SchemaIdentity)?;
    let day = utc_day(SystemTime::now());
    let sql = r#"
        INSERT INTO daily_usage (
            day_utc, definition_version, ctx_version, surface, operation,
            outcome, value_class, duration_bucket, target_type, pro_outcome,
            calls, result_count, citation_count, response_bytes
        ) VALUES (
            ?1, 1, ?2, 'cli', 'doctor', 'success',
            'not_applicable', 'under_10_ms', 'not_applicable',
            'not_applicable', 1, 0, 0, 0
        )
    "#;
    let mut next = 0_u64;
    loop {
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut full = None;
        for _ in 0..256 {
            let version = format!("0.26.0-cap-{next:08}");
            next += 1;
            if let Err(error) = transaction.execute(sql, params![day, version]) {
                if error.sqlite_error_code() == Some(rusqlite::ErrorCode::DiskFull) {
                    full = Some(version);
                    break;
                }
                return Err(error.into());
            }
        }
        if let Some(mut version) = full {
            // SQLITE_FULL may have already rolled the transaction back.
            drop(transaction);
            loop {
                match conn.execute(sql, params![day, version]) {
                    Ok(_) => {
                        version = format!("0.26.0-cap-{next:08}");
                        next += 1;
                    }
                    Err(error)
                        if error.sqlite_error_code() == Some(rusqlite::ErrorCode::DiskFull) =>
                    {
                        return Ok(version);
                    }
                    Err(error) => return Err(error.into()),
                }
            }
        }
        transaction.commit()?;
    }
}

#[cfg(test)]
pub(super) fn record_with_ctx_version_for_test(
    data_root: &Path,
    operation: CompletedOperation,
    ctx_version: &str,
) -> Result<(), UsageStoreError> {
    record_at_with_ctx_version(
        data_root,
        operation,
        SystemTime::now(),
        BUSY_TIMEOUT,
        ctx_version,
    )
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
    let conn = deserialize_read_only(image)?;
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

#[cfg(test)]
pub(super) fn reset_with_post_commit_for_test<T>(
    data_root: &Path,
    after_commit: impl FnOnce(&Path) -> T,
) -> Result<bool, UsageStoreError> {
    reset_with_post_commit(data_root, after_commit)
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
        super::report::validate_rows(&detached)?;
        drop(detached);
        guard.recheck_unchanged(path)?;
        cleanup_stale_initializer_slots(path, SystemTime::now())?;
        guard.recheck_unchanged(path)?;
    }
    let mut flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    if newly_created {
        flags |= OpenFlags::SQLITE_OPEN_CREATE;
    }
    let conn = Connection::open_with_flags(path, flags)?;
    verify_same_file(path, &guard.main.file)?;
    verify_single_link(&guard.main.file)?;
    verify_schema(&conn)?;
    super::report::validate_rows(&conn)?;
    configure_transient(&conn, busy_timeout)?;
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
            create_private_directory_all(parent)?;
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
            verify_same_file(path, &file)?;
            verify_single_link(&file)?;
            Ok(PreparedFile::NewInitialized(FamilyGuard::main_only(file)?))
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

fn initialize_schema(conn: &mut Connection) -> Result<(), UsageStoreError> {
    let application_id: i64 = conn.pragma_query_value(None, "application_id", |row| row.get(0))?;
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if application_id != 0 || user_version != 0 || !database_is_empty(conn)? {
        return Err(UsageStoreError::SchemaIdentity);
    }
    let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute_batch(DAILY_USAGE_SCHEMA)?;
    transaction.execute_batch(MAINTENANCE_SCHEMA)?;
    transaction.pragma_update(None, "application_id", APPLICATION_ID)?;
    transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    transaction.commit()?;
    Ok(())
}

fn verify_schema(conn: &Connection) -> Result<(), UsageStoreError> {
    let page_size: i64 = conn.pragma_query_value(None, "page_size", |row| row.get(0))?;
    if page_size != PAGE_SIZE_BYTES {
        return Err(UsageStoreError::SchemaIdentity);
    }
    let page_count: i64 = conn.pragma_query_value(None, "page_count", |row| row.get(0))?;
    if page_size.saturating_mul(page_count) > MAX_DATABASE_BYTES {
        return Err(UsageStoreError::GrowthLimit);
    }
    let application_id: i64 = conn.pragma_query_value(None, "application_id", |row| row.get(0))?;
    if application_id != APPLICATION_ID {
        return Err(UsageStoreError::ApplicationId);
    }
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if user_version != SCHEMA_VERSION {
        return Err(UsageStoreError::SchemaVersion(user_version));
    }
    verify_schema_object_allowlist(conn)?;
    let mut statement = conn.prepare("PRAGMA table_info(daily_usage)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if columns
        .iter()
        .map(String::as_str)
        .ne(EXPECTED_DAILY_COLUMNS.iter().copied())
    {
        return Err(UsageStoreError::SchemaIdentity);
    }
    verify_table_schema(conn, "daily_usage", DAILY_USAGE_SCHEMA)?;
    verify_table_schema(conn, "maintenance", MAINTENANCE_SCHEMA)?;
    Ok(())
}

fn verify_schema_object_allowlist(conn: &Connection) -> Result<(), UsageStoreError> {
    let mut statement =
        conn.prepare("SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY name")?;
    let objects = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    })?;
    let mut daily_usage = false;
    let mut maintenance = false;
    for object in objects {
        let (kind, name, table, sql) = object?;
        match (kind.as_str(), name.as_str(), table.as_str(), sql.as_deref()) {
            ("table", "daily_usage", "daily_usage", Some(_)) if !daily_usage => {
                daily_usage = true;
            }
            ("table", "maintenance", "maintenance", Some(_)) if !maintenance => {
                maintenance = true;
            }
            // SQLite may own implicit indexes. WITHOUT ROWID currently needs
            // none, but permit only SQLite-internal, SQL-less indexes attached
            // to one of the two exact tables.
            ("index", name, "daily_usage" | "maintenance", None) if name.starts_with("sqlite_") => {
            }
            _ => return Err(UsageStoreError::SchemaIdentity),
        }
    }
    if !daily_usage || !maintenance {
        return Err(UsageStoreError::SchemaIdentity);
    }
    Ok(())
}

fn verify_table_schema(
    conn: &Connection,
    table: &str,
    expected: &str,
) -> Result<(), UsageStoreError> {
    let actual = conn
        .query_row(
            "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or(UsageStoreError::SchemaIdentity)?;
    if canonical_schema(&actual) != canonical_schema(expected) {
        return Err(UsageStoreError::SchemaIdentity);
    }
    Ok(())
}

fn canonical_schema(sql: &str) -> String {
    sql.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_end_matches(';')
        .to_owned()
}

fn database_is_empty(conn: &Connection) -> Result<bool, UsageStoreError> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%' LIMIT 1",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_none())
}

fn protect_sqlite_files(path: &Path) -> Result<(), UsageStoreError> {
    protect_sqlite_member(path)?;
    for suffix in ["-wal", "-shm"] {
        let auxiliary = auxiliary_path(path, suffix);
        match auxiliary.symlink_metadata() {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                protect_sqlite_member(&auxiliary)?;
            }
            Ok(_) => return Err(UsageStoreError::SchemaIdentity),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn preflight_auxiliaries(path: &Path, database_exists: bool) -> Result<(), UsageStoreError> {
    for suffix in ["-wal", "-shm"] {
        let auxiliary = auxiliary_path(path, suffix);
        match auxiliary.symlink_metadata() {
            Ok(_) if !database_exists => return Err(UsageStoreError::SchemaIdentity),
            Ok(_) => verify_private_file_and_owner(&auxiliary)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn auxiliary_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = OsString::from(path.as_os_str());
    value.push(suffix);
    PathBuf::from(value)
}

struct GuardedMember {
    file: File,
    len: u64,
    modified: Option<SystemTime>,
}

impl GuardedMember {
    fn from_file(file: File) -> Result<Self, UsageStoreError> {
        verify_private_file_handle(&file)?;
        verify_file_owner(&file)?;
        verify_single_link(&file)?;
        let metadata = file.metadata()?;
        Ok(Self {
            file,
            len: metadata.len(),
            modified: metadata.modified().ok(),
        })
    }

    fn recheck(&self, path: &Path, unchanged: bool) -> Result<(), UsageStoreError> {
        verify_same_file(path, &self.file)?;
        verify_private_file_handle(&self.file)?;
        verify_file_owner(&self.file)?;
        verify_single_link(&self.file)?;
        if unchanged {
            let metadata = self.file.metadata()?;
            if metadata.len() != self.len || metadata.modified().ok() != self.modified {
                return Err(UsageStoreError::UnsafeReadState);
            }
        }
        Ok(())
    }
}

struct FamilyGuard {
    main: GuardedMember,
    wal: Option<GuardedMember>,
    shm: Option<GuardedMember>,
}

impl FamilyGuard {
    fn main_only(file: File) -> Result<Self, UsageStoreError> {
        Ok(Self {
            main: GuardedMember::from_file(file)?,
            wal: None,
            shm: None,
        })
    }

    fn recheck(&self, path: &Path) -> Result<(), UsageStoreError> {
        self.recheck_members(path, false)
    }

    fn recheck_unchanged(&self, path: &Path) -> Result<(), UsageStoreError> {
        self.recheck_members(path, true)
    }

    fn recheck_members(&self, path: &Path, unchanged: bool) -> Result<(), UsageStoreError> {
        self.main.recheck(path, unchanged)?;
        recheck_optional_member(self.wal.as_ref(), &auxiliary_path(path, "-wal"), unchanged)?;
        recheck_optional_member(self.shm.as_ref(), &auxiliary_path(path, "-shm"), unchanged)?;
        verify_family_size(path)
    }

    fn has_nonempty_auxiliary(&self) -> Result<bool, UsageStoreError> {
        Ok(self.wal.as_ref().is_some_and(|member| {
            member
                .file
                .metadata()
                .map_or(true, |metadata| metadata.len() > 0)
        }) || self.shm.as_ref().is_some_and(|member| {
            member
                .file
                .metadata()
                .map_or(true, |metadata| metadata.len() > 0)
        }))
    }
}

fn recheck_optional_member(
    guarded: Option<&GuardedMember>,
    path: &Path,
    unchanged: bool,
) -> Result<(), UsageStoreError> {
    match (guarded, path.symlink_metadata()) {
        (Some(member), Ok(_)) => member.recheck(path, unchanged),
        (None, Err(error)) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        (Some(_), Err(error)) if error.kind() == io::ErrorKind::NotFound => {
            Err(UsageStoreError::UnsafeReadState)
        }
        (None, Ok(_)) => Err(UsageStoreError::UnsafeReadState),
        (_, Err(error)) => Err(error.into()),
    }
}

fn preflight_existing_family(path: &Path, read_only: bool) -> Result<FamilyGuard, UsageStoreError> {
    verify_private_file_and_owner(path)?;
    verify_family_size(path)?;
    let main = GuardedMember::from_file(open_nofollow(path, read_only)?)?;
    verify_same_file(path, &main.file)?;
    let wal = preflight_optional_member(&auxiliary_path(path, "-wal"))?;
    let shm = preflight_optional_member(&auxiliary_path(path, "-shm"))?;
    let guard = FamilyGuard { main, wal, shm };
    guard.recheck(path)?;
    Ok(guard)
}

fn preflight_optional_member(path: &Path) -> Result<Option<GuardedMember>, UsageStoreError> {
    match path.symlink_metadata() {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            verify_private_file_and_owner(path)?;
            let member = GuardedMember::from_file(open_nofollow(path, true)?)?;
            verify_same_file(path, &member.file)?;
            Ok(Some(member))
        }
        Ok(_) => Err(UsageStoreError::SchemaIdentity),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn capture_checkpointed_image(
    path: &Path,
    guard: &FamilyGuard,
    between_reads: impl FnOnce(),
) -> Result<Vec<u8>, UsageStoreError> {
    guard.recheck_unchanged(path)?;
    let first = read_bounded_image(&guard.main.file)?;
    guard.recheck_unchanged(path)?;
    between_reads();
    guard.recheck_unchanged(path)?;
    let second = read_bounded_image(&guard.main.file)?;
    guard.recheck_unchanged(path)?;
    if first != second {
        return Err(UsageStoreError::UnsafeReadState);
    }
    normalize_checkpointed_header(first)
}

fn read_bounded_image(file: &File) -> Result<Vec<u8>, UsageStoreError> {
    let expected_size =
        usize::try_from(file.metadata()?.len()).map_err(|_| UsageStoreError::GrowthLimit)?;
    if expected_size > usize::try_from(MAX_DATABASE_BYTES).unwrap_or(usize::MAX) {
        return Err(UsageStoreError::GrowthLimit);
    }
    let mut reader = file.try_clone()?;
    reader.seek(SeekFrom::Start(0))?;
    let limit = u64::try_from(MAX_DATABASE_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut image = Vec::new();
    reader.take(limit).read_to_end(&mut image)?;
    if image.len() > usize::try_from(MAX_DATABASE_BYTES).unwrap_or(usize::MAX) {
        return Err(UsageStoreError::GrowthLimit);
    }
    if image.len() != expected_size {
        return Err(UsageStoreError::UnsafeReadState);
    }
    Ok(image)
}

fn normalize_checkpointed_header(mut image: Vec<u8>) -> Result<Vec<u8>, UsageStoreError> {
    const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";
    if image.len() < 100
        || image.get(..SQLITE_HEADER.len()) != Some(SQLITE_HEADER.as_slice())
        || image.len() % usize::try_from(PAGE_SIZE_BYTES).unwrap_or(usize::MAX) != 0
    {
        return Err(UsageStoreError::SchemaIdentity);
    }
    let header_page_size = u16::from_be_bytes([image[16], image[17]]);
    if i64::from(header_page_size) != PAGE_SIZE_BYTES
        || !matches!(image[18], 1 | 2)
        || !matches!(image[19], 1 | 2)
        || image[18] != image[19]
    {
        return Err(UsageStoreError::SchemaIdentity);
    }
    // The bytes are now detached from the source. WAL-mode databases use 2 in
    // these header slots; an in-memory, read-only deserialize needs the
    // rollback-journal marker and never observes source auxiliaries.
    image[18] = 1;
    image[19] = 1;
    Ok(image)
}

fn deserialize_read_only(image: Vec<u8>) -> Result<Connection, UsageStoreError> {
    let size = image.len();
    let allocation = unsafe { rusqlite::ffi::sqlite3_malloc64(size as u64) }.cast::<u8>();
    let allocation = NonNull::new(allocation).ok_or(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_NOMEM),
        None,
    ))?;
    unsafe {
        std::ptr::copy_nonoverlapping(image.as_ptr(), allocation.as_ptr(), size);
    }
    let data = unsafe { OwnedData::from_raw_nonnull(allocation, size) };
    let mut conn = Connection::open_in_memory_with_flags(
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    conn.deserialize(DatabaseName::Main, data, true)?;
    verify_schema(&conn)?;
    Ok(conn)
}

#[cfg(test)]
pub(super) fn capture_with_between_reads_for_test(
    data_root: &Path,
    between_reads: impl FnOnce(),
) -> Result<(), UsageStoreError> {
    let path = usage_path(data_root);
    let guard = preflight_existing_family(&path, true)?;
    if guard.has_nonempty_auxiliary()? {
        return Err(UsageStoreError::UnsafeReadState);
    }
    let image = capture_checkpointed_image(&path, &guard, between_reads)?;
    drop(deserialize_read_only(image)?);
    guard.recheck_unchanged(&path)
}

fn protect_sqlite_member(path: &Path) -> Result<(), UsageStoreError> {
    let file = open_nofollow(path, false)?;
    restrict_private_file_handle(&file)?;
    verify_file_owner(&file)?;
    verify_same_file(path, &file)
}

fn open_nofollow(path: &Path, read_only: bool) -> Result<File, UsageStoreError> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(!read_only);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    Ok(options.open(path)?)
}

fn verify_private_directory_and_owner(path: &Path) -> Result<(), UsageStoreError> {
    verify_private_directory(path)?;
    verify_metadata_owner(&path.symlink_metadata()?)
}

fn verify_private_file_and_owner(path: &Path) -> Result<(), UsageStoreError> {
    verify_private_file(path)?;
    verify_metadata_owner(&path.symlink_metadata()?)
}

fn verify_file_owner(file: &File) -> Result<(), UsageStoreError> {
    verify_metadata_owner(&file.metadata()?)
}

fn verify_metadata_owner(metadata: &fs::Metadata) -> Result<(), UsageStoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err(UsageStoreError::SchemaIdentity);
        }
    }
    let _ = metadata;
    Ok(())
}

fn verify_same_file(path: &Path, file: &File) -> Result<(), UsageStoreError> {
    let path_metadata = path.symlink_metadata()?;
    if path_metadata.file_type().is_symlink() || !path_metadata.is_file() {
        return Err(UsageStoreError::SchemaIdentity);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let file_metadata = file.metadata()?;
        if path_metadata.dev() != file_metadata.dev() || path_metadata.ino() != file_metadata.ino()
        {
            return Err(UsageStoreError::SchemaIdentity);
        }
    }
    Ok(())
}

fn verify_single_link(file: &File) -> Result<(), UsageStoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if file.metadata()?.nlink() != 1 {
            return Err(UsageStoreError::SchemaIdentity);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        if file.metadata()?.number_of_links() != 1 {
            return Err(UsageStoreError::SchemaIdentity);
        }
    }
    Ok(())
}

fn verify_family_size(path: &Path) -> Result<(), UsageStoreError> {
    let mut bytes = 0_u64;
    for member in [
        path.to_path_buf(),
        auxiliary_path(path, "-wal"),
        auxiliary_path(path, "-shm"),
    ] {
        match member.symlink_metadata() {
            Ok(metadata) => {
                bytes = bytes
                    .checked_add(metadata.len())
                    .ok_or(UsageStoreError::GrowthLimit)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    if bytes >= MAX_FAMILY_BYTES {
        return Err(UsageStoreError::GrowthLimit);
    }
    Ok(())
}

fn reject_future_dates(conn: &Connection, day: &str) -> Result<(), UsageStoreError> {
    let latest: Option<String> = conn.query_row(
        r#"
        SELECT MAX(value) FROM (
            SELECT MAX(day_utc) AS value FROM daily_usage
            UNION ALL
            SELECT MAX(last_retention_day) AS value FROM maintenance
        )
        "#,
        [],
        |row| row.get(0),
    )?;
    if latest.as_deref().is_some_and(|latest| latest > day) {
        return Err(UsageStoreError::FutureDate);
    }
    Ok(())
}

fn reject_future_daily_dates(conn: &Connection, day: &str) -> Result<(), UsageStoreError> {
    let latest: Option<String> =
        conn.query_row("SELECT MAX(day_utc) FROM daily_usage", [], |row| row.get(0))?;
    if latest.as_deref().is_some_and(|latest| latest > day) {
        return Err(UsageStoreError::FutureDate);
    }
    Ok(())
}

pub(crate) fn verify_report_dates(
    conn: &Connection,
    now: SystemTime,
) -> Result<(), UsageStoreError> {
    reject_future_dates(conn, &utc_day(now))
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
