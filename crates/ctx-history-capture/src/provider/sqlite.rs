use std::{
    collections::BTreeSet,
    env, fs,
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    ops::Deref,
    path::{Path, PathBuf},
};

use rusqlite::{Connection, OpenFlags};
use serde_json::json;
use url::Url;
use uuid::Uuid;

use crate::common::io::ensure_regular_provider_transcript_file;
use crate::compute_payload_hash;
use crate::provider::sqlite_observation::{
    observe_sqlite_source_generation, SqliteObservedFile, SqliteSourceGeneration,
    SQLITE_GENERATION_MAX_ATTEMPTS, SQLITE_SNAPSHOT_MAX_BYTES,
};

use crate::{CaptureError, Result, MAX_PROVIDER_SQLITE_VALUE_BYTES};

pub(crate) fn sqlite_table_exists(conn: &Connection, table: &str) -> Result<bool> {
    let exists: i64 = conn.query_row(
        "select count(*) from sqlite_schema where type = 'table' and name = ?1",
        [table],
        |row| row.get(0),
    )?;
    Ok(exists > 0)
}

pub(crate) fn sqlite_table_columns(conn: &Connection, table: &str) -> Result<BTreeSet<String>> {
    let mut stmt = conn.prepare(&format!("pragma table_info({})", sqlite_ident(table)))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    rows.collect::<std::result::Result<BTreeSet<_>, _>>()
        .map_err(CaptureError::from)
}

pub(crate) fn optional_column_expr<'a>(
    columns: &BTreeSet<String>,
    column: &'a str,
    fallback: &'a str,
) -> &'a str {
    if columns.contains(column) {
        column
    } else {
        fallback
    }
}

pub(crate) fn optional_text_column_expr(
    columns: &BTreeSet<String>,
    column: &str,
    fallback: &str,
) -> String {
    if columns.contains(column) {
        format!("CAST({column} AS TEXT)")
    } else {
        fallback.to_owned()
    }
}

pub(crate) fn optional_timestamp_millis_expr(
    columns: &BTreeSet<String>,
    column: &str,
    fallback: &str,
) -> String {
    if !columns.contains(column) {
        return fallback.to_owned();
    }
    let text = format!("trim(CAST({column} AS TEXT))");
    let numeric_body = format!(
        "CASE WHEN substr({text}, 1, 1) IN ('+', '-') THEN substr({text}, 2) ELSE {text} END"
    );
    let numeric_value = format!(
        "CASE WHEN abs(CAST({column} AS REAL)) < 100000000000 \
         THEN CAST(ROUND(CAST({column} AS REAL) * 1000) AS INTEGER) \
         ELSE CAST(ROUND(CAST({column} AS REAL)) AS INTEGER) END"
    );
    format!(
        "CASE WHEN {column} IS NULL THEN NULL \
         WHEN typeof({column}) IN ('integer', 'real') THEN {numeric_value} \
         WHEN {numeric_body} != '' \
              AND {numeric_body} != '.' \
              AND {numeric_body} NOT GLOB '*[^0-9.]*' \
              AND length({numeric_body}) - length(replace({numeric_body}, '.', '')) <= 1 \
         THEN {numeric_value} \
         ELSE CAST(ROUND(unixepoch({column}, 'subsec') * 1000) AS INTEGER) END"
    )
}

pub(crate) fn ensure_sqlite_table_columns(
    columns: &BTreeSet<String>,
    label: &str,
    required: &[&str],
) -> Result<()> {
    let missing = required
        .iter()
        .copied()
        .filter(|column| !columns.contains(*column))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(CaptureError::InvalidPayload(format!(
            "{label} missing required column(s): {}",
            missing.join(", ")
        )))
    }
}

pub(crate) fn sqlite_ident(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

pub(crate) fn sqlite_is_too_big(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(ref fail, _)
            if fail.code == rusqlite::ErrorCode::TooBig
    )
}

pub(crate) struct ReadOnlySqliteConnection {
    conn: Connection,
    _snapshot_dir: Option<PrivateSnapshotDir>,
}

