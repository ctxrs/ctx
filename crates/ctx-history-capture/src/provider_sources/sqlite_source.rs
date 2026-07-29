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

/// A pinned read-only SQLite view over a snapshot already owned by ctx.
///
/// The caller is responsible for proving that `path` lives in private
/// ctx-owned storage. SQLite may create or update sidecars beside this path;
/// it can never reach the provider directory through this API.
#[must_use = "call finish() after complete-content queries"]
#[derive(Debug)]
pub(crate) struct CtxOwnedSqliteReadSnapshot {
    connection: Option<Connection>,
}

impl CtxOwnedSqliteReadSnapshot {
    pub(crate) fn finish(mut self) -> SqliteSourceAccessResult<()> {
        let connection = self
            .connection
            .as_ref()
            .ok_or(SqliteSourceAccessError::SnapshotNotActive)?;
        clear_snapshot_authorizer(connection)?;
        connection
            .execute_batch("ROLLBACK")
            .map_err(|source| sqlite_error("ending the ctx-owned SQLite snapshot", source))?;
        self.connection.take();
        Ok(())
    }
}

impl Deref for CtxOwnedSqliteReadSnapshot {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        match self.connection.as_ref() {
            Some(connection) => connection,
            None => std::process::abort(),
        }
    }
}

impl Drop for CtxOwnedSqliteReadSnapshot {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.as_ref() {
            let _ = clear_snapshot_authorizer(connection);
            let _ = connection.execute_batch("ROLLBACK");
        }
        self.connection.take();
    }
}

pub(crate) fn open_ctx_owned_sqlite_read_snapshot(
    path: &Path,
) -> SqliteSourceAccessResult<CtxOwnedSqliteReadSnapshot> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|source| sqlite_error("opening a ctx-owned SQLite snapshot", source))?;
    verify_connection_read_only(&connection)?;
    configure_and_pin_snapshot(&connection)?;
    Ok(CtxOwnedSqliteReadSnapshot {
        connection: Some(connection),
    })
}

/// Retains an approved parent-directory handle together with the pathname that
/// stock SQLite is allowed to open beneath it.
pub(crate) fn retain_sqlite_source_directory_authority(
    authorized_parent: &File,
    approved_parent_path: &Path,
) -> SqliteSourceAccessResult<SqliteSourceDirectoryAuthority> {
    SqliteSourceDirectoryAuthority::retain(authorized_parent, approved_parent_path)
}

/// Opens one approved SQLite leaf through stock rusqlite/SQLite behavior.
pub(crate) fn open_root_handle_sqlite_source_snapshot(
    authority: &SqliteSourceDirectoryAuthority,
    database_name: &OsStr,
) -> SqliteSourceAccessResult<SqliteSourceReadSnapshot> {
    open_root_handle_sqlite_source_snapshot_inner(authority, database_name, || {})
}

fn open_root_handle_sqlite_source_snapshot_inner(
    authority: &SqliteSourceDirectoryAuthority,
    database_name: &OsStr,
    before_source_revalidation: impl FnOnce(),
) -> SqliteSourceAccessResult<SqliteSourceReadSnapshot> {
    let family = SqliteSourceFamily::open(authority, database_name)?;
    let native_evidence = family.capture_evidence()?;
    family.revalidate(&native_evidence)?;

    let acquired = acquire_sqlite_connection(&family, &native_evidence)?;
    verify_connection_read_only(&acquired.connection)?;
    configure_and_pin_snapshot(&acquired.connection)?;
    before_source_revalidation();

    // The source family is checked only after SQLite has pinned the selected
    // view. No provider observation may escape if acquisition raced a commit,
    // rewrite, truncation, replacement, or sidecar transition.
    family.revalidate(&native_evidence)?;
    let sqlite_evidence = capture_sqlite_evidence(&acquired.connection)?;
    family.revalidate(&native_evidence)?;
    let evidence = SqliteSourceEvidence::from_snapshot(&native_evidence, &sqlite_evidence);
    Ok(SqliteSourceReadSnapshot {
        connection: Some(acquired.connection),
        family,
        native_evidence,
        sqlite_evidence,
        evidence,
        strategy: acquired.strategy,
        copied_bytes: acquired.copied_bytes,
        _snapshot_directory: acquired.snapshot_directory,
    })
}

struct AcquiredSqliteConnection {
    connection: Connection,
    strategy: SqliteSourceSnapshotStrategy,
    copied_bytes: u64,
    snapshot_directory: Option<TempDir>,
}

