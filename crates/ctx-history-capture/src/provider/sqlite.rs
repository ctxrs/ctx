use std::{
    cell::RefCell,
    collections::BTreeSet,
    fs::{self, File},
    io::{self, Read, Write},
    ops::Deref,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use ctx_history_store::{
    ensure_indexing_disk_headroom, ExternalIndexingCopyLease, IndexingAdmission, IndexingIoPacer,
    Store, INDEXING_WAL_DELTA_BYTES,
};
use rusqlite::{Connection, OpenFlags};
use serde_json::json;
use tempfile::TempDir;
use url::Url;

use crate::common::io::ensure_regular_provider_transcript_file;
use crate::compute_payload_hash;

use crate::{CaptureError, Result, MAX_PROVIDER_SQLITE_VALUE_BYTES};

// Keep 32 handoff checks per byte-bounded indexing slice while avoiding tiny
// read/write syscalls for multi-gigabyte provider snapshots.
const PROVIDER_COPY_BUFFER_BYTES: usize = INDEXING_WAL_DELTA_BYTES as usize / 32;

thread_local! {
    static PROVIDER_COPY_PACER: RefCell<Option<IndexingIoPacer>> = const { RefCell::new(None) };
    static PROVIDER_COPY_ADMISSION: RefCell<Option<IndexingAdmission>> = const { RefCell::new(None) };
}

pub(crate) struct ProviderSourceIoPacingScope {
    previous_pacer: Option<IndexingIoPacer>,
    previous_admission: Option<IndexingAdmission>,
}

impl ProviderSourceIoPacingScope {
    pub(crate) fn enter(store: &Store) -> Self {
        let pacer = store.indexing_io_pacer();
        let previous_pacer = PROVIDER_COPY_PACER.with(|slot| slot.replace(Some(pacer)));
        let previous_admission =
            PROVIDER_COPY_ADMISSION.with(|slot| slot.replace(store.indexing_admission()));
        Self {
            previous_pacer,
            previous_admission,
        }
    }
}

impl Drop for ProviderSourceIoPacingScope {
    fn drop(&mut self) {
        PROVIDER_COPY_PACER.with(|slot| {
            slot.replace(self.previous_pacer.take());
        });
        PROVIDER_COPY_ADMISSION.with(|slot| {
            slot.replace(self.previous_admission.take());
        });
    }
}

trait ProviderCopyPolicy {
    fn begin_slice(&mut self) -> io::Result<()>;
    fn should_rotate(&mut self, started: Instant, bytes: u64) -> bool;
    fn finish_slice(&mut self, active: Duration, bytes: u64) -> io::Result<()>;
}

struct IndexingProviderCopyPolicy {
    pacer: IndexingIoPacer,
    admission: Option<IndexingAdmission>,
    destination: PathBuf,
    lease: Option<ExternalIndexingCopyLease>,
}

impl ProviderCopyPolicy for IndexingProviderCopyPolicy {
    fn begin_slice(&mut self) -> io::Result<()> {
        debug_assert!(self.lease.is_none());
        if let Some(admission) = &self.admission {
            self.lease = Some(
                admission
                    .acquire_external_copy_slice(
                        &self.destination,
                        INDEXING_WAL_DELTA_BYTES,
                        "provider SQLite snapshot copy",
                    )
                    .map_err(io::Error::other)?,
            );
        } else {
            ensure_indexing_disk_headroom(
                &self.destination,
                INDEXING_WAL_DELTA_BYTES,
                "provider SQLite snapshot copy",
            )
            .map_err(io::Error::other)?;
        }
        Ok(())
    }

    fn should_rotate(&mut self, started: Instant, bytes: u64) -> bool {
        self.pacer.source_io_slice_should_rotate(started, bytes)
    }

    fn finish_slice(&mut self, active: Duration, bytes: u64) -> io::Result<()> {
        self.lease.take();
        self.pacer.finish_source_io_slice(active, bytes);
        Ok(())
    }
}

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
    let sidecars = sqlite_existing_regular_sidecar_paths(path)?;
    if sidecars.is_empty() {
        let uri = sqlite_immutable_uri(path)?;
        let conn = Connection::open_with_flags(
            uri.as_str(),
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_URI,
        )?;
        return Ok(ReadOnlySqliteConnection {
            conn,
            _snapshot_dir: None,
        });
    }

    // Read-only SQLite connections can still update live WAL shared-memory files.
    // Copy the DB plus sidecars first so imports see committed WAL content without
    // mutating provider-owned history.
    let snapshot_parent = std::env::temp_dir();
    let snapshot_bytes = std::iter::once(path)
        .chain(sidecars.iter().map(PathBuf::as_path))
        .try_fold(0_u64, |total, path| {
            fs::metadata(path).map(|metadata| total.saturating_add(metadata.len()))
        })?;
    let reservation_path = snapshot_parent.join(".ctx-provider-sqlite-reservation");
    let admission = PROVIDER_COPY_ADMISSION.with(|slot| slot.borrow().clone());
    let _directory_lease = if let Some(admission) = admission {
        Some(
            admission
                .acquire_external_copy_slice(
                    &reservation_path,
                    snapshot_bytes,
                    "provider SQLite snapshot",
                )
                .map_err(CaptureError::from)?,
        )
    } else {
        ensure_indexing_disk_headroom(
            &reservation_path,
            snapshot_bytes,
            "provider SQLite snapshot",
        )?;
        None
    };
    let snapshot_dir = tempfile::Builder::new()
        .prefix("ctx-provider-sqlite-")
        .tempdir_in(&snapshot_parent)?;
    drop(_directory_lease);
    let snapshot_path = snapshot_dir.path().join(path.file_name().ok_or_else(|| {
        CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "provider SQLite path has no file name",
        }
    })?);
    copy_provider_sqlite_file(path, &snapshot_path)?;
    for sidecar in sidecars {
        let sidecar_name =
            sidecar
                .file_name()
                .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                    path: sidecar.clone(),
                    reason: "provider SQLite sidecar path has no file name",
                })?;
        copy_provider_sqlite_file(&sidecar, &snapshot_dir.path().join(sidecar_name))?;
    }
    let conn = Connection::open_with_flags(
        &snapshot_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    Ok(ReadOnlySqliteConnection {
        conn,
        _snapshot_dir: Some(snapshot_dir),
    })
}