struct PrivateSnapshotDir {
    path: PathBuf,
}

impl PrivateSnapshotDir {
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for PrivateSnapshotDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

impl Deref for ReadOnlySqliteConnection {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        &self.conn
    }
}

pub(crate) fn open_sqlite_readonly_source(path: &Path) -> Result<ReadOnlySqliteConnection> {
    open_stable_sqlite_source(path)
}

fn observe_provider_sqlite_source_generation(path: &Path) -> Result<SqliteSourceGeneration> {
    match observe_sqlite_source_generation(path) {
        Ok(generation) => Ok(generation),
        Err(observation_error) if observation_error.kind() == io::ErrorKind::InvalidInput => {
            match ensure_regular_provider_transcript_file(path) {
                Err(error @ CaptureError::InvalidProviderTranscriptPath { .. }) => Err(error),
                _ => Err(CaptureError::Io(observation_error)),
            }
        }
        Err(error) => Err(CaptureError::Io(error)),
    }
}

pub(crate) fn probe_sqlite_readonly_source(
    path: &Path,
    predicate: impl Fn(&Connection) -> rusqlite::Result<bool>,
) -> Result<bool> {
    for _ in 0..SQLITE_GENERATION_MAX_ATTEMPTS {
        let before = observe_provider_sqlite_source_generation(path)?;
        ensure_supported_sqlite_generation(path, &before)?;
        let connection = if before.requires_snapshot() {
            let Some(snapshot) = copy_stable_sqlite_generation(path, &before)? else {
                continue;
            };
            snapshot
        } else {
            let Some(connection) = open_generation_checked_pinned_main(path, &before)? else {
                continue;
            };
            connection
        };
        let result = predicate(&connection);
        drop(connection);
        run_probe_test_hook(path);
        let after = match observe_provider_sqlite_source_generation(path) {
            Ok(after) => after,
            Err(CaptureError::Io(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::NotFound
                        | io::ErrorKind::InvalidInput
                ) =>
            {
                continue
            }
            Err(error) => return Err(error),
        };
        if before == after {
            return result.map_err(CaptureError::from);
        }
    }
    Err(CaptureError::Io(io::Error::new(
        io::ErrorKind::WouldBlock,
        format!(
            "SQLite source generation changed during each probe attempt: {}",
            path.display()
        ),
    )))
}

fn open_sqlite_pinned_main(source: &SqliteObservedFile) -> Result<ReadOnlySqliteConnection> {
    let identity_path = source.pinned_open_path()?;
    let uri = sqlite_readonly_uri(&identity_path)?;
    let conn = Connection::open_with_flags(uri.as_str(), sqlite_pinned_open_flags())?;
    conn.pragma_update(None, "query_only", true)?;
    conn.execute_batch("BEGIN DEFERRED")?;
    let _: i64 = conn.query_row("PRAGMA schema_version", [], |row| row.get(0))?;
    Ok(ReadOnlySqliteConnection {
        conn,
        _snapshot_dir: None,
    })
}

fn sqlite_pinned_open_flags() -> OpenFlags {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_URI;
    #[cfg(windows)]
    {
        flags | OpenFlags::SQLITE_OPEN_NOFOLLOW
    }
    #[cfg(not(windows))]
    {
        // The trusted descriptor path is itself a procfs/devfs symlink. Its
        // live descriptor is the identity boundary, so SQLITE_OPEN_NOFOLLOW
        // would reject the race-safe path.
        flags
    }
}

fn open_stable_sqlite_source(path: &Path) -> Result<ReadOnlySqliteConnection> {
    for _ in 0..SQLITE_GENERATION_MAX_ATTEMPTS {
        let before = observe_provider_sqlite_source_generation(path)?;
        ensure_supported_sqlite_generation(path, &before)?;
        if before.requires_snapshot() {
            if let Some(snapshot) = copy_stable_sqlite_generation(path, &before)? {
                return Ok(snapshot);
            }
        } else if let Some(connection) = open_generation_checked_pinned_main(path, &before)? {
            return Ok(connection);
        }
    }
    Err(CaptureError::Io(std::io::Error::new(
        std::io::ErrorKind::WouldBlock,
        format!(
            "SQLite source generation did not stabilize after {SQLITE_GENERATION_MAX_ATTEMPTS} open attempts: {}",
            path.display()
        ),
    )))
}

fn open_generation_checked_pinned_main(
    path: &Path,
    before: &SqliteSourceGeneration,
) -> Result<Option<ReadOnlySqliteConnection>> {
    run_pinned_open_test_hook(path, SqlitePinnedOpenTestPhase::BeforeOpen);
    let connection = open_sqlite_pinned_main(before.main());
    run_pinned_open_test_hook(path, SqlitePinnedOpenTestPhase::AfterOpen);
    let after = match observe_provider_sqlite_source_generation(path) {
        Ok(after) => after,
        Err(CaptureError::Io(error))
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::NotFound | io::ErrorKind::InvalidInput
            ) =>
        {
            return Ok(None)
        }
        Err(error) => return Err(error),
    };
    if before != &after {
        return Ok(None);
    }
    if matches!(
        connection,
        Err(CaptureError::Sqlite(rusqlite::Error::SqliteFailure(ref error, _)))
            if error.extended_code == rusqlite::ffi::SQLITE_CANTOPEN_SYMLINK
    ) {
        return Ok(None);
    }
    match connection {
        Ok(connection) => Ok(Some(connection)),
        Err(error) if pinned_open_retryable(&error) => Ok(None),
        Err(error) => recover_unavailable_pinned_open(path, before, error),
    }
}

