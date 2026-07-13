use std::{
    collections::BTreeSet,
    env, fs,
    fs::File,
    io::{self, Read},
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
    observe_sqlite_source_generation, SqliteSourceGeneration, SQLITE_GENERATION_MAX_ATTEMPTS,
    SQLITE_SNAPSHOT_MAX_BYTES,
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
    ensure_regular_provider_transcript_file(path)?;
    let generation = observe_sqlite_source_generation(path)?;
    ensure_supported_sqlite_generation(path, &generation)?;
    if !generation.requires_snapshot() {
        return open_sqlite_immutable_main(path);
    }
    open_stable_sqlite_snapshot(path)
}

pub(crate) fn probe_sqlite_readonly_source(
    path: &Path,
    predicate: impl Fn(&Connection) -> rusqlite::Result<bool>,
) -> Result<bool> {
    ensure_regular_provider_transcript_file(path)?;
    for _ in 0..SQLITE_GENERATION_MAX_ATTEMPTS {
        let before = observe_sqlite_source_generation(path)?;
        ensure_supported_sqlite_generation(path, &before)?;
        let connection = if before.requires_snapshot() {
            let Some(snapshot) = copy_stable_sqlite_generation(path, &before)? else {
                continue;
            };
            snapshot
        } else {
            open_sqlite_immutable_main(path)?
        };
        let result = predicate(&connection);
        run_probe_test_hook(path);
        let after = match observe_sqlite_source_generation(path) {
            Ok(after) => after,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
            Err(error) => return Err(CaptureError::Io(error)),
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

fn open_sqlite_immutable_main(path: &Path) -> Result<ReadOnlySqliteConnection> {
    let uri = sqlite_immutable_uri(path)?;
    let conn = Connection::open_with_flags(
        uri.as_str(),
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI,
    )?;
    conn.pragma_update(None, "query_only", true)?;
    Ok(ReadOnlySqliteConnection {
        conn,
        _snapshot_dir: None,
    })
}

fn open_stable_sqlite_snapshot(path: &Path) -> Result<ReadOnlySqliteConnection> {
    for _ in 0..SQLITE_GENERATION_MAX_ATTEMPTS {
        let before = observe_sqlite_source_generation(path)?;
        ensure_supported_sqlite_generation(path, &before)?;
        if !before.requires_snapshot() {
            return open_sqlite_immutable_main(path);
        }
        if let Some(snapshot) = copy_stable_sqlite_generation(path, &before)? {
            return Ok(snapshot);
        }
    }
    Err(CaptureError::Io(std::io::Error::new(
        std::io::ErrorKind::WouldBlock,
        format!(
            "SQLite source generation did not stabilize after {SQLITE_GENERATION_MAX_ATTEMPTS} copy attempts: {}",
            path.display()
        ),
    )))
}

const SQLITE_SNAPSHOT_DISK_RESERVE_BYTES: u64 = 64 * 1024 * 1024;

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
        match copy_sqlite_snapshot_file(source.path(), &destination, source.snapshot_len()) {
            Ok(()) => record_snapshot_copy(),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::UnexpectedEof
                ) =>
            {
                return Ok(None)
            }
            Err(error) => return Err(CaptureError::Io(error)),
        }
    }
    run_snapshot_test_hook(path, attempt);
    let after = match observe_sqlite_source_generation(path) {
        Ok(after) => after,
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(None),
        Err(error) => return Err(CaptureError::Io(error)),
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

fn copy_sqlite_snapshot_file(source: &Path, destination: &Path, byte_count: u64) -> io::Result<()> {
    let mut source_file = File::open(source)?;
    let mut destination_file = create_private_snapshot_file(destination)?;
    let copied = io::copy(
        &mut source_file.by_ref().take(byte_count),
        &mut destination_file,
    )?;
    if copied != byte_count {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!("SQLite source changed while copying {}", source.display()),
        ));
    }
    Ok(())
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

#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SqliteSnapshotTestMetrics {
    pub(crate) attempts: usize,
    pub(crate) copied_files: usize,
}

#[cfg(test)]
thread_local! {
    static SNAPSHOT_TEST_METRICS: std::cell::Cell<SqliteSnapshotTestMetrics> =
        std::cell::Cell::new(SqliteSnapshotTestMetrics { attempts: 0, copied_files: 0 });
    static SNAPSHOT_TEST_HOOK: std::cell::RefCell<Option<Box<dyn FnMut(&Path, usize)>>> =
        std::cell::RefCell::new(None);
    static PROBE_TEST_HOOK: std::cell::RefCell<Option<Box<dyn FnMut(&Path)>>> =
        std::cell::RefCell::new(None);
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
fn run_probe_test_hook(path: &Path) {
    PROBE_TEST_HOOK.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook(path);
        }
    });
}