fn acquire_sqlite_connection(
    family: &SqliteSourceFamily,
    evidence: &SqliteFamilyEvidence,
) -> SqliteSourceAccessResult<AcquiredSqliteConnection> {
    if family.wal.is_none() && family.shared_memory.is_none() {
        #[cfg(target_os = "linux")]
        if immutable_procfd_available(&family.database.file) {
            return Ok(AcquiredSqliteConnection {
                connection: open_immutable_main(&family.database)?,
                strategy: SqliteSourceSnapshotStrategy::ImmutableMain,
                copied_bytes: 0,
                snapshot_directory: None,
            });
        }
    }

    enforce_snapshot_copy_bounds(family, evidence)?;
    let (snapshot_directory, snapshot_path, copied_bytes) =
        copy_sqlite_family_to_ctx(family, evidence)?;
    let connection = Connection::open_with_flags(
        &snapshot_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|source| sqlite_error("opening the ctx-owned provider snapshot", source))?;
    Ok(AcquiredSqliteConnection {
        connection,
        strategy: SqliteSourceSnapshotStrategy::CopiedFamily,
        copied_bytes,
        snapshot_directory: Some(snapshot_directory),
    })
}

#[cfg(target_os = "linux")]
fn immutable_procfd_available(database: &File) -> bool {
    PathBuf::from(format!("/proc/self/fd/{}", database.as_raw_fd())).exists()
}

#[cfg(target_os = "linux")]
fn open_immutable_main(database: &SqliteFamilyMember) -> SqliteSourceAccessResult<Connection> {
    let procfd_path = PathBuf::from(format!("/proc/self/fd/{}", database.file.as_raw_fd()));
    let mut uri = Url::from_file_path(&procfd_path).map_err(|()| {
        SqliteSourceAccessError::SnapshotUnavailable {
            reason: "the retained SQLite main handle cannot be represented as a file URI"
                .to_owned(),
        }
    })?;
    uri.query_pairs_mut()
        .append_pair("mode", "ro")
        .append_pair("immutable", "1");
    Connection::open_with_flags(
        uri.as_str(),
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE
            | OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|source| sqlite_error("opening the retained immutable provider database", source))
}

fn enforce_snapshot_copy_bounds(
    family: &SqliteSourceFamily,
    evidence: &SqliteFamilyEvidence,
) -> SqliteSourceAccessResult<()> {
    let mut total = bounded_snapshot_component(&family.database.path, evidence.database.length)?;
    if let (Some(wal), Some(state)) = (family.wal.as_ref(), evidence.wal.as_ref()) {
        total = total
            .checked_add(bounded_snapshot_component(&wal.path, state.length)?)
            .ok_or_else(|| SqliteSourceAccessError::SnapshotTooLarge {
                path: family.database.path.clone(),
                length: u64::MAX,
                maximum: SQLITE_SNAPSHOT_MAX_TOTAL_BYTES,
            })?;
    }
    if total > SQLITE_SNAPSHOT_MAX_TOTAL_BYTES {
        return Err(SqliteSourceAccessError::SnapshotTooLarge {
            path: family.database.path.clone(),
            length: total,
            maximum: SQLITE_SNAPSHOT_MAX_TOTAL_BYTES,
        });
    }
    Ok(())
}

fn bounded_snapshot_component(path: &Path, length: u64) -> SqliteSourceAccessResult<u64> {
    if length > SQLITE_SNAPSHOT_MAX_COMPONENT_BYTES {
        Err(SqliteSourceAccessError::SnapshotTooLarge {
            path: path.to_path_buf(),
            length,
            maximum: SQLITE_SNAPSHOT_MAX_COMPONENT_BYTES,
        })
    } else {
        Ok(length)
    }
}

fn copy_sqlite_family_to_ctx(
    family: &SqliteSourceFamily,
    evidence: &SqliteFamilyEvidence,
) -> SqliteSourceAccessResult<(TempDir, PathBuf, u64)> {
    let data_root =
        default_data_root().map_err(|source| SqliteSourceAccessError::SnapshotUnavailable {
            reason: format!("the ctx data root is unavailable: {source}"),
        })?;
    fs::create_dir_all(&data_root).map_err(|source| SqliteSourceAccessError::Io {
        operation: "creating the ctx data root for a provider SQLite snapshot",
        path: data_root.clone(),
        source,
    })?;
    let directory = tempfile::Builder::new()
        .prefix("provider-sqlite-snapshot-")
        .tempdir_in(&data_root)
        .map_err(|source| SqliteSourceAccessError::Io {
            operation: "creating a private provider SQLite snapshot",
            path: data_root,
            source,
        })?;
    let snapshot_path = directory.path().join("source.sqlite");
    copy_sqlite_member(&family.database, &snapshot_path, evidence.database.length)?;
    let mut copied_bytes = evidence.database.length;
    if let (Some(wal), Some(state)) = (family.wal.as_ref(), evidence.wal.as_ref()) {
        copy_sqlite_member(
            wal,
            &directory.path().join("source.sqlite-wal"),
            state.length,
        )?;
        copied_bytes = copied_bytes
            .checked_add(state.length)
            .ok_or(SqliteSourceAccessError::SourceChanged)?;
    }
    // SHM is lock coordination, not provider content. Copying it would retain
    // volatile reader marks. Stock SQLite rebuilds it only in this ctx-owned
    // directory from the certified DB/WAL pair.
    family.revalidate(evidence)?;
    Ok((directory, snapshot_path, copied_bytes))
}

