use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    ops::Deref,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{limits::Limit, Connection, OpenFlags};
use serde_json::json;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use url::Url;

use crate::common::io::ensure_regular_provider_transcript_file;
use crate::compute_payload_hash;
use crate::provider_sources::{observe_ordinary_file, OrdinaryFileObservation};

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

/// Temporarily lifts SQLite's value-length limit for metadata-only preflight queries.
///
/// The provider limit is restored exactly when this guard is dropped, including while
/// unwinding. Raw value hydration must run after the guard leaves scope.
#[must_use = "the SQLite length limit is restored when this guard is dropped"]
pub(crate) struct SqliteLengthPreflightGuard<'connection> {
    conn: &'connection Connection,
    prior_limit: i32,
}

impl<'connection> SqliteLengthPreflightGuard<'connection> {
    pub(crate) fn new(conn: &'connection Connection) -> Self {
        Self {
            conn,
            prior_limit: conn.set_limit(Limit::SQLITE_LIMIT_LENGTH, i32::MAX),
        }
    }
}

impl Drop for SqliteLengthPreflightGuard<'_> {
    fn drop(&mut self) {
        self.conn
            .set_limit(Limit::SQLITE_LIMIT_LENGTH, self.prior_limit);
    }
}

#[derive(Clone, PartialEq, Eq)]
struct ProviderSqliteFrozenFileMetadata {
    length: u64,
    modified: SystemTime,
    readonly: bool,
    device: Option<u64>,
    inode: Option<u64>,
    change_token: [u8; 32],
}

impl ProviderSqliteFrozenFileMetadata {
    fn read(path: &Path, invalid_reason: &'static str) -> Result<Self> {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
            return Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: invalid_reason,
            });
        }
        let observation = observe_ordinary_file(path)?;
        if metadata.len() != observation.len()
            || metadata.modified().ok() != Some(observation.modified_at())
        {
            return Err(CaptureError::SourceChangedDuringCapture);
        }
        let change_token = sqlite_component_change_token(path, &observation)?;
        Self::from_metadata(&metadata, change_token)
    }

    fn read_optional(path: &Path, invalid_reason: &'static str) -> Result<Option<Self>> {
        match fs::symlink_metadata(path) {
            Ok(metadata)
                if metadata.file_type().is_file() && !metadata.file_type().is_symlink() =>
            {
                Self::read(path, invalid_reason).map(Some)
            }
            Ok(_) => Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: invalid_reason,
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(CaptureError::Io(error)),
        }
    }

    fn from_metadata(metadata: &fs::Metadata, change_token: [u8; 32]) -> Result<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        if !metadata.file_type().is_file() {
            return Err(CaptureError::InvalidPayload(
                "provider SQLite source component is not a regular file".to_owned(),
            ));
        }
        Ok(Self {
            length: metadata.len(),
            modified: metadata.modified()?,
            readonly: metadata.permissions().readonly(),
            #[cfg(unix)]
            device: Some(metadata.dev()),
            #[cfg(not(unix))]
            device: None,
            #[cfg(unix)]
            inode: Some(metadata.ino()),
            #[cfg(not(unix))]
            inode: None,
            change_token,
        })
    }

    fn revision_component(&self) -> String {
        let (sign, seconds, nanos) = match self.modified.duration_since(UNIX_EPOCH) {
            Ok(duration) => ('+', duration.as_secs(), duration.subsec_nanos()),
            Err(error) => {
                let duration = error.duration();
                ('-', duration.as_secs(), duration.subsec_nanos())
            }
        };
        format!(
            "length={};modified={sign}{seconds}.{nanos:09};readonly={};device={};inode={};change={}",
            self.length,
            self.readonly,
            self.device
                .map_or_else(|| "none".to_owned(), |value| value.to_string()),
            self.inode
                .map_or_else(|| "none".to_owned(), |value| value.to_string()),
            hex_token(&self.change_token),
        )
    }
}

