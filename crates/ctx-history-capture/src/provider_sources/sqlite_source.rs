//! Stock SQLite snapshots for root-authorized provider databases.
//!
//! The ordinary provider-source layer approves and retains the database parent
//! directory. This module keeps that directory handle, rejects symlink,
//! reparse-point, and non-regular SQLite family members, and never asks SQLite
//! to create or update files in the provider directory.
//!
//! A certified sidecar-free database is opened through SQLite's immutable URI
//! mode. A database with WAL or SHM state is copied once, with bounded I/O, to a
//! private temporary directory below the ctx data root; SQLite may create its
//! own SHM only there. Rollback journals remain typed unavailable because
//! recovery could require database writes. Source DB/WAL identity and state,
//! plus bounded SHM content, are captured before acquisition and revalidated
//! after it and again before observations are published. Concurrent commits,
//! rewrites, truncation, and sidecar creation/deletion therefore fail closed.

use std::{
    ffi::{c_char, c_void, OsStr, OsString},
    fs::{self, File, Metadata, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    ops::Deref,
    path::{Component, Path, PathBuf},
    ptr,
};

use ctx_history_core::default_data_root;
use rusqlite::{config::DbConfig, ffi, Connection, OpenFlags};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use thiserror::Error;
#[cfg(target_os = "linux")]
use url::Url;

#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;

const EVIDENCE_DOMAIN: &[u8] = b"ctx-stock-sqlite-snapshot-v2\0";
const SQLITE_SNAPSHOT_MAX_COMPONENT_BYTES: u64 = 512 * 1024 * 1024;
const SQLITE_SNAPSHOT_MAX_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;
const SQLITE_COPY_BUFFER_BYTES: usize = 64 * 1024;
const SQLITE_WAL_TOKEN_BYTES: usize = 64;
const SQLITE_SHM_MAX_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SqliteSourceComponent {
    RollbackJournal,
}

impl std::fmt::Display for SqliteSourceComponent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::RollbackJournal => "rollback journal",
        })
    }
}