fn copy_sqlite_member(
    member: &SqliteFamilyMember,
    destination: &Path,
    expected_length: u64,
) -> SqliteSourceAccessResult<()> {
    let mut source_file =
        member
            .file
            .try_clone()
            .map_err(|source| SqliteSourceAccessError::Io {
                operation: "retaining a provider SQLite component for snapshot copy",
                path: member.path.clone(),
                source,
            })?;
    source_file
        .seek(SeekFrom::Start(0))
        .map_err(|source| SqliteSourceAccessError::Io {
            operation: "seeking a provider SQLite component for snapshot copy",
            path: member.path.clone(),
            source,
        })?;
    let mut destination_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|source| SqliteSourceAccessError::Io {
            operation: "creating a ctx-owned SQLite snapshot component",
            path: destination.to_path_buf(),
            source,
        })?;
    let mut remaining = expected_length;
    let mut buffer = [0_u8; SQLITE_COPY_BUFFER_BYTES];
    while remaining > 0 {
        let requested = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| SqliteSourceAccessError::SourceChanged)?;
        let read = source_file
            .read(&mut buffer[..requested])
            .map_err(|source| SqliteSourceAccessError::Io {
                operation: "reading a provider SQLite snapshot component",
                path: member.path.clone(),
                source,
            })?;
        if read == 0 {
            return Err(SqliteSourceAccessError::SourceChanged);
        }
        destination_file
            .write_all(&buffer[..read])
            .map_err(|source| SqliteSourceAccessError::Io {
                operation: "writing a ctx-owned SQLite snapshot component",
                path: destination.to_path_buf(),
                source,
            })?;
        remaining -= read as u64;
    }
    let mut extra = [0_u8; 1];
    if source_file
        .read(&mut extra)
        .map_err(|source| SqliteSourceAccessError::Io {
            operation: "certifying a provider SQLite snapshot component length",
            path: member.path.clone(),
            source,
        })?
        != 0
    {
        return Err(SqliteSourceAccessError::SourceChanged);
    }
    destination_file
        .flush()
        .map_err(|source| SqliteSourceAccessError::Io {
            operation: "flushing a ctx-owned SQLite snapshot component",
            path: destination.to_path_buf(),
            source,
        })?;
    Ok(())
}

#[cfg(test)]
fn open_root_handle_sqlite_source_snapshot_for_test(
    authority: &SqliteSourceDirectoryAuthority,
    database_name: &OsStr,
    before_sqlite_open: impl FnOnce(),
) -> SqliteSourceAccessResult<SqliteSourceReadSnapshot> {
    open_root_handle_sqlite_source_snapshot_inner(authority, database_name, before_sqlite_open)
}

#[derive(Debug)]
struct SqliteSourceFamily {
    authority: SqliteSourceDirectoryAuthority,
    database: SqliteFamilyMember,
    wal: Option<SqliteFamilyMember>,
    shared_memory: Option<SqliteFamilyMember>,
    wal_path: PathBuf,
    shared_memory_path: PathBuf,
    journal_path: PathBuf,
}

impl SqliteSourceFamily {
    fn open(
        authority: &SqliteSourceDirectoryAuthority,
        database_name: &OsStr,
    ) -> SqliteSourceAccessResult<Self> {
        validate_database_leaf(database_name)?;
        authority.revalidate()?;
        let retained_authority = SqliteSourceDirectoryAuthority {
            directory: authority.directory.try_clone().map_err(|source| {
                SqliteSourceAccessError::Io {
                    operation: "retaining the SQLite source parent",
                    path: authority.path.clone(),
                    source,
                }
            })?,
            path: authority.path.clone(),
            identity: authority.identity.clone(),
        };
        let database_path = authority.path.join(database_name);
        let database = SqliteFamilyMember::open(database_path, ExpectedObjectKind::RegularFile)?;
        let wal_path = authority.path.join(with_suffix(database_name, "-wal"));
        let shared_memory_path = authority.path.join(with_suffix(database_name, "-shm"));
        let journal_path = authority.path.join(with_suffix(database_name, "-journal"));
        let wal = SqliteFamilyMember::open_optional(wal_path.clone())?;
        let shared_memory = SqliteFamilyMember::open_optional(shared_memory_path.clone())?;
        if SqliteFamilyMember::path_exists(&journal_path)? {
            return Err(SqliteSourceAccessError::UnsupportedSidecarIdentity {
                component: SqliteSourceComponent::RollbackJournal,
                capability: "read-only provider snapshots do not perform rollback recovery",
            });
        }
        Ok(Self {
            authority: retained_authority,
            database,
            wal,
            shared_memory,
            wal_path,
            shared_memory_path,
            journal_path,
        })
    }

