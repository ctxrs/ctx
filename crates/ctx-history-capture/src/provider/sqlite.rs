use std::{
    collections::BTreeSet,
    ffi::OsStr,
    fs::File,
    io::{Read, Seek, SeekFrom},
    ops::Deref,
    path::Path,
};

use rusqlite::{limits::Limit, Connection};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::common::io::ProviderSourceRoot;
use crate::compute_payload_hash;
use crate::provider_sources::{
    observe_ordinary_file, open_ordinary_file_without_following,
    open_root_handle_sqlite_source_snapshot, retain_sqlite_source_directory_authority,
    OrdinaryFileObservation, SqliteSourceAccessError, SqliteSourceEvidence,
    SqliteSourceReadSnapshot,
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

const SQLITE_COMPONENT_TOKEN_DOMAIN: &[u8] = b"ctx-provider-sqlite-component-v1\0";
const SQLITE_HEADER_BYTES: usize = 100;
const SQLITE_WAL_HEADER_BYTES: usize = 32;
const SQLITE_WAL_FRAME_HEADER_BYTES: usize = 24;

pub(crate) fn sqlite_component_change_token(
    path: &Path,
    observation: &OrdinaryFileObservation,
) -> Result<[u8; 32]> {
    let mut file = open_ordinary_file_without_following(path)?;
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

#[cfg(test)]
fn hex_token(token: &[u8; 32]) -> String {
    token.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Clone, PartialEq, Eq)]
#[cfg(test)]
pub(crate) struct ProviderSqliteSourceSnapshot {
    data_root: std::path::PathBuf,
    evidence: SqliteSourceEvidence,
    source_invalid_reason: &'static str,
    sidecar_invalid_reason: &'static str,
}

#[cfg(test)]
impl ProviderSqliteSourceSnapshot {
    pub(crate) fn read(
        data_root: &Path,
        path: &Path,
        source_invalid_reason: &'static str,
        sidecar_invalid_reason: &'static str,
    ) -> Result<Self> {
        Ok(Self {
            data_root: data_root.to_path_buf(),
            evidence: read_sqlite_source_evidence(data_root, path)?,
            source_invalid_reason,
            sidecar_invalid_reason,
        })
    }

    #[cfg(test)]
    pub(crate) fn revision_component(&self) -> String {
        format!(
            "identity={};length={};revision={}",
            hex_token(self.evidence.identity()),
            self.evidence.length(),
            hex_token(self.evidence.revision()),
        )
    }

    pub(crate) fn revalidate(&self, path: &Path) -> Result<bool> {
        match Self::read(
            &self.data_root,
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

#[cfg(test)]
fn read_sqlite_source_evidence(data_root: &Path, path: &Path) -> Result<SqliteSourceEvidence> {
    RootAuthorizedProviderSqliteSnapshot::open(data_root, path)?.finish()
}

struct RootAuthorizedProviderSqliteSnapshot {
    snapshot: Option<SqliteSourceReadSnapshot>,
    authority_root: ProviderSourceRoot,
}

impl RootAuthorizedProviderSqliteSnapshot {
    fn open(data_root: &Path, path: &Path) -> Result<Self> {
        let (parent_path, database_name) = sqlite_parent_and_leaf(path)?;
        let admission_root = ProviderSourceRoot::open(parent_path)?;
        let parent = admission_root.directory()?;
        let parent_handle = parent.try_clone_authority_handle()?;
        let sqlite_authority =
            retain_sqlite_source_directory_authority(data_root, &parent_handle, parent_path)
                .map_err(map_sqlite_source_access_error)?;
        let snapshot = open_root_handle_sqlite_source_snapshot(&sqlite_authority, database_name)
            .map_err(map_sqlite_source_access_error)?;
        snapshot
            .revalidate()
            .map_err(map_sqlite_source_access_error)?;
        // Retain a second route authority around the already-open snapshot so
        // ancestor replacement is fenced independently from DB-family checks.
        let authority_root = ProviderSourceRoot::open(parent_path)?;
        Ok(Self {
            snapshot: Some(snapshot),
            authority_root,
        })
    }

    fn connection(&self) -> Result<&Connection> {
        self.snapshot
            .as_ref()
            .ok_or(CaptureError::SystemInvariant(
                "provider SQLite source snapshot is inactive",
            ))?
            .connection()
            .map_err(map_sqlite_source_access_error)
    }

    fn finish(mut self) -> Result<SqliteSourceEvidence> {
        let snapshot = self.snapshot.take().ok_or(CaptureError::SystemInvariant(
            "provider SQLite source snapshot is inactive",
        ))?;
        let finish = snapshot.finish().map_err(map_sqlite_source_access_error);
        let root_revalidation = self.authority_root.revalidate();
        match (finish, root_revalidation) {
            (Ok(evidence), Ok(())) => Ok(evidence),
            (Err(error), _) => Err(error),
            (_, Err(error)) => Err(error),
        }
    }
}

impl Drop for RootAuthorizedProviderSqliteSnapshot {
    fn drop(&mut self) {
        if let Some(snapshot) = self.snapshot.take() {
            let _ = snapshot.finish();
            let _ = self.authority_root.revalidate();
        }
    }
}

fn sqlite_parent_and_leaf(path: &Path) -> Result<(&Path, &OsStr)> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "provider SQLite path has no absolute parent directory",
        })?;
    let database_name =
        path.file_name()
            .ok_or_else(|| CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "provider SQLite path has no database leaf name",
            })?;
    Ok((parent, database_name))
}

