use std::{
    ffi::OsStr,
    fs::File,
    io::{Read, Seek, SeekFrom},
    ops::Deref,
    path::Path,
};

use rusqlite::{limits::Limit, Connection};
use sha2::{Digest, Sha256};

use ctx_history_source_io::{
    observe_ordinary_file, open_ordinary_file_without_following, OrdinaryFileObservation,
    ProviderSourceRoot,
};

use crate::{
    open_root_handle_sqlite_source_snapshot, retain_sqlite_source_directory_authority,
    SqliteSourceAccessError, SqliteSourceEvidence, SqliteSourceReadSnapshot,
};

/// Preserves both a provider operation failure and a failure encountered while
/// terminally revalidating and closing its SQLite read guard.
#[derive(Debug)]
pub enum SqliteReadFinalizationError<Primary, Finalization> {
    Primary(Primary),
    Finalization(Finalization),
    PrimaryAndFinalization {
        primary: Primary,
        finalization: Finalization,
    },
}

impl<Primary, Finalization> SqliteReadFinalizationError<Primary, Finalization> {
    pub fn map_error<E>(
        self,
        map_primary: impl FnOnce(Primary) -> E,
        map_finalization: impl FnOnce(Finalization) -> E,
        combine: impl FnOnce(E, E) -> E,
    ) -> E {
        match self {
            Self::Primary(primary) => map_primary(primary),
            Self::Finalization(finalization) => map_finalization(finalization),
            Self::PrimaryAndFinalization {
                primary,
                finalization,
            } => combine(map_primary(primary), map_finalization(finalization)),
        }
    }
}

impl<Primary, Finalization> std::fmt::Display for SqliteReadFinalizationError<Primary, Finalization>
where
    Primary: std::fmt::Display,
    Finalization: std::fmt::Display,
{
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Primary(primary) => primary.fmt(formatter),
            Self::Finalization(finalization) => finalization.fmt(formatter),
            Self::PrimaryAndFinalization {
                primary,
                finalization,
            } => write!(
                formatter,
                "{primary}; terminal SQLite revalidation/cleanup also failed: {finalization}"
            ),
        }
    }
}

pub(crate) fn combine_sqlite_read_finalization<T, Primary, Finalization>(
    primary: std::result::Result<T, Primary>,
    finalization: std::result::Result<(), Finalization>,
) -> std::result::Result<T, SqliteReadFinalizationError<Primary, Finalization>> {
    match (primary, finalization) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(primary), Ok(())) => Err(SqliteReadFinalizationError::Primary(primary)),
        (Ok(_), Err(finalization)) => Err(SqliteReadFinalizationError::Finalization(finalization)),
        (Err(primary), Err(finalization)) => {
            Err(SqliteReadFinalizationError::PrimaryAndFinalization {
                primary,
                finalization,
            })
        }
    }
}
use crate::{Result, SqliteIoError, MAX_PROVIDER_SQLITE_VALUE_BYTES};

const SQLITE_COMPONENT_TOKEN_DOMAIN: &[u8] = b"ctx-provider-sqlite-component-v1\0";
const SQLITE_HEADER_BYTES: usize = 100;
const SQLITE_WAL_HEADER_BYTES: usize = 32;
const SQLITE_WAL_FRAME_HEADER_BYTES: usize = 24;