    fn capture_evidence(&self) -> SqliteSourceAccessResult<SqliteFamilyEvidence> {
        Ok(SqliteFamilyEvidence {
            parent_identity: self.authority.identity.clone(),
            database: self.database.capture_state()?,
            wal: self
                .wal
                .as_ref()
                .map(SqliteFamilyMember::capture_state)
                .transpose()?,
            shared_memory: self
                .shared_memory
                .as_ref()
                .map(SqliteFamilyMember::capture_state)
                .transpose()?,
            wal_token: self
                .wal
                .as_ref()
                .map(SqliteFamilyMember::bounded_token)
                .transpose()?,
            shared_memory_token: self
                .shared_memory
                .as_ref()
                .map(SqliteFamilyMember::content_digest)
                .transpose()?,
        })
    }

    fn revalidate(&self, expected: &SqliteFamilyEvidence) -> SqliteSourceAccessResult<()> {
        self.authority.revalidate()?;
        if self.authority.identity != expected.parent_identity {
            return Err(SqliteSourceAccessError::SourceChanged);
        }
        self.database.revalidate(&expected.database)?;
        revalidate_optional_member(self.wal.as_ref(), expected.wal.as_ref(), &self.wal_path)?;
        revalidate_optional_member(
            self.shared_memory.as_ref(),
            expected.shared_memory.as_ref(),
            &self.shared_memory_path,
        )?;
        match (self.wal.as_ref(), expected.wal_token.as_ref()) {
            (Some(wal), Some(expected_token)) if wal.bounded_token()? == *expected_token => {}
            (None, None) => {}
            _ => return Err(SqliteSourceAccessError::SourceChanged),
        }
        match (
            self.shared_memory.as_ref(),
            expected.shared_memory_token.as_ref(),
        ) {
            (Some(shared_memory), Some(expected_token))
                if shared_memory.content_digest()? == *expected_token => {}
            (None, None) => {}
            _ => return Err(SqliteSourceAccessError::SourceChanged),
        }
        if SqliteFamilyMember::path_exists(&self.journal_path).map_err(map_revalidation_error)? {
            return Err(SqliteSourceAccessError::SourceChanged);
        }
        Ok(())
    }
}

#[derive(Debug)]
struct SqliteFamilyMember {
    file: File,
    path: PathBuf,
}

impl SqliteFamilyMember {
    fn open(path: PathBuf, kind: ExpectedObjectKind) -> SqliteSourceAccessResult<Self> {
        let file = open_nofollow(&path, kind)?;
        NativeFileState::read(&file, &path, kind)?;
        Ok(Self { file, path })
    }