#[derive(Debug, Error)]
pub(crate) enum SqliteSourceAccessError {
    #[error("unsafe SQLite source file {path:?}: {reason}")]
    UnsafeFile { path: PathBuf, reason: &'static str },
    #[error("SQLite source I/O failed during {operation} for {path:?}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("SQLite source open failed during {operation}: {source}")]
    Sqlite {
        operation: &'static str,
        #[source]
        source: rusqlite::Error,
    },
    #[error("SQLite source control {operation} failed with code {code}")]
    SqliteControl { operation: &'static str, code: i32 },
    #[error("SQLite source connection is not read-only")]
    ConnectionNotReadOnly,
    #[error("SQLite source connection is not query-only")]
    ConnectionNotQueryOnly,
    #[error("SQLite source connection does not match the approved path")]
    ConnectionIdentityMismatch,
    #[error("SQLite source file changed while its read snapshot was active")]
    SourceChanged,
    #[error("SQLite source snapshot exceeds the bounded limit for {path:?}: {length} > {maximum}")]
    SnapshotTooLarge {
        path: PathBuf,
        length: u64,
        maximum: u64,
    },
    #[error("SQLite source snapshot is unavailable: {reason}")]
    SnapshotUnavailable { reason: String },
    #[error("SQLite {component} is unavailable: {capability}")]
    UnsupportedSidecarIdentity {
        component: SqliteSourceComponent,
        capability: &'static str,
    },
    #[error("SQLite source snapshot transaction is no longer active")]
    SnapshotNotActive,
}

pub(crate) type SqliteSourceAccessResult<T> = Result<T, SqliteSourceAccessError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SqliteSourceSnapshotStrategy {
    ImmutableMain,
    CopiedFamily,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SqliteSourceEvidence {
    identity: [u8; 32],
    length: u64,
    wal_length: Option<u64>,
    shared_memory_length: Option<u64>,
    schema: SqliteSchemaEvidence,
    source: SqliteConnectionEvidence,
    revision: [u8; 32],
}

impl SqliteSourceEvidence {
    pub(crate) fn identity(&self) -> &[u8; 32] {
        &self.identity
    }

    pub(crate) fn length(&self) -> u64 {
        self.length
    }

    pub(crate) fn revision(&self) -> &[u8; 32] {
        &self.revision
    }

    #[cfg(test)]
    pub(crate) fn wal_length(&self) -> Option<u64> {
        self.wal_length
    }

    #[cfg(test)]
    pub(crate) fn shared_memory_length(&self) -> Option<u64> {
        self.shared_memory_length
    }
}

/// Retained authority for one approved SQLite parent directory.
#[derive(Debug)]
pub(crate) struct SqliteSourceDirectoryAuthority {
    directory: File,
    path: PathBuf,
    identity: NativeFileIdentity,
}

impl SqliteSourceDirectoryAuthority {
    fn retain(authorized_parent: &File, approved_path: &Path) -> SqliteSourceAccessResult<Self> {
        validate_approved_parent_path(approved_path)?;
        let directory =
            authorized_parent
                .try_clone()
                .map_err(|source| SqliteSourceAccessError::Io {
                    operation: "retaining the approved SQLite parent directory",
                    path: approved_path.to_path_buf(),
                    source,
                })?;
        let retained =
            NativeFileState::read(&directory, approved_path, ExpectedObjectKind::Directory)?;
        let named = open_nofollow(approved_path, ExpectedObjectKind::Directory)?;
        let named_state =
            NativeFileState::read(&named, approved_path, ExpectedObjectKind::Directory)?;
        if retained.identity != named_state.identity {
            return Err(SqliteSourceAccessError::ConnectionIdentityMismatch);
        }
        Ok(Self {
            directory,
            path: approved_path.to_path_buf(),
            identity: retained.identity,
        })
    }

    fn revalidate(&self) -> SqliteSourceAccessResult<()> {
        let retained =
            NativeFileState::read(&self.directory, &self.path, ExpectedObjectKind::Directory)
                .map_err(map_revalidation_error)?;
        if retained.identity != self.identity {
            return Err(SqliteSourceAccessError::SourceChanged);
        }
        let named = open_nofollow(&self.path, ExpectedObjectKind::Directory)
            .map_err(map_revalidation_error)?;
        let named_state = NativeFileState::read(&named, &self.path, ExpectedObjectKind::Directory)
            .map_err(map_revalidation_error)?;
        if named_state.identity == self.identity {
            Ok(())
        } else {
            Err(SqliteSourceAccessError::SourceChanged)
        }
    }
}

/// A stock read-only SQLite connection with a pinned read transaction.
#[must_use = "call finish() after provider queries and before publishing observations"]
#[derive(Debug)]
pub(crate) struct SqliteSourceReadSnapshot {
    connection: Option<Connection>,
    family: SqliteSourceFamily,
    native_evidence: SqliteFamilyEvidence,
    sqlite_evidence: SqliteSnapshotEvidence,
    evidence: SqliteSourceEvidence,
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "retained for bounded snapshot observability")
    )]
    strategy: SqliteSourceSnapshotStrategy,
    #[cfg_attr(
        not(test),
        allow(dead_code, reason = "retained for bounded snapshot observability")
    )]
    copied_bytes: u64,
    _snapshot_directory: Option<TempDir>,
}

impl SqliteSourceReadSnapshot {
    pub(crate) fn connection(&self) -> SqliteSourceAccessResult<&Connection> {
        let connection = self
            .connection
            .as_ref()
            .ok_or(SqliteSourceAccessError::SnapshotNotActive)?;
        verify_snapshot_active(connection)?;
        Ok(connection)
    }

    pub(crate) fn evidence(&self) -> &SqliteSourceEvidence {
        &self.evidence
    }

    #[cfg(test)]
    pub(crate) fn strategy(&self) -> SqliteSourceSnapshotStrategy {
        self.strategy
    }

    #[cfg(test)]
    pub(crate) fn copied_bytes(&self) -> u64 {
        self.copied_bytes
    }

    #[cfg(test)]
    pub(crate) fn snapshot_directory(&self) -> Option<&Path> {
        self._snapshot_directory
            .as_ref()
            .map(tempfile::TempDir::path)
    }