fn recover_unavailable_pinned_open(
    path: &Path,
    before: &SqliteSourceGeneration,
    error: CaptureError,
) -> Result<Option<ReadOnlySqliteConnection>> {
    if pinned_identity_open_unavailable(&error) {
        copy_stable_sqlite_generation(path, before)
    } else {
        Err(error)
    }
}

fn pinned_identity_open_unavailable(error: &CaptureError) -> bool {
    #[cfg(unix)]
    {
        matches!(
            error,
            CaptureError::Io(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::Unsupported
                )
        ) || matches!(
            error,
            CaptureError::Sqlite(rusqlite::Error::SqliteFailure(error, _))
                if error.code == rusqlite::ErrorCode::CannotOpen
        )
    }
    #[cfg(not(unix))]
    {
        let _ = error;
        false
    }
}

fn pinned_open_retryable(error: &CaptureError) -> bool {
    matches!(
        error,
        CaptureError::Sqlite(rusqlite::Error::SqliteFailure(error, _))
            if matches!(
                error.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            )
    )
}

const SQLITE_SNAPSHOT_DISK_RESERVE_BYTES: u64 = 64 * 1024 * 1024;

struct SnapshotCopyLock {
    _file: File,
}

fn acquire_snapshot_copy_lock(parent: &Path) -> io::Result<SnapshotCopyLock> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

        let effective_uid = unsafe { libc::geteuid() };
        let path = parent.join(format!(
            ".ctx-provider-sqlite-snapshot-{effective_uid}.lock"
        ));
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.file_type().is_file()
            || metadata.uid() != effective_uid
            || metadata.mode() & 0o077 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "SQLite snapshot lock is not a private owner-controlled regular file",
            ));
        }
        fs2::FileExt::lock_exclusive(&file)?;
        Ok(SnapshotCopyLock { _file: file })
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

        let path = parent.join("ctx-provider-sqlite-snapshot.lock");
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        if !file.metadata()?.file_type().is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "SQLite snapshot lock is not a regular file",
            ));
        }
        fs2::FileExt::lock_exclusive(&file)?;
        Ok(SnapshotCopyLock { _file: file })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = parent;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "cross-process SQLite snapshot locking is unsupported on this platform",
        ))
    }
}

fn ensure_supported_sqlite_generation(
    path: &Path,
    generation: &SqliteSourceGeneration,
) -> Result<()> {
    if let Some(reason) = generation.deferred_reason() {
        return Err(CaptureError::Io(io::Error::new(
            io::ErrorKind::WouldBlock,
            format!("{reason}: {}", path.display()),
        )));
    }
    Ok(())
}