const SQLITE_COMPONENT_TOKEN_DOMAIN: &[u8] = b"ctx-provider-sqlite-component-v1\0";
const SQLITE_HEADER_BYTES: usize = 100;
const SQLITE_WAL_HEADER_BYTES: usize = 32;
const SQLITE_WAL_FRAME_HEADER_BYTES: usize = 24;

pub(crate) fn sqlite_component_change_token(
    path: &Path,
    observation: &OrdinaryFileObservation,
) -> Result<[u8; 32]> {
    let mut file = open_sqlite_component_without_following(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() || metadata.len() != observation.len() {
        return Err(CaptureError::SourceChangedDuringCapture);
    }

    let prefix_len = usize::try_from(observation.len().min(SQLITE_HEADER_BYTES as u64))
        .map_err(|_| CaptureError::SourceChangedDuringCapture)?;
    let mut prefix = vec![0_u8; prefix_len];
    file.read_exact(&mut prefix)?;

    let mut hasher = Sha256::new();
    hasher.update(SQLITE_COMPONENT_TOKEN_DOMAIN);
    hasher.update(observation.len().to_le_bytes());
    hasher.update(observation.token());
    hasher.update(&prefix);
    if path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with("-wal"))
    {
        if let Some(frame_header) =
            sqlite_wal_last_frame_header(&mut file, observation.len(), &prefix)?
        {
            hasher.update(frame_header);
        }
    }

    let current = observe_ordinary_file(path)?;
    if &current != observation {
        return Err(CaptureError::SourceChangedDuringCapture);
    }
    Ok(hasher.finalize().into())
}

fn sqlite_wal_last_frame_header(
    file: &mut File,
    length: u64,
    prefix: &[u8],
) -> Result<Option<[u8; SQLITE_WAL_FRAME_HEADER_BYTES]>> {
    if prefix.len() < SQLITE_WAL_HEADER_BYTES {
        return Ok(None);
    }
    let raw_page_size = u32::from_be_bytes(prefix[8..12].try_into().map_err(|_| {
        CaptureError::InvalidPayload("invalid SQLite WAL page-size header".to_owned())
    })?);
    let page_size = match raw_page_size {
        1 => 65_536_u64,
        512..=65_536 if raw_page_size.is_power_of_two() => u64::from(raw_page_size),
        _ => return Ok(None),
    };
    let frame_size = page_size.saturating_add(SQLITE_WAL_FRAME_HEADER_BYTES as u64);
    let frames_bytes = length.saturating_sub(SQLITE_WAL_HEADER_BYTES as u64);
    if frames_bytes < frame_size || !frames_bytes.is_multiple_of(frame_size) {
        return Ok(None);
    }

    file.seek(SeekFrom::Start(length - frame_size))?;
    let mut header = [0_u8; SQLITE_WAL_FRAME_HEADER_BYTES];
    file.read_exact(&mut header)?;
    Ok(Some(header))
}

#[cfg(unix)]
fn open_sqlite_component_without_following(path: &Path) -> Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    Ok(OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?)
}

#[cfg(target_os = "windows")]
fn open_sqlite_component_without_following(path: &Path) -> Result<File> {
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    Ok(OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?)
}

#[cfg(not(any(unix, target_os = "windows")))]
fn open_sqlite_component_without_following(path: &Path) -> Result<File> {
    Ok(File::open(path)?)
}