    fn open_optional(path: PathBuf) -> SqliteSourceAccessResult<Option<Self>> {
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                validate_named_metadata(&path, &metadata, ExpectedObjectKind::RegularFile)?;
                Self::open(path, ExpectedObjectKind::RegularFile).map(Some)
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(SqliteSourceAccessError::Io {
                operation: "inspecting an optional SQLite source member",
                path,
                source,
            }),
        }
    }

    fn path_exists(path: &Path) -> SqliteSourceAccessResult<bool> {
        match fs::symlink_metadata(path) {
            Ok(metadata) => {
                validate_named_metadata(path, &metadata, ExpectedObjectKind::RegularFile)?;
                Ok(true)
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(SqliteSourceAccessError::Io {
                operation: "inspecting an optional SQLite source member",
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    fn capture_state(&self) -> SqliteSourceAccessResult<NativeFileState> {
        NativeFileState::read(&self.file, &self.path, ExpectedObjectKind::RegularFile)
    }

    fn revalidate(&self, expected: &NativeFileState) -> SqliteSourceAccessResult<()> {
        let retained = self.capture_state().map_err(map_revalidation_error)?;
        if &retained != expected {
            return Err(SqliteSourceAccessError::SourceChanged);
        }
        let named = open_nofollow(&self.path, ExpectedObjectKind::RegularFile)
            .map_err(map_revalidation_error)?;
        let named_state =
            NativeFileState::read(&named, &self.path, ExpectedObjectKind::RegularFile)
                .map_err(map_revalidation_error)?;
        if &named_state == expected {
            Ok(())
        } else {
            Err(SqliteSourceAccessError::SourceChanged)
        }
    }

    fn bounded_token(&self) -> SqliteSourceAccessResult<[u8; 32]> {
        let state = self.capture_state()?;
        let mut file = self
            .file
            .try_clone()
            .map_err(|source| SqliteSourceAccessError::Io {
                operation: "retaining the SQLite WAL for bounded revision evidence",
                path: self.path.clone(),
                source,
            })?;
        file.seek(SeekFrom::Start(0))
            .map_err(|source| SqliteSourceAccessError::Io {
                operation: "seeking the SQLite WAL for bounded revision evidence",
                path: self.path.clone(),
                source,
            })?;
        let prefix_len = usize::try_from(state.length.min(SQLITE_WAL_TOKEN_BYTES as u64))
            .map_err(|_| SqliteSourceAccessError::SourceChanged)?;
        let mut prefix = vec![0_u8; prefix_len];
        file.read_exact(&mut prefix)
            .map_err(|source| SqliteSourceAccessError::Io {
                operation: "reading the SQLite WAL prefix for bounded revision evidence",
                path: self.path.clone(),
                source,
            })?;
        let suffix_len = prefix_len;
        let mut suffix = vec![0_u8; suffix_len];
        if suffix_len > 0 {
            file.seek(SeekFrom::Start(state.length - suffix_len as u64))
                .map_err(|source| SqliteSourceAccessError::Io {
                    operation: "seeking the SQLite WAL suffix for bounded revision evidence",
                    path: self.path.clone(),
                    source,
                })?;
            file.read_exact(&mut suffix)
                .map_err(|source| SqliteSourceAccessError::Io {
                    operation: "reading the SQLite WAL suffix for bounded revision evidence",
                    path: self.path.clone(),
                    source,
                })?;
        }
        let mut digest = Sha256::new();
        digest.update(state.length.to_le_bytes());
        digest.update(prefix);
        digest.update(suffix);
        Ok(digest.finalize().into())
    }

    fn content_digest(&self) -> SqliteSourceAccessResult<[u8; 32]> {
        let state = self.capture_state()?;
        if state.length > SQLITE_SHM_MAX_BYTES {
            return Err(SqliteSourceAccessError::SnapshotTooLarge {
                path: self.path.clone(),
                length: state.length,
                maximum: SQLITE_SHM_MAX_BYTES,
            });
        }
        let mut file = self
            .file
            .try_clone()
            .map_err(|source| SqliteSourceAccessError::Io {
                operation: "retaining SQLite SHM for bounded content evidence",
                path: self.path.clone(),
                source,
            })?;
        file.seek(SeekFrom::Start(0))
            .map_err(|source| SqliteSourceAccessError::Io {
                operation: "seeking SQLite SHM for bounded content evidence",
                path: self.path.clone(),
                source,
            })?;
        let mut remaining = state.length;
        let mut buffer = vec![0_u8; SQLITE_COPY_BUFFER_BYTES];
        let mut digest = Sha256::new();
        digest.update(state.length.to_le_bytes());
        while remaining > 0 {
            let requested = usize::try_from(remaining.min(buffer.len() as u64))
                .map_err(|_| SqliteSourceAccessError::SourceChanged)?;
            file.read_exact(&mut buffer[..requested])
                .map_err(|source| SqliteSourceAccessError::Io {
                    operation: "reading SQLite SHM for bounded content evidence",
                    path: self.path.clone(),
                    source,
                })?;
            digest.update(&buffer[..requested]);
            remaining -= requested as u64;
        }
        Ok(digest.finalize().into())
    }
}

fn revalidate_optional_member(
    member: Option<&SqliteFamilyMember>,
    expected: Option<&NativeFileState>,
    path: &Path,
) -> SqliteSourceAccessResult<()> {
    match (member, expected) {
        (Some(member), Some(expected)) => member.revalidate(expected),
        (None, None) => {
            if SqliteFamilyMember::path_exists(path).map_err(map_revalidation_error)? {
                Err(SqliteSourceAccessError::SourceChanged)
            } else {
                Ok(())
            }
        }
        _ => Err(SqliteSourceAccessError::SourceChanged),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SqliteFamilyEvidence {
    parent_identity: NativeFileIdentity,
    database: NativeFileState,
    wal: Option<NativeFileState>,
    shared_memory: Option<NativeFileState>,
    wal_token: Option<[u8; 32]>,
    shared_memory_token: Option<[u8; 32]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SqliteSnapshotEvidence {
    schema: SqliteSchemaEvidence,
    source: SqliteConnectionEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SqliteSchemaEvidence {
    schema_version: i64,
    user_version: i64,
    application_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SqliteConnectionEvidence {
    data_version: i64,
    page_count: i64,
    freelist_count: i64,
}

impl SqliteSourceEvidence {
    fn from_snapshot(native: &SqliteFamilyEvidence, sqlite: &SqliteSnapshotEvidence) -> Self {
        let identity = native.database.identity.digest();
        let mut revision = Sha256::new();
        revision.update(EVIDENCE_DOMAIN);
        revision.update(b"revision\0");
        native.hash_into(&mut revision);
        sqlite.hash_into(&mut revision);
        Self {
            identity,
            length: native.database.length,
            wal_length: native.wal.as_ref().map(|state| state.length),
            shared_memory_length: native.shared_memory.as_ref().map(|state| state.length),
            schema: sqlite.schema.clone(),
            source: sqlite.source.clone(),
            revision: revision.finalize().into(),
        }
    }
}

impl SqliteFamilyEvidence {
    fn hash_into(&self, digest: &mut Sha256) {
        self.parent_identity.hash_into(digest);
        self.database.hash_into(digest);
        hash_optional_state(digest, self.wal.as_ref());
        // SHM is SQLite's volatile lock coordination, not provider content.
        // Stock read-only WAL readers may update its reader marks, so source
        // revisions intentionally derive from the DB, WAL, and SQLite evidence.
        match self.wal_token {
            Some(wal_token) => {
                digest.update([1]);
                digest.update(wal_token);
            }
            None => digest.update([0]),
        }
    }
}

impl SqliteSnapshotEvidence {
    fn hash_into(&self, digest: &mut Sha256) {
        digest.update(self.schema.schema_version.to_le_bytes());
        digest.update(self.schema.user_version.to_le_bytes());
        digest.update(self.schema.application_id.to_le_bytes());
        digest.update(self.source.data_version.to_le_bytes());
        digest.update(self.source.page_count.to_le_bytes());
        digest.update(self.source.freelist_count.to_le_bytes());
    }
}

fn hash_optional_state(digest: &mut Sha256, state: Option<&NativeFileState>) {
    match state {
        Some(state) => {
            digest.update([1]);
            state.hash_into(digest);
        }
        None => digest.update([0]),
    }
}

#[derive(Debug, Clone, Copy)]
enum ExpectedObjectKind {
    Directory,
    RegularFile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NativeFileState {
    identity: NativeFileIdentity,
    length: u64,
    platform: PlatformFileState,
}

impl NativeFileState {
    fn read(
        file: &File,
        path: &Path,
        expected_kind: ExpectedObjectKind,
    ) -> SqliteSourceAccessResult<Self> {
        let metadata = file
            .metadata()
            .map_err(|source| SqliteSourceAccessError::Io {
                operation: "reading retained SQLite source metadata",
                path: path.to_path_buf(),
                source,
            })?;
        validate_opened_metadata(path, &metadata, expected_kind)?;
        let (identity, platform) =
            platform_file_state(file, &metadata).map_err(|source| SqliteSourceAccessError::Io {
                operation: "reading native SQLite source identity",
                path: path.to_path_buf(),
                source,
            })?;
        Ok(Self {
            identity,
            length: metadata.len(),
            platform,
        })
    }

    fn hash_into(&self, digest: &mut Sha256) {
        self.identity.hash_into(digest);
        digest.update(self.length.to_le_bytes());
        self.platform.hash_into(digest);
    }
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct NativeFileIdentity {
    device: u64,
    inode: u64,
}

#[cfg(windows)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct NativeFileIdentity {
    volume_serial_number: u64,
    file_id: [u8; 16],
}

#[cfg(not(any(unix, windows)))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct NativeFileIdentity;

impl NativeFileIdentity {
    fn digest(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(EVIDENCE_DOMAIN);
        digest.update(b"identity\0");
        self.hash_into(&mut digest);
        digest.finalize().into()
    }

    fn hash_into(&self, digest: &mut Sha256) {
        #[cfg(unix)]
        {
            digest.update(self.device.to_le_bytes());
            digest.update(self.inode.to_le_bytes());
        }
        #[cfg(windows)]
        {
            digest.update(self.volume_serial_number.to_le_bytes());
            digest.update(self.file_id);
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = digest;
        }
    }
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct PlatformFileState {
    mode: u32,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

#[cfg(windows)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct PlatformFileState {
    creation_time: i64,
    last_write_time: i64,
    change_time: i64,
    attributes: u32,
}

#[cfg(not(any(unix, windows)))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct PlatformFileState;

impl PlatformFileState {
    fn hash_into(&self, digest: &mut Sha256) {
        #[cfg(unix)]
        {
            digest.update(self.mode.to_le_bytes());
            digest.update(self.modified_seconds.to_le_bytes());
            digest.update(self.modified_nanoseconds.to_le_bytes());
            digest.update(self.changed_seconds.to_le_bytes());
            digest.update(self.changed_nanoseconds.to_le_bytes());
        }
        #[cfg(windows)]
        {
            digest.update(self.creation_time.to_le_bytes());
            digest.update(self.last_write_time.to_le_bytes());
            digest.update(self.change_time.to_le_bytes());
            digest.update(self.attributes.to_le_bytes());
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = digest;
        }
    }
}

#[cfg(unix)]
fn platform_file_state(
    _file: &File,
    metadata: &Metadata,
) -> std::io::Result<(NativeFileIdentity, PlatformFileState)> {
    use std::os::unix::fs::MetadataExt;

    Ok((
        NativeFileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        },
        PlatformFileState {
            mode: metadata.mode(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        },
    ))
}

#[cfg(windows)]
fn platform_file_state(
    file: &File,
    _metadata: &Metadata,
) -> std::io::Result<(NativeFileIdentity, PlatformFileState)> {
    use std::{mem::size_of, os::windows::io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        FileBasicInfo, FileIdInfo, GetFileInformationByHandleEx, FILE_BASIC_INFO, FILE_ID_INFO,
    };

    let handle = file.as_raw_handle();
    let mut basic = FILE_BASIC_INFO::default();
    if unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileBasicInfo,
            (&mut basic as *mut FILE_BASIC_INFO).cast(),
            size_of::<FILE_BASIC_INFO>() as u32,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    let mut id = FILE_ID_INFO::default();
    if unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            (&mut id as *mut FILE_ID_INFO).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok((
        NativeFileIdentity {
            volume_serial_number: id.VolumeSerialNumber,
            file_id: id.FileId.Identifier,
        },
        PlatformFileState {
            creation_time: basic.CreationTime,
            last_write_time: basic.LastWriteTime,
            change_time: basic.ChangeTime,
            attributes: basic.FileAttributes,
        },
    ))
}

#[cfg(not(any(unix, windows)))]
fn platform_file_state(
    _file: &File,
    _metadata: &Metadata,
) -> std::io::Result<(NativeFileIdentity, PlatformFileState)> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "native SQLite source identity is unsupported on this platform",
    ))
}

fn validate_approved_parent_path(path: &Path) -> SqliteSourceAccessResult<()> {
    if !path.is_absolute()
        || path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(SqliteSourceAccessError::UnsafeFile {
            path: path.to_path_buf(),
            reason: "the approved SQLite parent path must be absolute and traversal-free",
        });
    }
    Ok(())
}

fn validate_database_leaf(name: &OsStr) -> SqliteSourceAccessResult<()> {
    let path = Path::new(name);
    if name.is_empty()
        || path.components().count() != 1
        || !matches!(path.components().next(), Some(Component::Normal(_)))
    {
        return Err(SqliteSourceAccessError::UnsafeFile {
            path: path.to_path_buf(),
            reason: "the SQLite database name must be one normal leaf component",
        });
    }
    Ok(())
}

fn with_suffix(name: &OsStr, suffix: &str) -> OsString {
    let mut value = name.to_os_string();
    value.push(suffix);
    value
}

fn open_nofollow(path: &Path, expected_kind: ExpectedObjectKind) -> SqliteSourceAccessResult<File> {
    let metadata = fs::symlink_metadata(path).map_err(|source| SqliteSourceAccessError::Io {
        operation: "inspecting a named SQLite source object",
        path: path.to_path_buf(),
        source,
    })?;
    validate_named_metadata(path, &metadata, expected_kind)?;
    let mut options = OpenOptions::new();
    options.read(true);
    configure_nofollow_open(&mut options, expected_kind);
    options
        .open(path)
        .map_err(|source| SqliteSourceAccessError::Io {
            operation: "opening a named SQLite source object without following",
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(unix)]
fn configure_nofollow_open(options: &mut OpenOptions, _expected_kind: ExpectedObjectKind) {
    use std::os::unix::fs::OpenOptionsExt;

    options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
}

#[cfg(windows)]
fn configure_nofollow_open(options: &mut OpenOptions, expected_kind: ExpectedObjectKind) {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    let mut flags = FILE_FLAG_OPEN_REPARSE_POINT;
    if matches!(expected_kind, ExpectedObjectKind::Directory) {
        flags |= FILE_FLAG_BACKUP_SEMANTICS;
    }
    options
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(flags);
}

#[cfg(not(any(unix, windows)))]
fn configure_nofollow_open(_options: &mut OpenOptions, _expected_kind: ExpectedObjectKind) {}

fn validate_named_metadata(
    path: &Path,
    metadata: &Metadata,
    expected_kind: ExpectedObjectKind,
) -> SqliteSourceAccessResult<()> {
    validate_opened_metadata(path, metadata, expected_kind)
}

fn validate_opened_metadata(
    path: &Path,
    metadata: &Metadata,
    expected_kind: ExpectedObjectKind,
) -> SqliteSourceAccessResult<()> {
    if metadata.file_type().is_symlink() || metadata_is_reparse_point(metadata) {
        return Err(SqliteSourceAccessError::UnsafeFile {
            path: path.to_path_buf(),
            reason: "symlink and reparse-point SQLite source objects are not allowed",
        });
    }
    let valid = match expected_kind {
        ExpectedObjectKind::Directory => metadata.is_dir(),
        ExpectedObjectKind::RegularFile => metadata.file_type().is_file(),
    };
    if valid {
        Ok(())
    } else {
        Err(SqliteSourceAccessError::UnsafeFile {
            path: path.to_path_buf(),
            reason: match expected_kind {
                ExpectedObjectKind::Directory => "the approved SQLite parent must be a directory",
                ExpectedObjectKind::RegularFile => {
                    "SQLite source family members must be regular files"
                }
            },
        })
    }
}

#[cfg(windows)]
fn metadata_is_reparse_point(metadata: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn metadata_is_reparse_point(_metadata: &Metadata) -> bool {
    false
}

fn map_revalidation_error(error: SqliteSourceAccessError) -> SqliteSourceAccessError {
    let _ = error;
    SqliteSourceAccessError::SourceChanged
}

fn configure_and_pin_snapshot(connection: &Connection) -> SqliteSourceAccessResult<()> {
    connection
        .set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)
        .map_err(|source| sqlite_error("disabling trusted provider schemas", source))?;
    connection
        .set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_TRIGGER, false)
        .map_err(|source| sqlite_error("disabling provider triggers", source))?;
    connection
        .set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
        .map_err(|source| sqlite_error("disabling provider WAL checkpoint-on-close", source))?;
    connection
        .pragma_update(None, "query_only", true)
        .map_err(|source| sqlite_error("enabling provider query-only mode", source))?;
    connection
        .pragma_update(None, "temp_store", "MEMORY")
        .map_err(|source| sqlite_error("forcing in-memory SQLite temporary storage", source))?;
    connection
        .pragma_update(None, "mmap_size", 0_i64)
        .map_err(|source| sqlite_error("disabling provider database mmap", source))?;
    let query_only: i64 = connection
        .pragma_query_value(None, "query_only", |row| row.get(0))
        .map_err(|source| sqlite_error("verifying provider query-only mode", source))?;
    if query_only != 1 {
        return Err(SqliteSourceAccessError::ConnectionNotQueryOnly);
    }
    connection
        .execute_batch("BEGIN DEFERRED")
        .map_err(|source| sqlite_error("starting the provider read snapshot", source))?;
    connection
        .query_row("SELECT count(*) FROM sqlite_schema", [], |row| {
            row.get::<_, i64>(0)
        })
        .map(|_| ())
        .map_err(|source| sqlite_error("pinning the provider read snapshot", source))?;
    install_snapshot_authorizer(connection)
}

fn capture_sqlite_evidence(
    connection: &Connection,
) -> SqliteSourceAccessResult<SqliteSnapshotEvidence> {
    Ok(SqliteSnapshotEvidence {
        schema: SqliteSchemaEvidence {
            schema_version: pragma_i64(connection, "schema_version")?,
            user_version: pragma_i64(connection, "user_version")?,
            application_id: pragma_i64(connection, "application_id")?,
        },
        source: SqliteConnectionEvidence {
            data_version: pragma_i64(connection, "data_version")?,
            page_count: pragma_i64(connection, "page_count")?,
            freelist_count: pragma_i64(connection, "freelist_count")?,
        },
    })
}

fn pragma_i64(connection: &Connection, name: &'static str) -> SqliteSourceAccessResult<i64> {
    connection
        .pragma_query_value(None, name, |row| row.get(0))
        .map_err(|source| sqlite_error("capturing provider SQLite evidence", source))
}

unsafe extern "C" fn deny_snapshot_transaction_control(
    _context: *mut c_void,
    action: i32,
    _argument_one: *const c_char,
    _argument_two: *const c_char,
    _database: *const c_char,
    _trigger: *const c_char,
) -> i32 {
    if matches!(action, ffi::SQLITE_TRANSACTION | ffi::SQLITE_SAVEPOINT) {
        ffi::SQLITE_DENY
    } else {
        ffi::SQLITE_OK
    }
}

fn install_snapshot_authorizer(connection: &Connection) -> SqliteSourceAccessResult<()> {
    let code = unsafe {
        ffi::sqlite3_set_authorizer(
            connection.handle(),
            Some(deny_snapshot_transaction_control),
            ptr::null_mut(),
        )
    };
    if code == ffi::SQLITE_OK {
        Ok(())
    } else {
        Err(SqliteSourceAccessError::SqliteControl {
            operation: "installing the provider snapshot transaction guard",
            code,
        })
    }
}

fn clear_snapshot_authorizer(connection: &Connection) -> SqliteSourceAccessResult<()> {
    let code = unsafe { ffi::sqlite3_set_authorizer(connection.handle(), None, ptr::null_mut()) };
    if code == ffi::SQLITE_OK {
        Ok(())
    } else {
        Err(SqliteSourceAccessError::SqliteControl {
            operation: "clearing the provider snapshot transaction guard",
            code,
        })
    }
}

fn verify_snapshot_active(connection: &Connection) -> SqliteSourceAccessResult<()> {
    if unsafe { ffi::sqlite3_get_autocommit(connection.handle()) } == 0 {
        Ok(())
    } else {
        Err(SqliteSourceAccessError::SnapshotNotActive)
    }
}

fn verify_connection_read_only(connection: &Connection) -> SqliteSourceAccessResult<()> {
    let readonly = unsafe { ffi::sqlite3_db_readonly(connection.handle(), c"main".as_ptr()) };
    if readonly == 1 {
        Ok(())
    } else {
        Err(SqliteSourceAccessError::ConnectionNotReadOnly)
    }
}

fn sqlite_error(operation: &'static str, source: rusqlite::Error) -> SqliteSourceAccessError {
    SqliteSourceAccessError::Sqlite { operation, source }
}

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