fn copy_stable_sqlite_generation(
    path: &Path,
    before: &SqliteSourceGeneration,
) -> Result<Option<ReadOnlySqliteConnection>> {
    let snapshot_bytes = before
        .snapshot_files()
        .into_iter()
        .try_fold(0_u64, |total, file| {
            total.checked_add(file.snapshot_len()).ok_or_else(|| {
                CaptureError::Io(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "SQLite snapshot byte count overflow",
                ))
            })
        })?;
    validate_snapshot_ceiling(snapshot_bytes)?;

    let snapshot_parent = env::temp_dir();
    // Serialize the free-space check with physical copying. Existing snapshots
    // are already reflected in available space when the next process enters.
    let _copy_lock = acquire_snapshot_copy_lock(&snapshot_parent)?;
    let available = fs2::available_space(&snapshot_parent)?;
    validate_snapshot_available_space(snapshot_bytes, available)?;
    let snapshot_dir = create_private_snapshot_dir_in(&snapshot_parent)?;

    let attempt = record_snapshot_attempt();
    for source in before.snapshot_files() {
        let file_name = source.path().file_name().ok_or_else(|| {
            CaptureError::InvalidProviderTranscriptPath {
                path: source.path().to_path_buf(),
                reason: "provider SQLite path has no file name",
            }
        })?;
        let destination = snapshot_dir.path().join(file_name);
        match copy_sqlite_snapshot_file(source, &destination, source.snapshot_len()) {
            Ok(true) => record_snapshot_copy(),
            Ok(false) => return Ok(None),
            Err(error) => return Err(CaptureError::Io(error)),
        }
    }
    run_snapshot_test_hook(path, attempt);
    let after = match observe_provider_sqlite_source_generation(path) {
        Ok(after) => after,
        Err(CaptureError::Io(error))
            if matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::NotFound | io::ErrorKind::InvalidInput
            ) =>
        {
            return Ok(None)
        }
        Err(error) => return Err(error),
    };
    if before != &after {
        return Ok(None);
    }

    let snapshot_path = snapshot_dir.path().join(path.file_name().ok_or_else(|| {
        CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "provider SQLite path has no file name",
        }
    })?);
    // Recovery, WAL-index creation, and journal cleanup stay inside this
    // protected RAII directory. Adapters only receive a query-only connection.
    let conn = Connection::open_with_flags(
        &snapshot_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let _: i64 = conn.query_row("PRAGMA schema_version", [], |row| row.get(0))?;
    conn.pragma_update(None, "query_only", true)?;
    Ok(Some(ReadOnlySqliteConnection {
        conn,
        _snapshot_dir: Some(snapshot_dir),
    }))
}

fn validate_snapshot_ceiling(snapshot_bytes: u64) -> Result<()> {
    if snapshot_bytes > SQLITE_SNAPSHOT_MAX_BYTES {
        return Err(CaptureError::Io(io::Error::new(
            io::ErrorKind::WouldBlock,
            format!(
                "SQLite snapshot requires {snapshot_bytes} bytes, exceeding the {SQLITE_SNAPSHOT_MAX_BYTES} byte ceiling"
            ),
        )));
    }
    Ok(())
}

fn validate_snapshot_available_space(snapshot_bytes: u64, available: u64) -> Result<()> {
    let required = snapshot_bytes
        .checked_add(SQLITE_SNAPSHOT_DISK_RESERVE_BYTES)
        .ok_or_else(|| {
            CaptureError::Io(io::Error::new(
                io::ErrorKind::WouldBlock,
                "SQLite snapshot disk requirement overflow",
            ))
        })?;
    if available < required {
        return Err(CaptureError::Io(io::Error::new(
            io::ErrorKind::WouldBlock,
            format!(
                "SQLite snapshot requires {required} bytes including reserve, but only {available} bytes are available"
            ),
        )));
    }
    Ok(())
}