fn hex_token(token: &[u8; 32]) -> String {
    token.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ProviderSqliteSourceSnapshot {
    database: ProviderSqliteFrozenFileMetadata,
    wal: Option<ProviderSqliteFrozenFileMetadata>,
    shared_memory: Option<ProviderSqliteFrozenFileMetadata>,
    rollback_journal: Option<ProviderSqliteFrozenFileMetadata>,
    source_invalid_reason: &'static str,
    sidecar_invalid_reason: &'static str,
}

impl ProviderSqliteSourceSnapshot {
    pub(crate) fn read(
        path: &Path,
        source_invalid_reason: &'static str,
        sidecar_invalid_reason: &'static str,
    ) -> Result<Self> {
        Ok(Self {
            database: ProviderSqliteFrozenFileMetadata::read(path, source_invalid_reason)?,
            wal: ProviderSqliteFrozenFileMetadata::read_optional(
                &sqlite_sidecar_path(path, "-wal"),
                sidecar_invalid_reason,
            )?,
            shared_memory: ProviderSqliteFrozenFileMetadata::read_optional(
                &sqlite_sidecar_path(path, "-shm"),
                sidecar_invalid_reason,
            )?,
            rollback_journal: ProviderSqliteFrozenFileMetadata::read_optional(
                &sqlite_sidecar_path(path, "-journal"),
                sidecar_invalid_reason,
            )?,
            source_invalid_reason,
            sidecar_invalid_reason,
        })
    }

    pub(crate) fn revision_component(&self) -> String {
        format!(
            "database={};wal={};shm={};journal={}",
            self.database.revision_component(),
            optional_sqlite_revision_component(self.wal.as_ref()),
            optional_sqlite_revision_component(self.shared_memory.as_ref()),
            optional_sqlite_revision_component(self.rollback_journal.as_ref()),
        )
    }

    pub(crate) fn revalidate(&self, path: &Path) -> Result<bool> {
        match Self::read(
            path,
            self.source_invalid_reason,
            self.sidecar_invalid_reason,
        ) {
            Ok(current) => Ok(current == *self),
            Err(CaptureError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(false)
            }
            Err(CaptureError::InvalidProviderTranscriptPath { .. }) => Ok(false),
            Err(error) => Err(error),
        }
    }
}

fn optional_sqlite_revision_component(
    metadata: Option<&ProviderSqliteFrozenFileMetadata>,
) -> String {
    metadata.map_or_else(|| "absent".to_owned(), |value| value.revision_component())
}

fn sqlite_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut sidecar = path.as_os_str().to_os_string();
    sidecar.push(suffix);
    PathBuf::from(sidecar)
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

pub(crate) fn open_provider_sqlite_readonly(path: &Path) -> Result<ReadOnlySqliteConnection> {
    let conn = open_sqlite_readonly_source(path)?;
    let value_limit = i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).map_err(|_| {
        CaptureError::InvalidPayload(format!(
            "provider SQLite value byte limit is unrepresentable: {MAX_PROVIDER_SQLITE_VALUE_BYTES}"
        ))
    })?;
    conn.set_limit(Limit::SQLITE_LIMIT_LENGTH, value_limit);
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.pragma_update(None, "query_only", true)?;
    Ok(conn)
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
    let snapshot_dir = tempfile::Builder::new()
        .prefix("ctx-provider-sqlite-")
        .tempdir()?;
    let snapshot_path = snapshot_dir.path().join(path.file_name().ok_or_else(|| {
        CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "provider SQLite path has no file name",
        }
    })?);
    fs::copy(path, &snapshot_path)?;
    for sidecar in sidecars {
        let sidecar_name =
            sidecar
                .file_name()
                .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                    path: sidecar.clone(),
                    reason: "provider SQLite sidecar path has no file name",
                })?;
        fs::copy(&sidecar, snapshot_dir.path().join(sidecar_name))?;
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