pub fn sqlite_component_change_token(
    path: &Path,
    observation: &OrdinaryFileObservation,
) -> Result<[u8; 32]> {
    let mut file = open_ordinary_file_without_following(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() || metadata.len() != observation.len() {
        return Err(SqliteIoError::SourceChangedDuringCapture);
    }

    let prefix_len = usize::try_from(observation.len().min(SQLITE_HEADER_BYTES as u64))
        .map_err(|_| SqliteIoError::SourceChangedDuringCapture)?;
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
        return Err(SqliteIoError::SourceChangedDuringCapture);
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
        SqliteIoError::InvalidPayload("invalid SQLite WAL page-size header".to_owned())
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

#[cfg(any(test, feature = "test-support"))]
fn hex_token(token: &[u8; 32]) -> String {
    token.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[derive(Clone, PartialEq, Eq)]
#[cfg(any(test, feature = "test-support"))]
pub struct ProviderSqliteSourceSnapshot {
    data_root: std::path::PathBuf,
    evidence: SqliteSourceEvidence,
    source_invalid_reason: &'static str,
    sidecar_invalid_reason: &'static str,
}

#[cfg(any(test, feature = "test-support"))]
impl ProviderSqliteSourceSnapshot {
    pub fn read(
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

    #[cfg(any(test, feature = "test-support"))]
    pub fn revision_component(&self) -> String {
        format!(
            "identity={};length={};revision={}",
            hex_token(self.evidence.identity()),
            self.evidence.length(),
            hex_token(self.evidence.revision()),
        )
    }

    pub fn revalidate(&self, path: &Path) -> Result<bool> {
        match Self::read(
            &self.data_root,
            path,
            self.source_invalid_reason,
            self.sidecar_invalid_reason,
        ) {
            Ok(current) => Ok(current == *self),
            Err(SqliteIoError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(false)
            }
            Err(SqliteIoError::InvalidProviderTranscriptPath { .. }) => Ok(false),
            Err(error) => Err(error),
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
fn read_sqlite_source_evidence(data_root: &Path, path: &Path) -> Result<SqliteSourceEvidence> {
    RootAuthorizedProviderSqliteSnapshot::open(data_root, path)?.finish()
}

struct RootAuthorizedProviderSqliteSnapshot {
    snapshot: Option<SqliteSourceReadSnapshot>,
}

impl RootAuthorizedProviderSqliteSnapshot {
    fn open(data_root: &Path, path: &Path) -> Result<Self> {
        let (parent_path, database_name) = sqlite_parent_and_leaf(path)?;
        let admission_root = ProviderSourceRoot::open(parent_path)?;
        let parent = admission_root.directory()?;
        let parent_handle = parent.try_clone_authority_handle()?;
        let sqlite_authority =
            retain_sqlite_source_directory_authority(data_root, &parent_handle, parent_path)
                .map_err(map_sqlite_source_access_error_to_io)?;
        let snapshot = open_root_handle_sqlite_source_snapshot(&sqlite_authority, database_name)
            .map_err(map_sqlite_source_access_error_to_io)?;
        if let Err(primary) = snapshot
            .revalidate()
            .map_err(map_sqlite_source_access_error_to_io)
        {
            return match snapshot
                .finish()
                .map_err(map_sqlite_source_access_error_to_io)
            {
                Ok(_) => Err(primary),
                Err(finalization) => Err(SqliteIoError::SqliteFinalization {
                    primary: Box::new(primary),
                    finalization: Box::new(finalization),
                }),
            };
        }
        Ok(Self {
            snapshot: Some(snapshot),
        })
    }

    fn connection(&self) -> Result<&Connection> {
        self.snapshot
            .as_ref()
            .ok_or(SqliteIoError::SystemInvariant(
                "provider SQLite source snapshot is inactive",
            ))?
            .connection()
            .map_err(map_sqlite_source_access_error_to_io)
    }

    fn finish(mut self) -> Result<SqliteSourceEvidence> {
        let snapshot = self.snapshot.take().ok_or(SqliteIoError::SystemInvariant(
            "provider SQLite source snapshot is inactive",
        ))?;
        snapshot
            .finish()
            .map_err(map_sqlite_source_access_error_to_io)
    }

    #[cfg(any(test, feature = "test-support"))]
    fn counter_observer(&self) -> crate::SqliteSourceSnapshotCounterObserver {
        self.snapshot
            .as_ref()
            .expect("active root-authorized SQLite snapshot")
            .counter_observer()
    }
}

fn sqlite_parent_and_leaf(path: &Path) -> Result<(&Path, &OsStr)> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| SqliteIoError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "provider SQLite path has no absolute parent directory",
        })?;
    let database_name =
        path.file_name()
            .ok_or_else(|| SqliteIoError::InvalidProviderTranscriptPath {
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
pub struct ReadOnlySqliteConnection {
    snapshot: Option<RootAuthorizedProviderSqliteSnapshot>,
}

pub struct MappedReadOnlySqliteConnection<E>(
    ReadOnlySqliteConnection,
    std::marker::PhantomData<fn() -> E>,
);

impl<E> std::ops::Deref for MappedReadOnlySqliteConnection<E> {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<E> MappedReadOnlySqliteConnection<E>
where
    E: From<SqliteIoError>,
{
    pub fn open(data_root: &Path, path: &Path) -> std::result::Result<Self, E> {
        open_provider_sqlite_readonly(data_root, path)
            .map(|connection| Self(connection, std::marker::PhantomData))
            .map_err(Into::into)
    }

    pub fn finish(self) -> std::result::Result<SqliteSourceEvidence, E> {
        self.0.finish().map_err(Into::into)
    }

    pub fn finish_with<T>(
        self,
        primary: std::result::Result<T, E>,
        combine: impl FnOnce(E, E) -> E,
    ) -> std::result::Result<T, E> {
        self.0
            .finish_with(primary)
            .map_err(|error| error.map_error(std::convert::identity, Into::into, combine))
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn counter_observer(&self) -> crate::SqliteSourceSnapshotCounterObserver {
        self.0.counter_observer()
    }
}

impl ReadOnlySqliteConnection {
    fn connection(&self) -> Result<&Connection> {
        self.snapshot
            .as_ref()
            .ok_or(SqliteIoError::SystemInvariant(
                "provider SQLite source snapshot is inactive",
            ))?
            .connection()
    }

    pub fn finish(mut self) -> Result<SqliteSourceEvidence> {
        self.snapshot
            .take()
            .ok_or(SqliteIoError::SystemInvariant(
                "provider SQLite source snapshot is inactive",
            ))?
            .finish()
    }

    /// Runs a provider read and then always performs terminal source-family
    /// revalidation and cleanup before returning its result.
    pub fn with_connection<T, E>(
        self,
        operation: impl FnOnce(&Connection) -> std::result::Result<T, E>,
    ) -> std::result::Result<T, SqliteReadFinalizationError<E, SqliteIoError>> {
        let primary = operation(&self);
        self.finish_with(primary)
    }

    /// Completes a previously-run provider operation while preserving both
    /// its primary error and any terminal revalidation/cleanup error.
    pub fn finish_with<T, E>(
        self,
        primary: std::result::Result<T, E>,
    ) -> std::result::Result<T, SqliteReadFinalizationError<E, SqliteIoError>> {
        combine_sqlite_read_finalization(primary, self.finish().map(|_| ()))
    }

    fn retain_after_configuration<E>(
        self,
        configured: std::result::Result<(), E>,
    ) -> std::result::Result<Self, SqliteReadFinalizationError<E, SqliteIoError>> {
        match configured {
            Ok(()) => Ok(self),
            Err(primary) => match self.finish() {
                Ok(_) => Err(SqliteReadFinalizationError::Primary(primary)),
                Err(finalization) => Err(SqliteReadFinalizationError::PrimaryAndFinalization {
                    primary,
                    finalization,
                }),
            },
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn counter_observer(&self) -> crate::SqliteSourceSnapshotCounterObserver {
        self.snapshot
            .as_ref()
            .expect("active read-only SQLite guard")
            .counter_observer()
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

pub fn open_provider_sqlite_readonly(
    data_root: &Path,
    path: &Path,
) -> Result<ReadOnlySqliteConnection> {
    let conn = open_sqlite_readonly_source(data_root, path)?;
    let configured = (|| {
        let value_limit = i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).map_err(|_| {
            SqliteIoError::InvalidPayload(format!(
                "provider SQLite value byte limit is unrepresentable: {MAX_PROVIDER_SQLITE_VALUE_BYTES}"
            ))
        })?;
        let connection = conn.connection()?;
        connection.set_limit(Limit::SQLITE_LIMIT_LENGTH, value_limit)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.pragma_update(None, "query_only", true)?;
        Ok(())
    })();
    conn.retain_after_configuration(configured)
        .map_err(source_io_finalization_error)
}

pub fn open_sqlite_readonly_source(
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

fn source_io_finalization_error(
    error: SqliteReadFinalizationError<SqliteIoError, SqliteIoError>,
) -> SqliteIoError {
    match error {
        SqliteReadFinalizationError::Primary(primary) => primary,
        SqliteReadFinalizationError::Finalization(finalization) => finalization,
        SqliteReadFinalizationError::PrimaryAndFinalization {
            primary,
            finalization,
        } => SqliteIoError::SqliteFinalization {
            primary: Box::new(primary),
            finalization: Box::new(finalization),
        },
    }
}

fn map_sqlite_source_access_error_to_io(error: SqliteSourceAccessError) -> SqliteIoError {
    match error {
        SqliteSourceAccessError::Io { source, .. } => SqliteIoError::Io(source),
        SqliteSourceAccessError::Sqlite { source, .. } => SqliteIoError::Sqlite(source),
        SqliteSourceAccessError::UnsafeFile { path, reason } => {
            SqliteIoError::InvalidProviderTranscriptPath { path, reason }
        }
        SqliteSourceAccessError::ConnectionIdentityMismatch
        | SqliteSourceAccessError::SourceChanged => SqliteIoError::SourceChangedDuringCapture,
        SqliteSourceAccessError::SnapshotNotActive => {
            SqliteIoError::SystemInvariant("provider SQLite source snapshot is inactive")
        }
        other => SqliteIoError::SystemIo {
            operation: "opening a root-authorized provider SQLite snapshot",
            source: std::io::Error::other(other),
        },
    }
}

#[cfg(any(test, feature = "test-support"))]
mod tests {
    use std::fs;

    #[cfg(target_os = "linux")]
    use std::{collections::BTreeMap, ffi::OsString, path::Path};

    #[cfg(target_os = "linux")]
    use rusqlite::config::DbConfig;
    use rusqlite::Connection;

    use super::{open_provider_sqlite_readonly, SqliteReadFinalizationError};
    #[cfg(target_os = "linux")]
    use super::{ProviderSqliteSourceSnapshot, SqliteSourceEvidence};
    #[cfg(target_os = "linux")]
    use crate::Result;

    #[cfg(target_os = "linux")]
    fn directory_file_bytes(path: &Path) -> BTreeMap<OsString, Vec<u8>> {
        fs::read_dir(path)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (entry.file_name(), fs::read(entry.path()).unwrap())
            })
            .collect()
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

    #[test]
    fn readonly_scope_finalizes_success_and_query_error_without_unfinished_drop() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let database = temp.path().join("provider.sqlite");
        let writer = Connection::open(&database).unwrap();
        writer
            .execute("CREATE TABLE messages (body TEXT NOT NULL)", [])
            .unwrap();
        writer
            .execute("INSERT INTO messages VALUES ('scoped')", [])
            .unwrap();
        drop(writer);

        let connection =
            open_provider_sqlite_readonly(crate::test_provider_sqlite_data_root(), &database)
                .unwrap();
        let observer = connection.counter_observer();
        let body: String = connection
            .with_connection(|connection| {
                connection.query_row("SELECT body FROM messages", [], |row| row.get(0))
            })
            .unwrap();
        assert_eq!(body, "scoped");
        let counters = observer.snapshot();
        assert_eq!(counters.unfinished_drops(), 0);
        assert_eq!(counters.active_snapshots(), 0);

        let connection =
            open_provider_sqlite_readonly(crate::test_provider_sqlite_data_root(), &database)
                .unwrap();
        let observer = connection.counter_observer();
        let error = connection
            .with_connection(|connection| {
                connection.query_row::<i64, _, _>("SELECT value FROM missing", [], |row| row.get(0))
            })
            .unwrap_err();
        assert!(matches!(error, SqliteReadFinalizationError::Primary(_)));
        let counters = observer.snapshot();
        assert_eq!(counters.unfinished_drops(), 0);
        assert_eq!(counters.active_snapshots(), 0);
    }

    #[test]
    fn readonly_scope_preserves_query_revalidation_and_cleanup_failures() {
        let temp = crate::test_support_paths::tempdir().unwrap();
        let database = temp.path().join("provider.sqlite");
        let admitted = temp.path().join("admitted.sqlite");
        let writer = Connection::open(&database).unwrap();
        writer
            .execute("CREATE TABLE messages (body TEXT NOT NULL)", [])
            .unwrap();
        drop(writer);
        crate::fail_next_opened_snapshot_cleanup_for_test();

        let connection =
            open_provider_sqlite_readonly(crate::test_provider_sqlite_data_root(), &database)
                .unwrap();
        let observer = connection.counter_observer();
        let error = connection
            .with_connection(|connection| {
                let primary =
                    connection
                        .query_row::<i64, _, _>("SELECT value FROM missing", [], |row| row.get(0));
                fs::rename(&database, &admitted).unwrap();
                let replacement = Connection::open(&database).unwrap();
                replacement
                    .execute("CREATE TABLE replacement(value TEXT)", [])
                    .unwrap();
                drop(replacement);
                primary
            })
            .unwrap_err();
        let rendered = error.to_string();
        assert!(rendered.contains("no such table: missing"));
        assert!(rendered.contains("changed while its read snapshot was active"));
        assert!(rendered.contains("injected SQLite snapshot cleanup failure"));
        let counters = observer.snapshot();
        assert_eq!(counters.unfinished_drops(), 0);
        assert_eq!(counters.active_snapshots(), 0);
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
}