fn copy_provider_sqlite_file(source: &Path, destination: &Path) -> Result<u64> {
    let destination_parent = destination
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let mut source = File::open(source)?;
    let pacer = PROVIDER_COPY_PACER
        .with(|slot| slot.borrow().clone())
        .unwrap_or_default()
        .for_destination_filesystem(&destination_parent);
    let mut policy = IndexingProviderCopyPolicy {
        pacer,
        admission: PROVIDER_COPY_ADMISSION.with(|slot| slot.borrow().clone()),
        destination: destination.to_path_buf(),
        lease: None,
    };
    policy.begin_slice()?;
    let mut destination = File::create(destination)?;
    copy_with_active_policy(&mut source, &mut destination, &mut policy).map_err(CaptureError::from)
}

#[cfg(test)]
fn copy_with_policy<R: Read, W: Write, P: ProviderCopyPolicy>(
    source: &mut R,
    destination: &mut W,
    policy: &mut P,
) -> io::Result<u64> {
    policy.begin_slice()?;
    copy_with_active_policy(source, destination, policy)
}

fn copy_with_active_policy<R: Read, W: Write, P: ProviderCopyPolicy>(
    source: &mut R,
    destination: &mut W,
    policy: &mut P,
) -> io::Result<u64> {
    let mut buffer = vec![0_u8; PROVIDER_COPY_BUFFER_BYTES];
    let mut total_bytes = 0_u64;
    let mut slice_bytes = 0_u64;
    let mut slice_started = Instant::now();
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            destination.flush()?;
            if slice_bytes > 0 {
                policy.finish_slice(slice_started.elapsed(), slice_bytes)?;
            }
            return Ok(total_bytes);
        }
        destination.write_all(&buffer[..read])?;
        let read = read as u64;
        total_bytes = total_bytes.saturating_add(read);
        slice_bytes = slice_bytes.saturating_add(read);
        if policy.should_rotate(slice_started, slice_bytes) {
            destination.flush()?;
            policy.finish_slice(slice_started.elapsed(), slice_bytes)?;
            slice_started = Instant::now();
            slice_bytes = 0;
            policy.begin_slice()?;
        }
    }
}