pub(crate) fn with_sqlite_read_snapshot<T>(
    conn: &Connection,
    read: impl FnOnce() -> Result<T>,
) -> Result<T> {
    // Keep provider snapshots scoped to one bounded read. Callers can release the
    // snapshot before writing to the ctx Store, even when they reuse the connection.
    conn.execute_batch("begin")?;
    let read_result = read();
    let rollback_result = conn.execute_batch("rollback");
    match (read_result, rollback_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (_, Err(error)) => Err(CaptureError::from(error)),
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

pub(crate) fn sqlite_schema_fingerprint(conn: &Connection) -> Result<String> {
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
    use std::{
        fs,
        panic::{catch_unwind, AssertUnwindSafe},
    };

    use rusqlite::{limits::Limit, params, types::Value as SqlValue, Connection};

    use super::{
        optional_text_column_expr, optional_timestamp_millis_expr, BTreeSet,
        ProviderSqliteSourceSnapshot, SqliteLengthPreflightGuard,
    };

    const TEST_LENGTH_LIMIT: i32 = 16 * 1024;

    fn connection_with_test_length_limit() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.set_limit(Limit::SQLITE_LIMIT_LENGTH, TEST_LENGTH_LIMIT);
        assert_eq!(conn.limit(Limit::SQLITE_LIMIT_LENGTH), TEST_LENGTH_LIMIT);
        conn
    }

    #[test]
    fn length_preflight_guard_restores_exact_prior_limit_after_success() {
        let conn = connection_with_test_length_limit();
        {
            let _guard = SqliteLengthPreflightGuard::new(&conn);
            let value: i64 = conn.query_row("SELECT 1", [], |row| row.get(0)).unwrap();
            assert_eq!(value, 1);
            assert!(conn.limit(Limit::SQLITE_LIMIT_LENGTH) > TEST_LENGTH_LIMIT);
        }
        assert_eq!(conn.limit(Limit::SQLITE_LIMIT_LENGTH), TEST_LENGTH_LIMIT);
    }

    #[test]
    fn length_preflight_guard_restores_exact_prior_limit_after_sqlite_error() {
        let conn = connection_with_test_length_limit();
        let result = {
            let _guard = SqliteLengthPreflightGuard::new(&conn);
            conn.query_row::<i64, _, _>("SELECT missing FROM missing_table", [], |row| row.get(0))
        };
        assert!(result.is_err());
        assert_eq!(conn.limit(Limit::SQLITE_LIMIT_LENGTH), TEST_LENGTH_LIMIT);
    }

    #[test]
    fn length_preflight_guard_restores_nested_prior_limits() {
        let conn = connection_with_test_length_limit();
        let outer_guard = SqliteLengthPreflightGuard::new(&conn);
        let raised_limit = conn.limit(Limit::SQLITE_LIMIT_LENGTH);
        assert!(raised_limit > TEST_LENGTH_LIMIT);
        {
            let _inner_guard = SqliteLengthPreflightGuard::new(&conn);
            assert_eq!(conn.limit(Limit::SQLITE_LIMIT_LENGTH), raised_limit);
        }
        assert_eq!(conn.limit(Limit::SQLITE_LIMIT_LENGTH), raised_limit);
        drop(outer_guard);
        assert_eq!(conn.limit(Limit::SQLITE_LIMIT_LENGTH), TEST_LENGTH_LIMIT);
    }

    #[test]
    fn length_preflight_guard_restores_exact_prior_limit_while_unwinding() {
        let conn = connection_with_test_length_limit();
        let unwind = catch_unwind(AssertUnwindSafe(|| {
            let _guard = SqliteLengthPreflightGuard::new(&conn);
            panic!("exercise SQLite length preflight guard drop");
        }));
        assert!(unwind.is_err());
        assert_eq!(conn.limit(Limit::SQLITE_LIMIT_LENGTH), TEST_LENGTH_LIMIT);
    }

    #[test]
    fn provider_sqlite_snapshot_tracks_database_and_sidecars() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let database = temp.path().join("provider.sqlite");
        let wal = temp.path().join("provider.sqlite-wal");
        fs::write(&database, b"database-v1").unwrap();
        fs::write(&wal, b"wal-v1").unwrap();

        let snapshot = ProviderSqliteSourceSnapshot::read(
            &database,
            "test database must be regular",
            "test sidecar must be regular",
        )
        .unwrap();
        assert!(snapshot.revalidate(&database).unwrap());
        assert!(snapshot.revision_component().contains("database=length=11"));
        assert!(snapshot.revision_component().contains("wal=length=6"));

        fs::write(&wal, b"wal-v2-with-more-bytes").unwrap();
        assert!(!snapshot.revalidate(&database).unwrap());
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