/// Provider-neutral SQLite read guard.
///
/// Call [`Self::finish`] after the final query and before publishing values
/// read through this connection so source-family and outer-route changes are
/// returned as capture errors.
#[must_use = "call finish() before publishing provider SQLite observations"]
pub(crate) struct ReadOnlySqliteConnection {
    snapshot: Option<RootAuthorizedProviderSqliteSnapshot>,
}

impl ReadOnlySqliteConnection {
    fn connection(&self) -> Result<&Connection> {
        self.snapshot
            .as_ref()
            .ok_or(CaptureError::SystemInvariant(
                "provider SQLite source snapshot is inactive",
            ))?
            .connection()
    }

    #[cfg(test)]
    pub(crate) fn finish(mut self) -> Result<SqliteSourceEvidence> {
        self.snapshot
            .take()
            .ok_or(CaptureError::SystemInvariant(
                "provider SQLite source snapshot is inactive",
            ))?
            .finish()
    }
}

impl Deref for ReadOnlySqliteConnection {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        match self.connection() {
            Ok(connection) => connection,
            Err(_) => inactive_readonly_sqlite_connection(),
        }
    }
}

impl Drop for ReadOnlySqliteConnection {
    fn drop(&mut self) {
        if let Some(snapshot) = self.snapshot.take() {
            let _ = snapshot.finish();
        }
    }
}

pub(crate) fn open_provider_sqlite_readonly(
    data_root: &Path,
    path: &Path,
) -> Result<ReadOnlySqliteConnection> {
    let conn = open_sqlite_readonly_source(data_root, path)?;
    let value_limit = i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).map_err(|_| {
        CaptureError::InvalidPayload(format!(
            "provider SQLite value byte limit is unrepresentable: {MAX_PROVIDER_SQLITE_VALUE_BYTES}"
        ))
    })?;
    let connection = conn.connection()?;
    connection.set_limit(Limit::SQLITE_LIMIT_LENGTH, value_limit);
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    connection.pragma_update(None, "query_only", true)?;
    Ok(conn)
}

pub(crate) fn open_sqlite_readonly_source(
    data_root: &Path,
    path: &Path,
) -> Result<ReadOnlySqliteConnection> {
    let snapshot = RootAuthorizedProviderSqliteSnapshot::open(data_root, path)?;
    Ok(ReadOnlySqliteConnection {
        snapshot: Some(snapshot),
    })
}

#[cold]
fn inactive_readonly_sqlite_connection() -> ! {
    std::process::abort()
}