fn copy_sqlite_snapshot_file(
    source: &crate::provider::sqlite_observation::SqliteObservedFile,
    destination: &Path,
    byte_count: u64,
) -> io::Result<bool> {
    let mut source_file = source.snapshot_reader()?;
    let mut destination_file = create_private_snapshot_file(destination)?;
    run_snapshot_copy_test_hook(source.path());
    source_file.seek(SeekFrom::Start(0))?;
    let copied = io::copy(
        &mut source_file.by_ref().take(byte_count),
        &mut destination_file,
    )?;
    if copied != byte_count {
        return Ok(false);
    }
    Ok(true)
}

fn create_private_snapshot_dir_in(parent: &Path) -> io::Result<PrivateSnapshotDir> {
    for _ in 0..16 {
        let path = parent.join(format!("ctx-provider-sqlite-{}", Uuid::new_v4().simple()));
        match create_private_snapshot_dir_at(&path) {
            Ok(()) => return Ok(PrivateSnapshotDir { path }),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique private SQLite snapshot directory",
    ))
}

#[cfg(unix)]
fn create_private_snapshot_dir_at(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700).create(path)
}

#[cfg(windows)]
fn create_private_snapshot_dir_at(path: &Path) -> io::Result<()> {
    use std::{mem, os::windows::ffi::OsStrExt};
    use windows_sys::Win32::{
        Foundation::LocalFree, Security::SECURITY_ATTRIBUTES, Storage::FileSystem::CreateDirectoryW,
    };

    let descriptor = private_windows_security_descriptor(true)?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let path = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let created = unsafe { CreateDirectoryW(path.as_ptr(), &attributes) };
    let error = (created == 0).then(io::Error::last_os_error);
    unsafe {
        LocalFree(descriptor);
    }
    error.map_or(Ok(()), Err)
}

#[cfg(not(any(unix, windows)))]
fn create_private_snapshot_dir_at(_path: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "private SQLite snapshots are unsupported on this platform",
    ))
}

#[cfg(unix)]
fn create_private_snapshot_file(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(windows)]
fn create_private_snapshot_file(path: &Path) -> io::Result<File> {
    use std::{mem, os::windows::ffi::OsStrExt, os::windows::io::FromRawHandle, ptr};
    use windows_sys::Win32::{
        Foundation::{LocalFree, INVALID_HANDLE_VALUE},
        Security::SECURITY_ATTRIBUTES,
        Storage::FileSystem::{
            CreateFileW, CREATE_NEW, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_GENERIC_WRITE,
        },
    };

    let descriptor = private_windows_security_descriptor(false)?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor,
        bInheritHandle: 0,
    };
    let path = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            FILE_GENERIC_WRITE,
            0,
            &attributes,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        )
    };
    let error = (handle == INVALID_HANDLE_VALUE).then(io::Error::last_os_error);
    unsafe {
        LocalFree(descriptor);
    }
    if let Some(error) = error {
        return Err(error);
    }
    Ok(unsafe { File::from_raw_handle(handle) })
}

#[cfg(not(any(unix, windows)))]
fn create_private_snapshot_file(_path: &Path) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "private SQLite snapshots are unsupported on this platform",
    ))
}