#[cfg(not(test))]
fn run_probe_test_hook(_path: &Path) {}

fn sqlite_immutable_uri(path: &Path) -> Result<String> {
    let absolute_path =
        path.canonicalize()
            .map_err(|_| CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "failed to resolve provider SQLite path",
            })?;
    let mut url = Url::from_file_path(&absolute_path).map_err(|()| {
        CaptureError::InvalidProviderTranscriptPath {
            path: absolute_path,
            reason: "provider SQLite path cannot be represented as a file URI",
        }
    })?;
    url.query_pairs_mut()
        .append_pair("mode", "ro")
        .append_pair("immutable", "1");
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
mod tests {
    use std::{cell::Cell, fs, io::Write, path::Path, rc::Rc};

    use rusqlite::{params, types::Value as SqlValue, Connection};

    use super::{
        create_private_snapshot_dir_in, create_private_snapshot_file,
        install_sqlite_probe_test_hook, install_sqlite_snapshot_test_hook,
        open_sqlite_readonly_source, optional_text_column_expr, optional_timestamp_millis_expr,
        probe_sqlite_readonly_source, take_sqlite_snapshot_test_metrics,
        validate_snapshot_available_space, validate_snapshot_ceiling, BTreeSet,
        SqliteSnapshotTestMetrics, SQLITE_SNAPSHOT_DISK_RESERVE_BYTES, SQLITE_SNAPSHOT_MAX_BYTES,
    };

    #[test]
    fn optional_sqlite_casts_normalize_native_text_and_timestamp_shapes() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE samples (position INTEGER, value)", [])
            .unwrap();
        let samples = [
            (SqlValue::Integer(1_783_653_514), Some(1_783_653_514_000)),
            (SqlValue::Real(1_783_653_514.491), Some(1_783_653_514_491)),
            (
                SqlValue::Integer(1_783_653_514_491),
                Some(1_783_653_514_491),
            ),
            (SqlValue::Real(1_783_653_514_491.0), Some(1_783_653_514_491)),
            (SqlValue::Text("1783653514".into()), Some(1_783_653_514_000)),
            (
                SqlValue::Text("+1783653514".into()),
                Some(1_783_653_514_000),
            ),
            (SqlValue::Text("-1.25".into()), Some(-1_250)),
            (
                SqlValue::Text("1783653514.491".into()),
                Some(1_783_653_514_491),
            ),
            (
                SqlValue::Text("1783653514491".into()),
                Some(1_783_653_514_491),
            ),
            (
                SqlValue::Text("0001783653514".into()),
                Some(1_783_653_514_000),
            ),
            (
                SqlValue::Text("2026-07-10T03:18:34.491Z".into()),
                Some(1_783_653_514_491),
            ),
            (
                SqlValue::Text("2026-07-10T05:48:34.491+02:30".into()),
                Some(1_783_653_514_491),
            ),
            (SqlValue::Text("not-a-timestamp".into()), None),
            (SqlValue::Text("  ".into()), None),
            (SqlValue::Null, None),
        ];
        for (position, (value, _)) in samples.iter().enumerate() {
            conn.execute(
                "INSERT INTO samples VALUES (?1, ?2)",
                params![position as i64, value],
            )
            .unwrap();
        }