fn sqlite_existing_regular_sidecar_paths(path: &Path) -> Result<Vec<PathBuf>> {
    let mut sidecars = Vec::new();
    for sidecar in sqlite_sidecar_paths(path) {
        match sidecar.symlink_metadata() {
            Ok(metadata) if metadata.file_type().is_file() => sidecars.push(sidecar),
            Ok(_) => {
                return Err(CaptureError::InvalidProviderTranscriptPath {
                    path: sidecar,
                    reason: "provider SQLite sidecar is not a regular file",
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(CaptureError::Io(error)),
        }
    }
    Ok(sidecars)
}

fn sqlite_sidecar_paths(path: &Path) -> Vec<PathBuf> {
    ["-wal", "-shm", "-journal"]
        .into_iter()
        .map(|suffix| {
            let mut sidecar = path.as_os_str().to_os_string();
            sidecar.push(suffix);
            PathBuf::from(sidecar)
        })
        .collect()
}

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
    use std::{fs, io, io::Cursor, sync::mpsc, thread};

    use ctx_history_store::Store;
    use rusqlite::{params, types::Value as SqlValue, Connection};

    use super::{
        copy_provider_sqlite_file, copy_with_policy, optional_text_column_expr,
        optional_timestamp_millis_expr, BTreeSet, Duration, Instant, ProviderCopyPolicy,
        PROVIDER_COPY_ADMISSION, PROVIDER_COPY_BUFFER_BYTES,
    };

    struct CountingCopyPolicy {
        rotate_bytes: u64,
        finished_slices: Vec<u64>,
    }

    impl ProviderCopyPolicy for CountingCopyPolicy {
        fn begin_slice(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn should_rotate(&mut self, _started: Instant, bytes: u64) -> bool {
            bytes >= self.rotate_bytes
        }

        fn finish_slice(&mut self, _active: Duration, bytes: u64) -> io::Result<()> {
            self.finished_slices.push(bytes);
            Ok(())
        }
    }

    #[test]
    fn synthetic_snapshot_copy_is_counted_and_throttled_in_bounded_slices() {
        let source = (0..PROVIDER_COPY_BUFFER_BYTES * 10 + 123)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let mut reader = Cursor::new(&source);
        let mut destination = Vec::new();
        let rotate_bytes = (PROVIDER_COPY_BUFFER_BYTES * 3) as u64;
        let mut policy = CountingCopyPolicy {
            rotate_bytes,
            finished_slices: Vec::new(),
        };

        let copied = copy_with_policy(&mut reader, &mut destination, &mut policy).unwrap();

        assert_eq!(copied, source.len() as u64);
        assert_eq!(destination, source);
        assert_eq!(policy.finished_slices.len(), 4);
        assert!(policy
            .finished_slices
            .iter()
            .all(|bytes| *bytes <= rotate_bytes));
    }

    #[test]
    fn provider_copy_waits_for_canonical_admission_before_destination_creation() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("provider.sqlite");
        let destination = temp.path().join("snapshot").join("provider.sqlite");
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::write(&source, vec![7_u8; PROVIDER_COPY_BUFFER_BYTES * 2]).unwrap();

        let store = Store::open(temp.path().join("work.sqlite")).unwrap();
        store.begin_immediate_batch().unwrap();
        let admission = store.indexing_admission().unwrap();
        let worker_destination = destination.clone();
        let (result_tx, result_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            PROVIDER_COPY_ADMISSION.with(|slot| {
                slot.replace(Some(admission));
            });
            result_tx
                .send(copy_provider_sqlite_file(&source, &worker_destination))
                .unwrap();
        });

        thread::sleep(Duration::from_millis(150));
        assert!(
            !destination.exists(),
            "snapshot destination was created before canonical admission"
        );
        store.rollback_batch().unwrap();
        assert_eq!(
            result_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("provider copy did not continue after admission release")
                .unwrap(),
            (PROVIDER_COPY_BUFFER_BYTES * 2) as u64
        );
        worker.join().unwrap();
        assert!(destination.exists());
    }

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
}