#[cfg(windows)]
fn private_windows_security_descriptor(
    directory: bool,
) -> io::Result<windows_sys::Win32::Security::PSECURITY_DESCRIPTOR> {
    use std::ptr;
    use windows_sys::Win32::Security::{
        Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW, PSECURITY_DESCRIPTOR,
    };

    let sddl = if directory {
        "D:P(A;OICI;FA;;;OW)(A;OICI;FA;;;SY)"
    } else {
        "D:P(A;;FA;;;OW)(A;;FA;;;SY)"
    };
    let sddl = sddl.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            ptr::null_mut(),
        )
    };
    if converted == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(descriptor)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SqlitePinnedOpenTestPhase {
    BeforeOpen,
    AfterOpen,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SqliteSnapshotTestMetrics {
    pub(crate) attempts: usize,
    pub(crate) copied_files: usize,
}

#[cfg(test)]
type SqliteSnapshotTestHook = Box<dyn FnMut(&Path, usize)>;
#[cfg(test)]
type SqlitePathTestHook = Box<dyn FnMut(&Path)>;
#[cfg(test)]
type SqlitePinnedOpenTestHook = Box<dyn FnMut(&Path, SqlitePinnedOpenTestPhase)>;

#[cfg(test)]
thread_local! {
    static SNAPSHOT_TEST_METRICS: std::cell::Cell<SqliteSnapshotTestMetrics> =
        const { std::cell::Cell::new(SqliteSnapshotTestMetrics { attempts: 0, copied_files: 0 }) };
    static SNAPSHOT_TEST_HOOK: std::cell::RefCell<Option<SqliteSnapshotTestHook>> =
        const { std::cell::RefCell::new(None) };
    static SNAPSHOT_COPY_TEST_HOOK: std::cell::RefCell<Option<SqlitePathTestHook>> =
        const { std::cell::RefCell::new(None) };
    static PINNED_OPEN_TEST_HOOK: std::cell::RefCell<Option<SqlitePinnedOpenTestHook>> =
        const { std::cell::RefCell::new(None) };
    static PROBE_TEST_HOOK: std::cell::RefCell<Option<SqlitePathTestHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn take_sqlite_snapshot_test_metrics() -> SqliteSnapshotTestMetrics {
    SNAPSHOT_TEST_METRICS.with(|metrics| metrics.replace(SqliteSnapshotTestMetrics::default()))
}

#[cfg(test)]
pub(crate) struct SqliteSnapshotTestHookGuard;

#[cfg(test)]
impl Drop for SqliteSnapshotTestHookGuard {
    fn drop(&mut self) {
        SNAPSHOT_TEST_HOOK.with(|hook| *hook.borrow_mut() = None);
    }
}

#[cfg(test)]
pub(crate) struct SqliteProbeTestHookGuard;

#[cfg(test)]
impl Drop for SqliteProbeTestHookGuard {
    fn drop(&mut self) {
        PROBE_TEST_HOOK.with(|hook| *hook.borrow_mut() = None);
    }
}

#[cfg(test)]
pub(crate) struct SqliteSnapshotCopyTestHookGuard;

#[cfg(test)]
impl Drop for SqliteSnapshotCopyTestHookGuard {
    fn drop(&mut self) {
        SNAPSHOT_COPY_TEST_HOOK.with(|hook| *hook.borrow_mut() = None);
    }
}

#[cfg(test)]
pub(crate) struct SqlitePinnedOpenTestHookGuard;

#[cfg(test)]
impl Drop for SqlitePinnedOpenTestHookGuard {
    fn drop(&mut self) {
        PINNED_OPEN_TEST_HOOK.with(|hook| *hook.borrow_mut() = None);
    }
}

#[cfg(test)]
pub(crate) fn install_sqlite_snapshot_test_hook(
    hook: impl FnMut(&Path, usize) + 'static,
) -> SqliteSnapshotTestHookGuard {
    SNAPSHOT_TEST_HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
    SqliteSnapshotTestHookGuard
}

#[cfg(test)]
pub(crate) fn install_sqlite_probe_test_hook(
    hook: impl FnMut(&Path) + 'static,
) -> SqliteProbeTestHookGuard {
    PROBE_TEST_HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
    SqliteProbeTestHookGuard
}

#[cfg(test)]
pub(crate) fn install_sqlite_snapshot_copy_test_hook(
    hook: impl FnMut(&Path) + 'static,
) -> SqliteSnapshotCopyTestHookGuard {
    SNAPSHOT_COPY_TEST_HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
    SqliteSnapshotCopyTestHookGuard
}

#[cfg(test)]
pub(crate) fn install_sqlite_pinned_open_test_hook(
    hook: impl FnMut(&Path, SqlitePinnedOpenTestPhase) + 'static,
) -> SqlitePinnedOpenTestHookGuard {
    PINNED_OPEN_TEST_HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
    SqlitePinnedOpenTestHookGuard
}

#[cfg(test)]
fn record_snapshot_attempt() -> usize {
    SNAPSHOT_TEST_METRICS.with(|metrics| {
        let mut value = metrics.get();
        value.attempts += 1;
        metrics.set(value);
        value.attempts
    })
}

#[cfg(not(test))]
fn record_snapshot_attempt() -> usize {
    0
}

#[cfg(test)]
fn record_snapshot_copy() {
    SNAPSHOT_TEST_METRICS.with(|metrics| {
        let mut value = metrics.get();
        value.copied_files += 1;
        metrics.set(value);
    });
}

#[cfg(not(test))]
fn record_snapshot_copy() {}

#[cfg(test)]
fn run_snapshot_test_hook(path: &Path, attempt: usize) {
    SNAPSHOT_TEST_HOOK.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook(path, attempt);
        }
    });
}