    /// Revalidates the pinned SQLite view and retained DB family without
    /// ending the read transaction.
    pub(crate) fn revalidate(&self) -> SqliteSourceAccessResult<()> {
        let connection = self.connection()?;
        let current_sqlite_evidence = capture_sqlite_evidence(connection)?;
        if current_sqlite_evidence != self.sqlite_evidence {
            return Err(SqliteSourceAccessError::SourceChanged);
        }
        self.family.revalidate(&self.native_evidence)
    }

    /// Revalidates the source while the read transaction is pinned, ends the
    /// transaction, closes SQLite, then checks the approved names once more.
    pub(crate) fn finish(mut self) -> SqliteSourceAccessResult<SqliteSourceEvidence> {
        self.revalidate()?;
        let connection = self
            .connection
            .as_ref()
            .ok_or(SqliteSourceAccessError::SnapshotNotActive)?;
        clear_snapshot_authorizer(connection)?;
        connection
            .execute_batch("ROLLBACK")
            .map_err(|source| sqlite_error("ending the provider read snapshot", source))?;
        self.connection.take();
        self.family.revalidate(&self.native_evidence)?;
        Ok(self.evidence.clone())
    }
}

impl Drop for SqliteSourceReadSnapshot {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.as_ref() {
            let _ = clear_snapshot_authorizer(connection);
            let _ = connection.execute_batch("ROLLBACK");
        }
        self.connection.take();
    }
}

mod family;
mod logical;
mod snapshot;

