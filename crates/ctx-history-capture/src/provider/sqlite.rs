use std::{
    collections::BTreeSet,
    fs::{File, OpenOptions},
    io,
    ops::Deref,
    path::Path,
};

use rusqlite::{Connection, OpenFlags};
use serde_json::json;
use tempfile::TempDir;
use url::Url;

use crate::common::io::ensure_regular_provider_transcript_file;
use crate::compute_payload_hash;
use crate::provider::sqlite_observation::{
    observe_sqlite_source_generation, SQLITE_GENERATION_MAX_ATTEMPTS,
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
    _snapshot_dir: Option<TempDir>,
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
    let immutable = open_sqlite_immutable_main(path)?;
    let main_result = predicate(&immutable);
    if matches!(main_result, Ok(true)) {
        return Ok(true);
    }

    let generation = observe_sqlite_source_generation(path)?;
    if !generation.requires_snapshot() {
        return main_result.map_err(CaptureError::from);
    }
    let snapshot = open_stable_sqlite_snapshot(path)?;
    predicate(&snapshot).map_err(CaptureError::from)
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
    for attempt in 1..=SQLITE_GENERATION_MAX_ATTEMPTS {
        let before = observe_sqlite_source_generation(path)?;
        if !before.requires_snapshot() {
            return open_sqlite_immutable_main(path);
        }

        let snapshot_dir = tempfile::Builder::new()
            .prefix("ctx-provider-sqlite-")
            .tempdir()?;
        record_snapshot_attempt();
        let mut raced = false;
        for source in before.snapshot_files() {
            let file_name = source.path().file_name().ok_or_else(|| {
                CaptureError::InvalidProviderTranscriptPath {
                    path: source.path().to_path_buf(),
                    reason: "provider SQLite path has no file name",
                }
            })?;
            match copy_sqlite_snapshot_file(source.path(), &snapshot_dir.path().join(file_name)) {
                Ok(_) => record_snapshot_copy(),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    raced = true;
                    break;
                }
                Err(error) => return Err(CaptureError::Io(error)),
            }
        }
        if raced {
            continue;
        }
        run_snapshot_test_hook(path, attempt);
        let after = match observe_sqlite_source_generation(path) {
            Ok(after) => after,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
            Err(error) => return Err(CaptureError::Io(error)),
        };
        if before != after {
            continue;
        }

        let snapshot_path = snapshot_dir.path().join(path.file_name().ok_or_else(|| {
            CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "provider SQLite path has no file name",
            }
        })?);
        // Recovery, WAL-index creation, and any journal cleanup happen only in
        // this ctx-owned directory. The connection is query-only before adapters use it.
        let conn = Connection::open_with_flags(
            &snapshot_path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let _: i64 = conn.query_row("PRAGMA schema_version", [], |row| row.get(0))?;
        conn.pragma_update(None, "query_only", true)?;
        return Ok(ReadOnlySqliteConnection {
            conn,
            _snapshot_dir: Some(snapshot_dir),
        });
    }
    Err(CaptureError::Io(std::io::Error::new(
        std::io::ErrorKind::WouldBlock,
        format!(
            "SQLite source generation did not stabilize after {SQLITE_GENERATION_MAX_ATTEMPTS} copy attempts: {}",
            path.display()
        ),
    )))
}

fn copy_sqlite_snapshot_file(source: &Path, destination: &Path) -> io::Result<()> {
    let mut source = File::open(source)?;
    let mut destination = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    io::copy(&mut source, &mut destination)?;
    Ok(())
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
pub(crate) fn install_sqlite_snapshot_test_hook(
    hook: impl FnMut(&Path, usize) + 'static,
) -> SqliteSnapshotTestHookGuard {
    SNAPSHOT_TEST_HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
    SqliteSnapshotTestHookGuard
}

#[cfg(test)]
fn record_snapshot_attempt() {
    SNAPSHOT_TEST_METRICS.with(|metrics| {
        let mut value = metrics.get();
        value.attempts += 1;
        metrics.set(value);
    });
}

#[cfg(not(test))]
fn record_snapshot_attempt() {}

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
    use std::{fs, path::Path};

    use rusqlite::{params, types::Value as SqlValue, Connection};

    use super::{
        install_sqlite_snapshot_test_hook, open_sqlite_readonly_source, optional_text_column_expr,
        optional_timestamp_millis_expr, take_sqlite_snapshot_test_metrics, BTreeSet,
        SqliteSnapshotTestMetrics,
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
    fn disappearing_wal_retries_as_a_new_generation() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("source.db");
        drop(Connection::open(&db).unwrap());
        let wal = sidecar(&db, "-wal");
        fs::write(&wal, synthetic_wal()).unwrap();
        let removed_wal = wal.clone();
        let _hook = install_sqlite_snapshot_test_hook(move |_, attempt| {
            if attempt == 1 {
                fs::remove_file(&removed_wal).unwrap();
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

    fn synthetic_wal() -> Vec<u8> {
        let page_size = 512_u32;
        let mut bytes = vec![0_u8; 32 + 24 + page_size as usize];
        bytes[0..4].copy_from_slice(&0x377f_0682_u32.to_be_bytes());
        bytes[8..12].copy_from_slice(&page_size.to_be_bytes());
        bytes[32..36].copy_from_slice(&1_u32.to_be_bytes());
        bytes[36..40].copy_from_slice(&1_u32.to_be_bytes());
        bytes
    }

    fn sidecar(path: &Path, suffix: &str) -> std::path::PathBuf {
        let mut sidecar = path.as_os_str().to_owned();
        sidecar.push(suffix);
        sidecar.into()
    }
}