#[cfg(not(test))]
fn run_snapshot_test_hook(_path: &Path, _attempt: usize) {}

#[cfg(test)]
fn run_snapshot_copy_test_hook(path: &Path) {
    SNAPSHOT_COPY_TEST_HOOK.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook(path);
        }
    });
}

#[cfg(not(test))]
fn run_snapshot_copy_test_hook(_path: &Path) {}

#[cfg(test)]
fn run_pinned_open_test_hook(path: &Path, phase: SqlitePinnedOpenTestPhase) {
    PINNED_OPEN_TEST_HOOK.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook(path, phase);
        }
    });
}

#[cfg(not(test))]
fn run_pinned_open_test_hook(_path: &Path, _phase: SqlitePinnedOpenTestPhase) {}

#[cfg(test)]
fn run_probe_test_hook(path: &Path) {
    PROBE_TEST_HOOK.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook(path);
        }
    });
}

#[cfg(not(test))]
fn run_probe_test_hook(_path: &Path) {}

fn sqlite_readonly_uri(path: &Path) -> Result<String> {
    let absolute_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()?.join(path)
    };
    let mut url = Url::from_file_path(&absolute_path).map_err(|()| {
        CaptureError::InvalidProviderTranscriptPath {
            path: absolute_path,
            reason: "provider SQLite path cannot be represented as a file URI",
        }
    })?;
    url.query_pairs_mut().append_pair("mode", "ro");
    Ok(url.to_string())
}

pub(crate) fn sqlite_row_ids_with_oversized_value(
    path: &Path,
    table: &str,
    id_column: &str,
    value_column: &str,
) -> Result<BTreeSet<String>> {
    let conn = open_sqlite_readonly_source(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.pragma_update(None, "query_only", true)?;
    // This prescan intentionally omits SQLITE_LIMIT_LENGTH: bounded connections
    // can raise SQLITE_TOOBIG before returning ids, and this query returns ids only.
    let mut stmt = conn.prepare(&format!(
        "select {} from {} where length(cast({} as blob)) > ?",
        sqlite_ident(id_column),
        sqlite_ident(table),
        sqlite_ident(value_column),
    ))?;
    let rows = stmt.query_map([MAX_PROVIDER_SQLITE_VALUE_BYTES as i64], |row| {
        row.get::<_, String>(0)
    })?;
    rows.collect::<std::result::Result<BTreeSet<_>, _>>()
        .map_err(CaptureError::from)
}

pub(crate) fn opencode_schema_fingerprint(conn: &Connection) -> Result<String> {
    let mut stmt = conn.prepare(
        "select name, sql from sqlite_schema where type in ('table','index') order by name",
    )?;
    let rows = stmt.query_map([], |row| {
        let name: String = row.get(0)?;
        let sql: Option<String> = row.get(1)?;
        Ok(format!("{name}:{}", sql.unwrap_or_default()))
    })?;
    let schema = rows.collect::<std::result::Result<Vec<_>, _>>()?.join("\n");
    compute_payload_hash(&json!({ "schema": schema }))
}

#[cfg(test)]
include!("sqlite/tests.rs");