        let columns = BTreeSet::from(["value".to_owned()]);
        let timestamp = optional_timestamp_millis_expr(&columns, "value", "NULL");
        let sql = format!("SELECT {timestamp} FROM samples ORDER BY position");
        let actual = conn
            .prepare(&sql)
            .unwrap()
            .query_map([], |row| row.get::<_, Option<i64>>(0))
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            actual,
            samples
                .iter()
                .map(|(_, expected)| *expected)
                .collect::<Vec<_>>()
        );

        let text = optional_text_column_expr(&columns, "value", "NULL");
        let value: String = conn
            .query_row(
                &format!("SELECT {text} FROM samples WHERE position = 0"),
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(value, "1783653514");

        let missing = BTreeSet::new();
        assert_eq!(
            optional_timestamp_millis_expr(&missing, "value", "fallback"),
            "fallback"
        );
        assert_eq!(
            optional_text_column_expr(&missing, "value", "fallback"),
            "fallback"
        );
    }

    #[test]
    fn checkpointed_wal_retries_as_a_new_generation() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("source.db");
        let writer = real_wal_writer(&db);
        let _hook = install_sqlite_snapshot_test_hook(move |_, attempt| {
            if attempt == 1 {
                writer
                    .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
                    .unwrap();
            }
        });

        take_sqlite_snapshot_test_metrics();
        let connection = open_sqlite_readonly_source(&db).unwrap();
        assert_eq!(
            connection
                .query_row("PRAGMA query_only", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            take_sqlite_snapshot_test_metrics(),
            SqliteSnapshotTestMetrics {
                attempts: 1,
                copied_files: 2,
            }
        );
    }

    #[test]
    fn real_wal_snapshot_is_private_query_only_and_raii_cleaned() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("source.db");
        let _writer = real_wal_writer(&db);

        let connection = open_sqlite_readonly_source(&db).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT value FROM entries WHERE id = 1", [], |row| {
                    row.get::<_, String>(0)
                })
                .unwrap(),
            "omega"
        );
        assert_eq!(
            connection
                .query_row("PRAGMA query_only", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
        let snapshot = connection
            ._snapshot_dir
            .as_ref()
            .expect("WAL requires a snapshot")
            .path()
            .to_path_buf();
        assert!(snapshot.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            assert_eq!(
                fs::metadata(&snapshot).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(snapshot.join("source.db"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        drop(connection);
        assert!(!snapshot.exists());
    }

    #[test]
    fn immutable_main_with_shm_only_does_not_copy() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("source.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute("CREATE TABLE entries (id INTEGER)", [])
            .unwrap();
        drop(conn);
        fs::write(sidecar(&db, "-shm"), b"volatile coordination state").unwrap();

        take_sqlite_snapshot_test_metrics();
        let connection = open_sqlite_readonly_source(&db).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT count(*) FROM entries", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert!(connection._snapshot_dir.is_none());
        assert_eq!(
            take_sqlite_snapshot_test_metrics(),
            SqliteSnapshotTestMetrics::default()
        );
    }

    #[test]
    fn copied_readonly_main_is_writable_for_hot_journal_recovery() {
        let fixture = real_hot_journal_fixture();
        let mut permissions = fs::metadata(&fixture.db).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&fixture.db, permissions).unwrap();

        let connection = open_sqlite_readonly_source(&fixture.db).unwrap();
        let restored: i64 = connection
            .query_row(
                "SELECT count(*) FROM entries WHERE substr(value, 1, 1) = 'a'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(restored, 256);
        drop(connection);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            fs::set_permissions(&fixture.db, fs::Permissions::from_mode(0o600)).unwrap();
        }
        #[cfg(not(unix))]
        {
            let mut permissions = fs::metadata(&fixture.db).unwrap().permissions();
            permissions.set_readonly(false);
            fs::set_permissions(&fixture.db, permissions).unwrap();
        }
    }

    #[test]
    fn hot_journal_with_attached_database_super_pointer_defers() {
        let fixture = real_hot_journal_fixture();
        let journal = sidecar(&fixture.db, "-journal");
        let super_journal = fixture
            .db
            .parent()
            .unwrap()
            .join("attached-main.db-mj H8a1");
        fs::write(&super_journal, b"active multi-database commit").unwrap();
        append_super_journal_trailer(&journal, &native_path_bytes(&super_journal));

        let error = match open_sqlite_readonly_source(&fixture.db) {
            Ok(_) => panic!("super-journal generation was imported"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            crate::CaptureError::Io(ref error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
    }

    #[test]
    fn sqlite_probe_retries_both_negative_and_positive_races() {
        for starts_present in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let db = temp.path().join("probe.db");
            let conn = Connection::open(&db).unwrap();
            if starts_present {
                conn.execute("CREATE TABLE target (id INTEGER)", [])
                    .unwrap();
            } else {
                conn.execute("CREATE TABLE baseline (id INTEGER)", [])
                    .unwrap();
            }
            drop(conn);

            let changed = Rc::new(Cell::new(false));
            let changed_for_hook = Rc::clone(&changed);
            let _hook = install_sqlite_probe_test_hook(move |path| {
                if changed_for_hook.replace(true) {
                    return;
                }
                let conn = Connection::open(path).unwrap();
                if starts_present {
                    conn.execute("DROP TABLE target", []).unwrap();
                } else {
                    conn.execute("CREATE TABLE target (id INTEGER)", [])
                        .unwrap();
                }
            });
            let found = probe_sqlite_readonly_source(&db, |conn| {
                conn.query_row(
                    "SELECT count(*) FROM sqlite_schema WHERE type = 'table' AND name = 'target'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .map(|count| count == 1)
            })
            .unwrap();
            assert_eq!(found, !starts_present);
        }
    }

    #[test]
    fn snapshot_resource_limits_are_retryable() {
        let ceiling = validate_snapshot_ceiling(SQLITE_SNAPSHOT_MAX_BYTES + 1).unwrap_err();
        assert!(matches!(
            ceiling,
            crate::CaptureError::Io(ref error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
        let disk =
            validate_snapshot_available_space(1, SQLITE_SNAPSHOT_DISK_RESERVE_BYTES).unwrap_err();
        assert!(matches!(
            disk,
            crate::CaptureError::Io(ref error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
    }

    #[cfg(unix)]
    #[test]
    fn private_snapshot_creation_is_owner_only_with_permissive_umask() {
        use std::os::unix::fs::PermissionsExt;

        let parent = tempfile::tempdir().unwrap();
        let old_umask = unsafe { libc::umask(0) };
        let created = (|| -> std::io::Result<_> {
            let dir = create_private_snapshot_dir_in(parent.path())?;
            let file = dir.path().join("source.db");
            drop(create_private_snapshot_file(&file)?);
            Ok((dir, file))
        })();
        unsafe {
            libc::umask(old_umask);
        }
        let (dir, file) = created.unwrap();

        assert_eq!(
            fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    fn real_wal_writer(path: &Path) -> Connection {
        let writer = Connection::open(path).unwrap();
        writer
            .execute_batch(
                "CREATE TABLE entries (id INTEGER PRIMARY KEY, value TEXT);
                 INSERT INTO entries VALUES (1, 'alpha');
                 PRAGMA journal_mode = WAL;
                 PRAGMA wal_autocheckpoint = 0;
                 PRAGMA wal_checkpoint(TRUNCATE);",
            )
            .unwrap();
        writer
            .execute("UPDATE entries SET value = 'omega' WHERE id = 1", [])
            .unwrap();
        assert!(sidecar(path, "-wal").is_file());
        writer
    }

    struct HotJournalFixture {
        _temp: tempfile::TempDir,
        db: std::path::PathBuf,
    }

    fn real_hot_journal_fixture() -> HotJournalFixture {
        let source_temp = tempfile::tempdir().unwrap();
        let source = source_temp.path().join("source.db");
        let writer = Connection::open(&source).unwrap();
        writer
            .execute_batch(
                "PRAGMA page_size = 512;
                 PRAGMA journal_mode = DELETE;
                 PRAGMA synchronous = FULL;
                 PRAGMA cache_size = 1;
                 PRAGMA cache_spill = 1;
                 CREATE TABLE entries (id INTEGER PRIMARY KEY, value TEXT);",
            )
            .unwrap();
        let value = "a".repeat(2048);
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        for id in 0..256_i64 {
            writer
                .execute("INSERT INTO entries VALUES (?1, ?2)", params![id, &value])
                .unwrap();
        }
        writer.execute_batch("COMMIT").unwrap();
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        writer
            .execute("UPDATE entries SET value = replace(value, 'a', 'b')", [])
            .unwrap();
        let journal = sidecar(&source, "-journal");
        let journal_bytes = fs::read(&journal).unwrap();
        assert!(journal_bytes.starts_with(&super::super::sqlite_observation::JOURNAL_MAGIC));

        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("hot.db");
        fs::copy(&source, &db).unwrap();
        fs::write(sidecar(&db, "-journal"), journal_bytes).unwrap();
        writer.execute_batch("ROLLBACK").unwrap();

        HotJournalFixture { _temp: temp, db }
    }

    fn append_super_journal_trailer(journal: &Path, name: &[u8]) {
        let mut file = fs::OpenOptions::new().append(true).open(journal).unwrap();
        file.write_all(&1_048_577_u32.to_be_bytes()).unwrap();
        file.write_all(name).unwrap();
        file.write_all(&(name.len() as u32).to_be_bytes()).unwrap();
        let checksum = name.iter().fold(0_u32, |sum, byte| {
            sum.wrapping_add((*byte as i8 as i32) as u32)
        });
        file.write_all(&checksum.to_be_bytes()).unwrap();
        file.write_all(&super::super::sqlite_observation::JOURNAL_MAGIC)
            .unwrap();
        file.sync_all().unwrap();
    }

    #[cfg(unix)]
    fn native_path_bytes(path: &Path) -> Vec<u8> {
        use std::os::unix::ffi::OsStrExt;

        path.as_os_str().as_bytes().to_vec()
    }

    #[cfg(not(unix))]
    fn native_path_bytes(path: &Path) -> Vec<u8> {
        path.to_str().unwrap().as_bytes().to_vec()
    }

    fn sidecar(path: &Path, suffix: &str) -> std::path::PathBuf {
        let mut sidecar = path.as_os_str().to_owned();
        sidecar.push(suffix);
        sidecar.into()
    }
}