use family::{
    capture_sqlite_evidence, clear_snapshot_authorizer, configure_and_pin_snapshot,
    map_revalidation_error, open_nofollow, sqlite_error, validate_approved_parent_path,
    verify_connection_read_only, verify_snapshot_active, ExpectedObjectKind, NativeFileIdentity,
    NativeFileState, SqliteConnectionEvidence, SqliteFamilyEvidence, SqliteFamilyMember,
    SqliteSchemaEvidence, SqliteSnapshotEvidence, SqliteSourceFamily,
};
pub(crate) use logical::SqliteLogicalSnapshot;
#[cfg(test)]
use snapshot::open_root_handle_sqlite_source_snapshot_for_test;
pub(crate) use snapshot::{
    open_ctx_owned_sqlite_read_snapshot, open_root_handle_sqlite_source_snapshot,
    retain_sqlite_source_directory_authority, CtxOwnedSqliteReadSnapshot,
};

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        ffi::{OsStr, OsString},
        fs::{self, File, OpenOptions},
        io::{Seek, SeekFrom, Write},
        path::Path,
        time::{Duration, Instant},
    };

    #[cfg(unix)]
    use std::os::unix::fs::{symlink, PermissionsExt};

    use rusqlite::{config::DbConfig, params, Connection};

    use super::{
        default_data_root, open_root_handle_sqlite_source_snapshot,
        open_root_handle_sqlite_source_snapshot_for_test, retain_sqlite_source_directory_authority,
        SqliteSourceAccessError, SqliteSourceComponent, SqliteSourceDirectoryAuthority,
        SqliteSourceReadSnapshot, SqliteSourceSnapshotStrategy, SQLITE_SHM_MAX_BYTES,
        SQLITE_SNAPSHOT_MAX_COMPONENT_BYTES,
    };

    fn create_database(path: &Path, value: &str) {
        let connection = Connection::open(path).unwrap();
        connection
            .execute("CREATE TABLE messages (body TEXT NOT NULL)", [])
            .unwrap();
        connection
            .execute("INSERT INTO messages (body) VALUES (?1)", params![value])
            .unwrap();
    }

    fn create_persistent_wal(path: &Path) -> Connection {
        let connection = Connection::open(path).unwrap();
        let mode: String = connection
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        connection
            .execute("CREATE TABLE messages (body TEXT NOT NULL)", [])
            .unwrap();
        connection
            .execute("INSERT INTO messages (body) VALUES ('from-wal')", [])
            .unwrap();
        connection
            .set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
            .unwrap();
        connection
    }

    fn retain_parent(path: &Path) -> SqliteSourceDirectoryAuthority {
        let parent = File::open(path).unwrap();
        retain_sqlite_source_directory_authority(&parent, path).unwrap()
    }

    fn read_values(snapshot: &SqliteSourceReadSnapshot) -> Vec<String> {
        snapshot
            .connection()
            .unwrap()
            .prepare("SELECT body FROM messages ORDER BY rowid")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    }

    fn directory_file_bytes(path: &Path) -> BTreeMap<OsString, Vec<u8>> {
        fs::read_dir(path)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (entry.file_name(), fs::read(entry.path()).unwrap())
            })
            .collect()
    }

    #[test]
    fn stock_sqlite_initial_snapshot_succeeds_with_idle_wal_writer() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("provider.sqlite");
        create_database(&database, "before-wal");
        let writer = Connection::open(&database).unwrap();
        let mode: String = writer
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        writer.execute_batch("PRAGMA wal_autocheckpoint=0").unwrap();
        let wal = database.with_file_name("provider.sqlite-wal");
        assert!(
            !wal.exists(),
            "the idle writer must not have materialized a WAL pathname"
        );
        let before = directory_file_bytes(temp.path());
        let parent = retain_parent(temp.path());

        let snapshot =
            open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite"))
                .unwrap();
        assert_eq!(read_values(&snapshot), ["before-wal"]);
        assert_eq!(
            snapshot.strategy(),
            SqliteSourceSnapshotStrategy::ImmutableMain
        );
        assert_eq!(snapshot.copied_bytes(), 0);
        assert_eq!(snapshot.evidence().wal_length(), None);
        snapshot.finish().unwrap();
        assert!(!wal.exists());
        assert!(!database.with_file_name("provider.sqlite-shm").exists());
        assert_eq!(directory_file_bytes(temp.path()), before);

        drop(writer);
    }

    #[test]
    fn stock_sqlite_reads_active_wal_read_only_and_query_only() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("provider.sqlite");
        let writer = create_persistent_wal(&database);
        let wal = database.with_file_name("provider.sqlite-wal");
        let shared_memory = database.with_file_name("provider.sqlite-shm");
        let before_database = fs::read(&database).unwrap();
        let before_wal = fs::read(&wal).unwrap();
        let before_shared_memory = fs::read(&shared_memory).unwrap();
        let before_directory = directory_file_bytes(temp.path());
        let parent = retain_parent(temp.path());

        let snapshot =
            open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite"))
                .unwrap();
        assert_eq!(read_values(&snapshot), ["from-wal"]);
        assert_eq!(
            snapshot.strategy(),
            SqliteSourceSnapshotStrategy::CopiedFamily
        );
        assert_eq!(
            snapshot.copied_bytes(),
            u64::try_from(before_database.len() + before_wal.len()).unwrap()
        );
        assert!(snapshot
            .snapshot_directory()
            .unwrap()
            .starts_with(default_data_root().unwrap()));
        let snapshot_directory = snapshot.snapshot_directory().unwrap().to_path_buf();
        assert_eq!(
            snapshot
                .connection()
                .unwrap()
                .pragma_query_value(None, "query_only", |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert!(snapshot
            .connection()
            .unwrap()
            .execute("INSERT INTO messages (body) VALUES ('forbidden')", [])
            .is_err());
        assert!(
            snapshot
                .connection()
                .unwrap()
                .execute_batch("COMMIT")
                .is_err(),
            "provider consumers may not end the guard-owned transaction"
        );
        assert!(snapshot.evidence().wal_length().is_some());
        assert!(snapshot.evidence().shared_memory_length().is_some());
        snapshot.finish().unwrap();

        assert!(!snapshot_directory.exists());
        assert_eq!(fs::read(&database).unwrap(), before_database);
        assert_eq!(fs::read(&wal).unwrap(), before_wal);
        assert_eq!(fs::read(&shared_memory).unwrap(), before_shared_memory);
        assert_eq!(directory_file_bytes(temp.path()), before_directory);
        assert_eq!(
            writer
                .query_row("SELECT count(*) FROM messages", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn copied_wal_snapshot_keeps_missing_provider_shm_missing() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("provider.sqlite");
        let writer = create_persistent_wal(&database);
        drop(writer);
        let wal = database.with_file_name("provider.sqlite-wal");
        let shared_memory = database.with_file_name("provider.sqlite-shm");
        fs::remove_file(&shared_memory).unwrap();
        let before_database = fs::read(&database).unwrap();
        let before_wal = fs::read(&wal).unwrap();
        let parent = retain_parent(temp.path());

        let snapshot =
            open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite"))
                .unwrap();
        assert_eq!(read_values(&snapshot), ["from-wal"]);
        assert_eq!(
            snapshot.strategy(),
            SqliteSourceSnapshotStrategy::CopiedFamily
        );
        assert_eq!(
            snapshot.copied_bytes(),
            u64::try_from(before_database.len() + before_wal.len()).unwrap()
        );
        snapshot.finish().unwrap();

        assert!(!shared_memory.exists());
        assert_eq!(fs::read(&database).unwrap(), before_database);
        assert_eq!(fs::read(&wal).unwrap(), before_wal);
    }

    #[cfg(unix)]
    #[test]
    fn active_wal_snapshot_reads_a_read_only_provider_tree() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("provider.sqlite");
        let writer = create_persistent_wal(&database);
        drop(writer);
        let wal = database.with_file_name("provider.sqlite-wal");
        let shared_memory = database.with_file_name("provider.sqlite-shm");
        for path in [&database, &wal, &shared_memory] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o444)).unwrap();
        }
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o555)).unwrap();
        let before = directory_file_bytes(temp.path());
        let parent = retain_parent(temp.path());

        let snapshot =
            open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite"))
                .unwrap();
        assert_eq!(read_values(&snapshot), ["from-wal"]);
        assert_eq!(
            snapshot.strategy(),
            SqliteSourceSnapshotStrategy::CopiedFamily
        );
        snapshot.finish().unwrap();
        assert_eq!(directory_file_bytes(temp.path()), before);

        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o755)).unwrap();
        for path in [&database, &wal, &shared_memory] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
        }
    }

    #[test]
    fn sidecar_creation_during_immutable_open_is_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("provider.sqlite");
        let wal = database.with_file_name("provider.sqlite-wal");
        create_database(&database, "expected");
        let parent = retain_parent(temp.path());

        let result = open_root_handle_sqlite_source_snapshot_for_test(
            &parent,
            OsStr::new("provider.sqlite"),
            || fs::write(&wal, b"appeared during acquisition").unwrap(),
        );

        assert!(matches!(
            result,
            Err(SqliteSourceAccessError::SourceChanged)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn wal_deletion_during_copied_acquisition_is_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("provider.sqlite");
        let _writer = create_persistent_wal(&database);
        let wal = database.with_file_name("provider.sqlite-wal");
        let parent = retain_parent(temp.path());

        let result = open_root_handle_sqlite_source_snapshot_for_test(
            &parent,
            OsStr::new("provider.sqlite"),
            || fs::remove_file(&wal).unwrap(),
        );

        assert!(matches!(
            result,
            Err(SqliteSourceAccessError::SourceChanged)
        ));
    }

    #[test]
    fn bounded_active_wal_copy_has_one_source_byte_pass() {
        const PAYLOAD_BYTES: usize = 8 * 1024 * 1024;

        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("provider.sqlite");
        let writer = Connection::open(&database).unwrap();
        let mode: String = writer
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        writer.execute_batch("PRAGMA wal_autocheckpoint=0").unwrap();
        writer
            .execute("CREATE TABLE payloads (body BLOB NOT NULL)", [])
            .unwrap();
        writer
            .execute(
                "INSERT INTO payloads (body) VALUES (zeroblob(?1))",
                [PAYLOAD_BYTES],
            )
            .unwrap();
        writer
            .set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
            .unwrap();
        let wal = database.with_file_name("provider.sqlite-wal");
        let expected_copied =
            fs::metadata(&database).unwrap().len() + fs::metadata(&wal).unwrap().len();
        let parent = retain_parent(temp.path());

        let started = Instant::now();
        let snapshot =
            open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite"))
                .unwrap();
        let length: i64 = snapshot
            .connection()
            .unwrap()
            .query_row("SELECT length(body) FROM payloads", [], |row| row.get(0))
            .unwrap();
        assert_eq!(length, PAYLOAD_BYTES as i64);
        assert_eq!(snapshot.copied_bytes(), expected_copied);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "an 8 MiB active-WAL snapshot exceeded the focused sanity bound"
        );
        snapshot.finish().unwrap();
    }

    #[test]
    fn oversized_snapshot_component_is_typed_unavailable_before_copy() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("provider.sqlite");
        let file = File::create(&database).unwrap();
        file.set_len(SQLITE_SNAPSHOT_MAX_COMPONENT_BYTES + 1)
            .unwrap();
        fs::write(
            database.with_file_name("provider.sqlite-shm"),
            b"force bounded copied-family acquisition",
        )
        .unwrap();
        let parent = retain_parent(temp.path());

        let result =
            open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite"));
        assert!(matches!(
            result,
            Err(SqliteSourceAccessError::SnapshotTooLarge { .. })
        ));
    }

    #[test]
    fn shared_memory_rewrite_during_copied_acquisition_is_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("provider.sqlite");
        let shared_memory = database.with_file_name("provider.sqlite-shm");
        create_database(&database, "expected");
        fs::write(&shared_memory, vec![0_u8; 32 * 1024]).unwrap();
        let parent = retain_parent(temp.path());

        let result = open_root_handle_sqlite_source_snapshot_for_test(
            &parent,
            OsStr::new("provider.sqlite"),
            || {
                let mut file = OpenOptions::new().write(true).open(&shared_memory).unwrap();
                file.seek(SeekFrom::Start(16 * 1024)).unwrap();
                file.write_all(b"changed-shm").unwrap();
                file.sync_all().unwrap();
            },
        );

        assert!(matches!(
            result,
            Err(SqliteSourceAccessError::SourceChanged)
        ));
    }

    #[test]
    fn oversized_shared_memory_is_typed_unavailable_before_hashing() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("provider.sqlite");
        let shared_memory = database.with_file_name("provider.sqlite-shm");
        create_database(&database, "expected");
        let file = File::create(&shared_memory).unwrap();
        file.set_len(SQLITE_SHM_MAX_BYTES + 1).unwrap();
        let parent = retain_parent(temp.path());

        let result =
            open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite"));

        assert!(matches!(
            result,
            Err(SqliteSourceAccessError::SnapshotTooLarge { .. })
        ));
    }

    #[test]
    fn stock_sqlite_keeps_a_pinned_view_and_fails_changed_writer_generation() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("provider.sqlite");
        let writer = create_persistent_wal(&database);
        let parent = retain_parent(temp.path());
        let snapshot =
            open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite"))
                .unwrap();
        assert_eq!(read_values(&snapshot), ["from-wal"]);

        writer
            .execute("INSERT INTO messages (body) VALUES ('later')", [])
            .unwrap();
        assert_eq!(read_values(&snapshot), ["from-wal"]);
        assert!(matches!(
            snapshot.finish(),
            Err(SqliteSourceAccessError::SourceChanged)
        ));

        let replacement =
            open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite"))
                .unwrap();
        assert_eq!(read_values(&replacement), ["from-wal", "later"]);
        replacement.finish().unwrap();
    }

    #[test]
    fn committed_wal_write_during_stock_open_is_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("provider.sqlite");
        let writer = create_persistent_wal(&database);
        let parent = retain_parent(temp.path());

        let result = open_root_handle_sqlite_source_snapshot_for_test(
            &parent,
            OsStr::new("provider.sqlite"),
            || {
                writer
                    .execute("INSERT INTO messages (body) VALUES ('during-open')", [])
                    .unwrap();
            },
        );

        assert!(matches!(
            result,
            Err(SqliteSourceAccessError::SourceChanged)
        ));
    }

    #[test]
    fn direct_wal_truncate_is_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("provider.sqlite");
        let _writer = create_persistent_wal(&database);
        let wal = database.with_file_name("provider.sqlite-wal");
        let parent = retain_parent(temp.path());
        let snapshot =
            open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite"))
                .unwrap();
        assert_eq!(read_values(&snapshot), ["from-wal"]);

        let file = OpenOptions::new().write(true).open(&wal).unwrap();
        file.set_len(0).unwrap();
        file.sync_all().unwrap();

        assert!(snapshot.finish().is_err());
    }

    #[test]
    fn source_revision_changes_after_a_committed_wal_generation() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("provider.sqlite");
        let writer = create_persistent_wal(&database);
        let parent = retain_parent(temp.path());
        let first = open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite"))
            .unwrap();
        let first_revision = *first.evidence().revision();
        first.finish().unwrap();

        writer
            .execute("INSERT INTO messages (body) VALUES ('next')", [])
            .unwrap();
        let second =
            open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite"))
                .unwrap();
        assert_ne!(second.evidence().revision(), &first_revision);
        second.finish().unwrap();
    }

    #[test]
    fn direct_database_rewrite_is_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("provider.sqlite");
        create_database(&database, "expected");
        let parent = retain_parent(temp.path());
        let snapshot =
            open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite"))
                .unwrap();
        assert_eq!(read_values(&snapshot), ["expected"]);

        let mut file = OpenOptions::new().append(true).open(&database).unwrap();
        file.write_all(b"rewrite evidence").unwrap();
        file.sync_all().unwrap();
        assert!(matches!(
            snapshot.finish(),
            Err(SqliteSourceAccessError::SourceChanged)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn leaf_swap_between_admission_and_stock_open_is_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("provider.sqlite");
        let admitted = temp.path().join("admitted.sqlite");
        let attacker = temp.path().join("attacker.sqlite");
        create_database(&database, "expected");
        create_database(&attacker, "attacker");
        let parent = retain_parent(temp.path());

        let result = open_root_handle_sqlite_source_snapshot_for_test(
            &parent,
            OsStr::new("provider.sqlite"),
            || {
                fs::rename(&database, &admitted).unwrap();
                fs::rename(&attacker, &database).unwrap();
            },
        );
        assert!(matches!(
            result,
            Err(SqliteSourceAccessError::SourceChanged)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_database_is_rejected_before_sqlite_open() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target.sqlite");
        let link = temp.path().join("provider.sqlite");
        create_database(&target, "target");
        symlink(&target, &link).unwrap();
        let parent = retain_parent(temp.path());

        let result =
            open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite"));
        assert!(matches!(
            result,
            Err(SqliteSourceAccessError::UnsafeFile { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_sidecar_is_rejected_before_sqlite_open() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("provider.sqlite");
        let target = temp.path().join("outside-wal");
        create_database(&database, "expected");
        fs::write(&target, b"not a WAL").unwrap();
        symlink(&target, database.with_file_name("provider.sqlite-wal")).unwrap();
        let parent = retain_parent(temp.path());

        let result =
            open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite"));
        assert!(matches!(
            result,
            Err(SqliteSourceAccessError::UnsafeFile { .. })
        ));
    }

    #[test]
    fn nonregular_database_is_rejected_before_sqlite_open() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("provider.sqlite")).unwrap();
        let parent = retain_parent(temp.path());

        let result =
            open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite"));
        assert!(matches!(
            result,
            Err(SqliteSourceAccessError::UnsafeFile { .. })
        ));
    }

    #[test]
    fn nonregular_sidecar_is_rejected_before_sqlite_open() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("provider.sqlite");
        create_database(&database, "expected");
        fs::create_dir(database.with_file_name("provider.sqlite-shm")).unwrap();
        let parent = retain_parent(temp.path());

        let result =
            open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite"));
        assert!(matches!(
            result,
            Err(SqliteSourceAccessError::UnsafeFile { .. })
        ));
    }

    #[test]
    fn rollback_journal_is_typed_unavailable_without_recovery() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("provider.sqlite");
        create_database(&database, "expected");
        fs::write(
            database.with_file_name("provider.sqlite-journal"),
            b"not recovered",
        )
        .unwrap();
        let parent = retain_parent(temp.path());

        let result =
            open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite"));
        assert!(matches!(
            result,
            Err(SqliteSourceAccessError::UnsupportedSidecarIdentity {
                component: SqliteSourceComponent::RollbackJournal,
                ..
            })
        ));
    }
}