pub(crate) fn map_sqlite_source_access_error(error: SqliteSourceAccessError) -> CaptureError {
    match error {
        SqliteSourceAccessError::Io { source, .. } => CaptureError::Io(source),
        SqliteSourceAccessError::Sqlite { source, .. } => CaptureError::Sqlite(source),
        SqliteSourceAccessError::UnsafeFile { path, reason } => {
            CaptureError::InvalidProviderTranscriptPath { path, reason }
        }
        SqliteSourceAccessError::ConnectionIdentityMismatch
        | SqliteSourceAccessError::SourceChanged => CaptureError::SourceChangedDuringCapture,
        SqliteSourceAccessError::SnapshotNotActive => {
            CaptureError::SystemInvariant("provider SQLite source snapshot is inactive")
        }
        other => CaptureError::SystemIo {
            operation: "opening a root-authorized provider SQLite snapshot",
            source: std::io::Error::other(other),
        },
    }
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
        collections::BTreeMap,
        ffi::OsString,
        fs,
        panic::{catch_unwind, AssertUnwindSafe},
        path::Path,
    };

    #[cfg(target_os = "linux")]
    use rusqlite::config::DbConfig;
    use rusqlite::{limits::Limit, params, types::Value as SqlValue, Connection};

    use super::{
        open_provider_sqlite_readonly, optional_text_column_expr, optional_timestamp_millis_expr,
        BTreeSet, ProviderSqliteSourceSnapshot, SqliteLengthPreflightGuard, SqliteSourceEvidence,
    };
    #[cfg(target_os = "linux")]
    use crate::Result;

    const TEST_LENGTH_LIMIT: i32 = 16 * 1024;

    fn directory_file_bytes(path: &Path) -> BTreeMap<OsString, Vec<u8>> {
        fs::read_dir(path)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (entry.file_name(), fs::read(entry.path()).unwrap())
            })
            .collect()
    }

    fn connection_with_test_length_limit() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.set_limit(Limit::SQLITE_LIMIT_LENGTH, TEST_LENGTH_LIMIT);
        assert_eq!(conn.limit(Limit::SQLITE_LIMIT_LENGTH), TEST_LENGTH_LIMIT);
        conn
    }

    #[cfg(target_os = "linux")]
    fn create_persistent_wal(path: &Path) {
        let writer = Connection::open(path).unwrap();
        let mode: String = writer
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        writer
            .execute("CREATE TABLE messages (body TEXT NOT NULL)", [])
            .unwrap();
        writer
            .execute("INSERT INTO messages (body) VALUES ('from-wal')", [])
            .unwrap();
        writer
            .set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
            .unwrap();
        drop(writer);
        assert!(path.with_file_name("provider.sqlite-wal").exists());
        assert!(path.with_file_name("provider.sqlite-shm").exists());
    }

    #[cfg(target_os = "linux")]
    fn read_provider_body_with_finish(
        path: &Path,
        before_finish: impl FnOnce(),
    ) -> Result<(String, SqliteSourceEvidence)> {
        let connection =
            open_provider_sqlite_readonly(crate::test_provider_sqlite_data_root(), path)?;
        let body = connection.query_row("SELECT body FROM messages", [], |row| row.get(0))?;
        before_finish();
        let evidence = connection.finish()?;
        Ok((body, evidence))
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

    #[cfg(target_os = "linux")]
    #[test]
    fn provider_sqlite_snapshot_uses_root_bound_guard_evidence() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let database = temp.path().join("provider.sqlite");
        let writer = Connection::open(&database).unwrap();
        writer
            .execute("CREATE TABLE messages (body TEXT NOT NULL)", [])
            .unwrap();
        writer
            .execute("INSERT INTO messages (body) VALUES ('v1')", [])
            .unwrap();
        drop(writer);

        let snapshot = ProviderSqliteSourceSnapshot::read(
            crate::test_provider_sqlite_data_root(),
            &database,
            "test database must be regular",
            "test sidecar must be regular",
        )
        .unwrap();
        assert!(snapshot.revalidate(&database).unwrap());
        assert!(snapshot.revision_component().contains("identity="));
        assert!(snapshot.revision_component().contains(";revision="));

        let writer = Connection::open(&database).unwrap();
        writer
            .execute("INSERT INTO messages (body) VALUES ('v2')", [])
            .unwrap();
        drop(writer);
        assert!(!snapshot.revalidate(&database).unwrap());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn provider_sqlite_opener_retains_the_root_bound_snapshot_guard() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let database = temp.path().join("provider.sqlite");
        let writer = Connection::open(&database).unwrap();
        writer
            .execute("CREATE TABLE messages (body TEXT NOT NULL)", [])
            .unwrap();
        writer
            .execute("INSERT INTO messages (body) VALUES ('guarded')", [])
            .unwrap();
        drop(writer);

        let connection =
            open_provider_sqlite_readonly(crate::test_provider_sqlite_data_root(), &database)
                .unwrap();
        assert!(
            !connection.is_autocommit(),
            "the root-authorized guard must keep its read snapshot pinned"
        );
        let body: String = connection
            .query_row("SELECT body FROM messages", [], |row| row.get(0))
            .unwrap();
        assert_eq!(body, "guarded");
        connection.finish().unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn provider_sqlite_initial_snapshot_succeeds_with_idle_wal_writer() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let database = temp.path().join("provider.sqlite");
        let writer = Connection::open(&database).unwrap();
        writer
            .execute("CREATE TABLE messages (body TEXT NOT NULL)", [])
            .unwrap();
        writer
            .execute("INSERT INTO messages (body) VALUES ('idle-wal')", [])
            .unwrap();
        let mode: String = writer
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        writer.execute_batch("PRAGMA wal_autocheckpoint=0").unwrap();
        assert!(
            !database.with_file_name("provider.sqlite-wal").exists(),
            "the idle writer must not have materialized a WAL pathname"
        );
        let before = directory_file_bytes(temp.path());

        let (body, evidence) = read_provider_body_with_finish(&database, || {}).unwrap();

        assert_eq!(body, "idle-wal");
        assert_eq!(evidence.wal_length(), None);
        assert_eq!(evidence.shared_memory_length(), None);
        assert_eq!(directory_file_bytes(temp.path()), before);
        drop(writer);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn active_source_family_contract_sqlite_reads_active_wal_without_provider_writes() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let database = temp.path().join("provider.sqlite");
        let wal = temp.path().join("provider.sqlite-wal");
        let shared_memory = temp.path().join("provider.sqlite-shm");
        create_persistent_wal(&database);
        let before_database = fs::read(&database).unwrap();
        let before_wal = fs::read(&wal).unwrap();
        let before_shared_memory = fs::read(&shared_memory).unwrap();
        let before_directory = directory_file_bytes(temp.path());

        let source_snapshot = ProviderSqliteSourceSnapshot::read(
            crate::test_provider_sqlite_data_root(),
            &database,
            "test database must be regular",
            "test sidecar must be regular",
        )
        .unwrap();
        assert!(source_snapshot.evidence.wal_length().is_some());
        let (body, evidence) = read_provider_body_with_finish(&database, || {}).unwrap();

        assert_eq!(body, "from-wal");
        assert!(evidence.wal_length().is_some());
        assert!(evidence.shared_memory_length().is_some());
        assert_eq!(fs::read(&database).unwrap(), before_database);
        assert_eq!(fs::read(&wal).unwrap(), before_wal);
        assert_eq!(fs::read(&shared_memory).unwrap(), before_shared_memory);
        assert_eq!(directory_file_bytes(temp.path()), before_directory);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn provider_sqlite_leaf_swap_prevents_observation_escape() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let database = temp.path().join("provider.sqlite");
        let admitted = temp.path().join("admitted.sqlite");
        let attacker = temp.path().join("attacker.sqlite");
        let writer = Connection::open(&database).unwrap();
        writer
            .execute("CREATE TABLE messages (body TEXT NOT NULL)", [])
            .unwrap();
        writer
            .execute("INSERT INTO messages (body) VALUES ('expected')", [])
            .unwrap();
        drop(writer);
        let writer = Connection::open(&attacker).unwrap();
        writer
            .execute("CREATE TABLE messages (body TEXT NOT NULL)", [])
            .unwrap();
        writer
            .execute("INSERT INTO messages (body) VALUES ('attacker')", [])
            .unwrap();
        drop(writer);

        let result = read_provider_body_with_finish(&database, || {
            fs::rename(&database, &admitted).unwrap();
            fs::rename(&attacker, &database).unwrap();
        });

        assert!(
            result.is_err(),
            "the value read before final source revalidation must not escape"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn provider_sqlite_parent_swap_prevents_observation_escape() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let live = temp.path().join("live");
        let admitted = temp.path().join("admitted");
        let replacement = temp.path().join("replacement");
        fs::create_dir(&live).unwrap();
        fs::create_dir(&replacement).unwrap();
        let database = live.join("provider.sqlite");
        let attacker = replacement.join("provider.sqlite");
        let writer = Connection::open(&database).unwrap();
        writer
            .execute("CREATE TABLE messages (body TEXT NOT NULL)", [])
            .unwrap();
        writer
            .execute("INSERT INTO messages (body) VALUES ('expected')", [])
            .unwrap();
        drop(writer);
        let writer = Connection::open(&attacker).unwrap();
        writer
            .execute("CREATE TABLE messages (body TEXT NOT NULL)", [])
            .unwrap();
        writer
            .execute("INSERT INTO messages (body) VALUES ('attacker')", [])
            .unwrap();
        drop(writer);

        let result = read_provider_body_with_finish(&database, || {
            fs::rename(&live, &admitted).unwrap();
            fs::rename(&replacement, &live).unwrap();
        });

        assert!(
            result.is_err(),
            "the retained parent route must be revalidated before returning the value"
        );
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
