//! SQLite connection binding for root-authorized provider source files.
//!
//! Path containment belongs to `common::io::root_handle`. This module accepts
//! the retained [`std::fs::File`] from that layer and separately accepts the
//! pathname SQLite must open. It does not canonicalize or independently walk
//! the authority root.
//!
//! [`open_root_handle_sqlite_source_snapshot`] is the adapter-facing opener. On
//! Linux it opens DB/WAL/SHM names once, relative to a retained authorized
//! parent directory, then registers a per-snapshot VFS implemented in
//! `root_handle_sqlite_vfs`:
//!
//! - `xOpen` duplicates only the admitted DB/WAL handles and rejects journals,
//!   temporary files, writes, truncation, deletion, and unknown names;
//! - main and SHM locking use OFD locks that conflict with ordinary SQLite
//!   POSIX locks;
//! - SHM is `MAP_SHARED | PROT_READ` and reported as read-only, so SQLite can
//!   coordinate a WAL reader without modifying provider read marks; and
//! - the pinned WAL prefix, family identities, named routes, and parent handle
//!   are revalidated while the read transaction is still active.
//!
//! The connection is query-only, disables checkpoint-on-close, stores temporary
//! data in memory, and never copies provider bodies. Existing rollback journals
//! fail closed because reading them could require recovery writes.
//!
//! [`open_sqlite_source_snapshot`] remains a main-file-only compatibility seam.
//! It proves stock `unix` VFS main identity with an OFD lock-owner challenge and
//! rejects WAL because stock SQLite exposes no Unix WAL/SHM native identity.
//!
//! macOS, FreeBSD, Windows, non-local filesystems, and Linux filesystems without
//! procfd, coherent mmap, mount identity, and OFD locks return typed errors.
//!
//! # Adapter migration
//!
//! 1. Retain the `ProviderSourceDirectory` containing the database.
//! 2. Hand its retained directory handle to
//!    [`retain_sqlite_source_directory_authority`] once; never reopen it by
//!    pathname.
//! 3. Pass that capability and the single database leaf to
//!    [`open_root_handle_sqlite_source_snapshot`].
//! 4. Run provider SQL only through [`SqliteSourceReadSnapshot::connection`].
//!    Transaction-control SQL is denied because the guard already owns the
//!    pinned read transaction.
//! 5. Call [`SqliteSourceReadSnapshot::finish`] before publishing observations,
//!    then revalidate the ordinary authority root to certify its outer route.

use std::{
    ffi::OsStr,
    fs::File,
    path::{Path, PathBuf},
};

use rusqlite::Connection;
use thiserror::Error;

#[cfg(target_os = "linux")]
use std::{
    ffi::{c_char, c_void, CStr},
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    ptr,
    sync::Mutex,
};

#[cfg(target_os = "linux")]
use rusqlite::{config::DbConfig, ffi, OpenFlags};

pub(crate) use super::root_handle_sqlite_vfs::SqliteSourceDirectoryAuthority;
use super::root_handle_sqlite_vfs::{
    RootHandleSqliteFamilyEvidence, RootHandleSqliteSource, RootHandleSqliteVfs,
    RootHandleSqliteVfsError,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SqliteSourceComponent {
    Wal,
    SharedMemory,
    RollbackJournal,
}

impl std::fmt::Display for SqliteSourceComponent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Wal => "WAL",
            Self::SharedMemory => "SHM",
            Self::RollbackJournal => "rollback journal",
        })
    }
}

#[derive(Debug, Error)]
pub(crate) enum SqliteSourceAccessError {
    #[error("SQLite source access is unsupported on {platform}: {capability}")]
    UnsupportedPlatform {
        platform: &'static str,
        capability: &'static str,
    },
    #[error("unsafe SQLite source file {path:?}: {reason}")]
    UnsafeFile { path: PathBuf, reason: &'static str },
    #[error("unsupported SQLite source filesystem at {path:?}: {filesystem}")]
    UnsafeFilesystem { path: PathBuf, filesystem: String },
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
    #[error("SQLite source VFS control {operation} failed with code {code}")]
    SqliteControl { operation: &'static str, code: i32 },
    #[error("SQLite source opened through unexpected VFS {actual:?}; expected {expected:?}")]
    UnexpectedVfs { expected: String, actual: String },
    #[error("SQLite source connection is not read-only")]
    ConnectionNotReadOnly,
    #[error("SQLite main connection identity does not match the root-bound file")]
    ConnectionIdentityMismatch,
    #[error("SQLite main identity lock challenge encountered ambiguous lock state")]
    AmbiguousLockState,
    #[error("SQLite source file changed while its read snapshot was active")]
    SourceChanged,
    #[error("SQLite {component} identity is unsupported: {capability}")]
    UnsupportedSidecarIdentity {
        component: SqliteSourceComponent,
        capability: &'static str,
    },
    #[error("SQLite source snapshot transaction is no longer active")]
    SnapshotNotActive,
    #[error(transparent)]
    RootHandleVfs(#[from] RootHandleSqliteVfsError),
}

pub(crate) type SqliteSourceAccessResult<T> = Result<T, SqliteSourceAccessError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SqliteSourceEvidence {
    identity: [u8; 32],
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
    wal_length: Option<u64>,
    shared_memory_length: Option<u64>,
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

    pub(crate) fn wal_length(&self) -> Option<u64> {
        self.wal_length
    }

    pub(crate) fn shared_memory_length(&self) -> Option<u64> {
        self.shared_memory_length
    }
}

/// A read-only SQLite connection whose `main` file has already been matched to
/// the caller's retained, root-authorized file handle.
#[must_use = "call finish() after provider queries and before publishing observations"]
#[derive(Debug)]
pub(crate) struct SqliteSourceReadSnapshot {
    connection: Option<Connection>,
    evidence: SqliteSourceEvidence,
    #[cfg(target_os = "linux")]
    expected_file: Option<File>,
    #[cfg(target_os = "linux")]
    expected_state: Option<LinuxFileState>,
    root_handle_source: Option<RootHandleSqliteSource>,
    root_handle_evidence: Option<RootHandleSqliteFamilyEvidence>,
    root_handle_vfs: Option<RootHandleSqliteVfs>,
}

impl SqliteSourceReadSnapshot {
    pub(crate) fn connection(&self) -> SqliteSourceAccessResult<&Connection> {
        let connection = self
            .connection
            .as_ref()
            .ok_or(SqliteSourceAccessError::SnapshotNotActive)?;
        #[cfg(target_os = "linux")]
        verify_snapshot_active(connection)?;
        Ok(connection)
    }

    pub(crate) fn evidence(&self) -> &SqliteSourceEvidence {
        &self.evidence
    }

    /// Ends the read transaction after revalidating both the root-bound handle
    /// and SQLite's retained native descriptor.
    pub(crate) fn finish(mut self) -> SqliteSourceAccessResult<SqliteSourceEvidence> {
        #[cfg(target_os = "linux")]
        {
            let connection = self
                .connection
                .as_ref()
                .ok_or(SqliteSourceAccessError::SnapshotNotActive)?;
            verify_snapshot_active(connection)?;
            if let (Some(source), Some(evidence)) = (
                self.root_handle_source.as_ref(),
                self.root_handle_evidence.as_ref(),
            ) {
                source.revalidate(evidence)?;
            } else {
                verify_sqlite_file_has_not_moved(connection)?;
                verify_expected_file(
                    self.expected_file
                        .as_ref()
                        .ok_or(SqliteSourceAccessError::SnapshotNotActive)?,
                    self.expected_state
                        .as_ref()
                        .ok_or(SqliteSourceAccessError::SnapshotNotActive)?,
                )?;
            }
            clear_snapshot_authorizer(connection)?;
            connection.execute_batch("ROLLBACK").map_err(|source| {
                SqliteSourceAccessError::Sqlite {
                    operation: "ending the provider read snapshot",
                    source,
                }
            })?;
            self.connection.take();
            self.root_handle_vfs.take();
            if let (Some(source), Some(evidence)) = (
                self.root_handle_source.as_ref(),
                self.root_handle_evidence.as_ref(),
            ) {
                source.revalidate(evidence)?;
            }
            return Ok(self.evidence.clone());
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(unsupported_platform())
        }
    }
}

impl Drop for SqliteSourceReadSnapshot {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.as_ref() {
            #[cfg(target_os = "linux")]
            let _ = clear_snapshot_authorizer(connection);
            let _ = connection.execute_batch("ROLLBACK");
        }
        self.connection.take();
        self.root_handle_vfs.take();
    }
}

/// Opens SQLite at `connection_path` and proves that its actual `main` file is
/// `root_bound_database` before returning a query-capable guard.
///
/// The caller must obtain `root_bound_database` through the shared no-follow
/// provider-source authority layer and retain that owner until after `finish`.
pub(crate) fn open_sqlite_source_snapshot(
    connection_path: &Path,
    root_bound_database: &File,
) -> SqliteSourceAccessResult<SqliteSourceReadSnapshot> {
    #[cfg(target_os = "linux")]
    {
        open_linux_snapshot(connection_path, root_bound_database, || {})
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (connection_path, root_bound_database);
        Err(unsupported_platform())
    }
}

/// Opens a SQLite family from one retained, root-authorized parent directory.
///
/// Every SQLite-read file is opened relative to `authority` before the VFS is
/// registered. SQLite receives only duplicated handles for those exact
/// objects; it never reopens the provider pathname.
pub(crate) fn open_root_handle_sqlite_source_snapshot(
    authority: &SqliteSourceDirectoryAuthority,
    database_name: &OsStr,
) -> SqliteSourceAccessResult<SqliteSourceReadSnapshot> {
    open_root_handle_sqlite_source_snapshot_inner(authority, database_name, || {})
}

/// Retains the exact parent-directory handle supplied by the ordinary
/// provider-source authority layer.
///
/// This is the narrow handoff seam until `ProviderSourceDirectory` exposes its
/// retained handle directly. Callers must not construct it from a pathname
/// reopen.
pub(crate) fn retain_sqlite_source_directory_authority(
    authorized_parent: &File,
) -> SqliteSourceAccessResult<SqliteSourceDirectoryAuthority> {
    SqliteSourceDirectoryAuthority::retain(authorized_parent).map_err(Into::into)
}

fn open_root_handle_sqlite_source_snapshot_inner(
    authority: &SqliteSourceDirectoryAuthority,
    database_name: &OsStr,
    before_vfs_open: impl FnOnce(),
) -> SqliteSourceAccessResult<SqliteSourceReadSnapshot> {
    #[cfg(target_os = "linux")]
    {
        let source = RootHandleSqliteSource::open(authority, database_name)?;
        before_vfs_open();
        let vfs = source.register_vfs()?;
        let vfs_name = vfs.name().to_owned();
        let connection = Connection::open_with_flags_and_vfs(
            vfs.virtual_path(),
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
            &vfs_name,
        )
        .map_err(|source| SqliteSourceAccessError::Sqlite {
            operation: "opening the root-handle provider database",
            source,
        })?;
        verify_requested_vfs(&connection, &vfs_name)?;
        let sqlite_file = sqlite_main_file(&connection)?;
        if !vfs.connection_main_matches(sqlite_file, &source)? {
            return Err(SqliteSourceAccessError::ConnectionIdentityMismatch);
        }
        verify_connection_read_only(&connection)?;
        configure_and_pin_snapshot(&connection, true)?;
        let family_evidence = source.capture_evidence()?;
        source.revalidate(&family_evidence)?;
        let evidence = SqliteSourceEvidence {
            identity: *family_evidence.revision(),
            length: family_evidence.database_length(),
            modified_seconds: 0,
            modified_nanoseconds: 0,
            changed_seconds: 0,
            changed_nanoseconds: 0,
            wal_length: family_evidence.wal_length(),
            shared_memory_length: family_evidence.shared_memory_length(),
            revision: *family_evidence.revision(),
        };
        return Ok(SqliteSourceReadSnapshot {
            connection: Some(connection),
            evidence,
            expected_file: None,
            expected_state: None,
            root_handle_source: Some(source),
            root_handle_evidence: Some(family_evidence),
            root_handle_vfs: Some(vfs),
        });
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (authority, database_name, before_vfs_open);
        Err(unsupported_platform())
    }
}

#[cfg(all(test, target_os = "linux"))]
fn open_root_handle_sqlite_source_snapshot_for_test(
    authority: &SqliteSourceDirectoryAuthority,
    database_name: &OsStr,
    before_vfs_open: impl FnOnce(),
) -> SqliteSourceAccessResult<SqliteSourceReadSnapshot> {
    open_root_handle_sqlite_source_snapshot_inner(authority, database_name, before_vfs_open)
}

#[cfg(not(target_os = "linux"))]
fn unsupported_platform() -> SqliteSourceAccessError {
    #[cfg(target_os = "macos")]
    let capability =
        "the root-handle VFS requires an audited descriptor-relative open and SHM lock bridge";
    #[cfg(target_os = "freebsd")]
    let capability =
        "the root-handle VFS requires an audited descriptor-relative open and SHM lock bridge";
    #[cfg(target_os = "windows")]
    let capability = "the root-handle VFS requires audited handle-relative family opens, file identity, and WAL shared-memory locking";
    #[cfg(not(any(target_os = "macos", target_os = "freebsd", target_os = "windows")))]
    let capability = "no safe native SQLite source-family identity binding is implemented";
    SqliteSourceAccessError::UnsupportedPlatform {
        platform: std::env::consts::OS,
        capability,
    }
}

#[cfg(target_os = "linux")]
static SQLITE_SOURCE_OPEN_LOCK: Mutex<()> = Mutex::new(());

#[cfg(target_os = "linux")]
const UNIX_VFS: &str = "unix";

#[cfg(target_os = "linux")]
const REVISION_DOMAIN: &[u8] = b"ctx-root-bound-sqlite-main-v1\0";

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct LinuxFileIdentity {
    device: u64,
    inode: u64,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, PartialEq, Eq)]
struct LinuxFileState {
    identity: LinuxFileIdentity,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

#[cfg(target_os = "linux")]
impl LinuxFileState {
    fn read(file: &File, path: &Path) -> SqliteSourceAccessResult<Self> {
        let metadata = file
            .metadata()
            .map_err(|source| SqliteSourceAccessError::Io {
                operation: "reading root-bound SQLite metadata",
                path: path.to_path_buf(),
                source,
            })?;
        if !metadata.file_type().is_file() {
            return Err(SqliteSourceAccessError::UnsafeFile {
                path: path.to_path_buf(),
                reason: "the root-bound SQLite source must be a regular file",
            });
        }
        Ok(Self {
            identity: LinuxFileIdentity {
                device: metadata.dev(),
                inode: metadata.ino(),
            },
            length: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        })
    }

    fn evidence(&self) -> SqliteSourceEvidence {
        use sha2::{Digest, Sha256};

        let mut identity = Sha256::new();
        identity.update(REVISION_DOMAIN);
        identity.update(b"identity\0");
        identity.update(self.identity.device.to_le_bytes());
        identity.update(self.identity.inode.to_le_bytes());
        let identity: [u8; 32] = identity.finalize().into();

        let mut revision = Sha256::new();
        revision.update(REVISION_DOMAIN);
        revision.update(identity);
        revision.update(self.length.to_le_bytes());
        revision.update(self.modified_seconds.to_le_bytes());
        revision.update(self.modified_nanoseconds.to_le_bytes());
        revision.update(self.changed_seconds.to_le_bytes());
        revision.update(self.changed_nanoseconds.to_le_bytes());
        SqliteSourceEvidence {
            identity,
            length: self.length,
            modified_seconds: self.modified_seconds,
            modified_nanoseconds: self.modified_nanoseconds,
            changed_seconds: self.changed_seconds,
            changed_nanoseconds: self.changed_nanoseconds,
            wal_length: None,
            shared_memory_length: None,
            revision: revision.finalize().into(),
        }
    }
}

#[cfg(target_os = "linux")]
fn open_linux_snapshot(
    connection_path: &Path,
    root_bound_database: &File,
    before_connection_open: impl FnOnce(),
) -> SqliteSourceAccessResult<SqliteSourceReadSnapshot> {
    let expected_state = LinuxFileState::read(root_bound_database, connection_path)?;
    qualify_linux_filesystem(root_bound_database, connection_path)?;
    let expected_file =
        root_bound_database
            .try_clone()
            .map_err(|source| SqliteSourceAccessError::Io {
                operation: "retaining the root-bound SQLite file",
                path: connection_path.to_path_buf(),
                source,
            })?;

    let _open_guard = SQLITE_SOURCE_OPEN_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    before_connection_open();
    let connection = Connection::open_with_flags_and_vfs(
        connection_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        UNIX_VFS,
    )
    .map_err(|source| SqliteSourceAccessError::Sqlite {
        operation: "opening the provider database",
        source,
    })?;

    verify_requested_vfs(&connection, UNIX_VFS)?;
    verify_connection_read_only(&connection)?;
    verify_sqlite_file_has_not_moved(&connection)?;

    // This is the decisive pre-query proof. Paths and descriptor-table
    // snapshots are not part of it.
    verify_main_identity_with_lock_challenge(&connection, &expected_file)?;

    configure_and_pin_snapshot(&connection, false)?;
    verify_expected_file(&expected_file, &expected_state)?;

    Ok(SqliteSourceReadSnapshot {
        connection: Some(connection),
        evidence: expected_state.evidence(),
        expected_file: Some(expected_file),
        expected_state: Some(expected_state),
        root_handle_source: None,
        root_handle_evidence: None,
        root_handle_vfs: None,
    })
}

#[cfg(target_os = "linux")]
fn configure_and_pin_snapshot(
    connection: &Connection,
    allow_wal: bool,
) -> SqliteSourceAccessResult<()> {
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
    let journal_mode: String = connection
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .map_err(|source| sqlite_error("reading the provider journal mode", source))?;
    if journal_mode.eq_ignore_ascii_case("wal") && !allow_wal {
        return Err(SqliteSourceAccessError::UnsupportedSidecarIdentity {
            component: SqliteSourceComponent::Wal,
            capability:
                "the stock Unix VFS exposes no native WAL/SHM identity; a root-handle VFS is required",
        });
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

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
fn verify_snapshot_active(connection: &Connection) -> SqliteSourceAccessResult<()> {
    if unsafe { ffi::sqlite3_get_autocommit(connection.handle()) } == 0 {
        Ok(())
    } else {
        Err(SqliteSourceAccessError::SnapshotNotActive)
    }
}

#[cfg(target_os = "linux")]
fn verify_main_identity_with_lock_challenge(
    connection: &Connection,
    expected_file: &File,
) -> SqliteSourceAccessResult<()> {
    if !matches!(probe_ofd_write_lock(expected_file)?, LockProbe::Unlocked) {
        return Err(SqliteSourceAccessError::AmbiguousLockState);
    }

    let sqlite_file = sqlite_main_file(connection)?;
    let methods = unsafe { sqlite_file.as_ref().and_then(|file| file.pMethods.as_ref()) }
        .ok_or(SqliteSourceAccessError::AmbiguousLockState)?;
    let lock = methods
        .xLock
        .ok_or(SqliteSourceAccessError::AmbiguousLockState)?;
    let unlock = methods
        .xUnlock
        .ok_or(SqliteSourceAccessError::AmbiguousLockState)?;

    let lock_code = unsafe { lock(sqlite_file, ffi::SQLITE_LOCK_SHARED) };
    if lock_code != ffi::SQLITE_OK {
        return Err(SqliteSourceAccessError::AmbiguousLockState);
    }
    let observed = probe_ofd_write_lock(expected_file);
    let unlock_code = unsafe { unlock(sqlite_file, ffi::SQLITE_LOCK_NONE) };
    if unlock_code != ffi::SQLITE_OK {
        return Err(SqliteSourceAccessError::AmbiguousLockState);
    }
    let observed = observed?;
    let current_pid = unsafe { libc::getpid() };
    if !matches!(
        observed,
        LockProbe::Conflicting {
            lock_type,
            owner_pid
        } if lock_type == libc::F_RDLCK as i16 && owner_pid == current_pid
    ) {
        return Err(SqliteSourceAccessError::ConnectionIdentityMismatch);
    }
    if !matches!(probe_ofd_write_lock(expected_file)?, LockProbe::Unlocked) {
        return Err(SqliteSourceAccessError::AmbiguousLockState);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn sqlite_main_file(connection: &Connection) -> SqliteSourceAccessResult<*mut ffi::sqlite3_file> {
    let mut file = std::ptr::null_mut::<ffi::sqlite3_file>();
    let code = unsafe {
        ffi::sqlite3_file_control(
            connection.handle(),
            c"main".as_ptr(),
            ffi::SQLITE_FCNTL_FILE_POINTER,
            (&mut file as *mut *mut ffi::sqlite3_file).cast(),
        )
    };
    if code != ffi::SQLITE_OK || file.is_null() {
        return Err(SqliteSourceAccessError::SqliteControl {
            operation: "reading SQLite's public main-file pointer",
            code,
        });
    }
    Ok(file)
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LockProbe {
    Unlocked,
    Conflicting {
        lock_type: i16,
        owner_pid: libc::pid_t,
    },
}

#[cfg(target_os = "linux")]
fn probe_ofd_write_lock(file: &File) -> SqliteSourceAccessResult<LockProbe> {
    let mut lock = libc::flock {
        l_type: libc::F_WRLCK as i16,
        l_whence: libc::SEEK_SET as i16,
        l_start: 0,
        l_len: 0,
        l_pid: 0,
    };
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_OFD_GETLK, &mut lock) } < 0 {
        return Err(SqliteSourceAccessError::UnsupportedPlatform {
            platform: "linux",
            capability: "the source filesystem must support F_OFD_GETLK",
        });
    }
    if lock.l_type == libc::F_UNLCK as i16 {
        Ok(LockProbe::Unlocked)
    } else {
        Ok(LockProbe::Conflicting {
            lock_type: lock.l_type,
            owner_pid: lock.l_pid,
        })
    }
}

#[cfg(target_os = "linux")]
fn sqlite_error(operation: &'static str, source: rusqlite::Error) -> SqliteSourceAccessError {
    SqliteSourceAccessError::Sqlite { operation, source }
}

#[cfg(target_os = "linux")]
fn verify_expected_file(file: &File, expected: &LinuxFileState) -> SqliteSourceAccessResult<()> {
    let current = LinuxFileState::read(file, Path::new("<root-bound SQLite file>"))?;
    if &current == expected {
        Ok(())
    } else {
        Err(SqliteSourceAccessError::SourceChanged)
    }
}

#[cfg(target_os = "linux")]
fn verify_requested_vfs(
    connection: &Connection,
    expected_vfs: &str,
) -> SqliteSourceAccessResult<()> {
    let mut vfs = std::ptr::null_mut::<ffi::sqlite3_vfs>();
    let code = unsafe {
        ffi::sqlite3_file_control(
            connection.handle(),
            c"main".as_ptr(),
            ffi::SQLITE_FCNTL_VFS_POINTER,
            (&mut vfs as *mut *mut ffi::sqlite3_vfs).cast(),
        )
    };
    if code != ffi::SQLITE_OK {
        return Err(SqliteSourceAccessError::SqliteControl {
            operation: "reading the provider VFS pointer",
            code,
        });
    }
    let actual = unsafe {
        if vfs.is_null() || (*vfs).zName.is_null() {
            "<null>".to_owned()
        } else {
            CStr::from_ptr((*vfs).zName).to_string_lossy().into_owned()
        }
    };
    if actual == expected_vfs {
        Ok(())
    } else {
        Err(SqliteSourceAccessError::UnexpectedVfs {
            expected: expected_vfs.to_owned(),
            actual,
        })
    }
}

#[cfg(target_os = "linux")]
fn verify_connection_read_only(connection: &Connection) -> SqliteSourceAccessResult<()> {
    let readonly = unsafe { ffi::sqlite3_db_readonly(connection.handle(), c"main".as_ptr()) };
    if readonly == 1 {
        Ok(())
    } else {
        Err(SqliteSourceAccessError::ConnectionNotReadOnly)
    }
}

#[cfg(target_os = "linux")]
fn verify_sqlite_file_has_not_moved(connection: &Connection) -> SqliteSourceAccessResult<()> {
    let mut moved = 0_i32;
    let code = unsafe {
        ffi::sqlite3_file_control(
            connection.handle(),
            c"main".as_ptr(),
            ffi::SQLITE_FCNTL_HAS_MOVED,
            (&mut moved as *mut i32).cast(),
        )
    };
    if code != ffi::SQLITE_OK {
        return Err(SqliteSourceAccessError::SqliteControl {
            operation: "checking whether the provider main file moved",
            code,
        });
    }
    if moved == 0 {
        Ok(())
    } else {
        Err(SqliteSourceAccessError::ConnectionIdentityMismatch)
    }
}

#[cfg(target_os = "linux")]
fn qualify_linux_filesystem(file: &File, path: &Path) -> SqliteSourceAccessResult<()> {
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    if unsafe { libc::fstatfs(file.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(SqliteSourceAccessError::Io {
            operation: "identifying the SQLite source filesystem",
            path: path.to_path_buf(),
            source: std::io::Error::last_os_error(),
        });
    }
    let filesystem_type = unsafe { stat.assume_init() }.f_type as u64;
    match filesystem_type {
        0xEF53 | 0x5846_5342 | 0x9123_683E | 0xF2F5_2010 | 0x2FC1_2FC1 | 0x0102_1994
        | 0x8584_58F6 | 0x794C_7630 | 0x2405_1905 | 0x3153_464A => Ok(()),
        other => Err(SqliteSourceAccessError::UnsafeFilesystem {
            path: path.to_path_buf(),
            filesystem: format!("unqualified Linux filesystem magic 0x{other:x}"),
        }),
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::{ffi::OsStr, fs, fs::File, path::Path};

    use rusqlite::{config::DbConfig, ffi, params, Connection, OpenFlags};

    use super::{
        open_linux_snapshot, open_root_handle_sqlite_source_snapshot,
        open_root_handle_sqlite_source_snapshot_for_test, open_sqlite_source_snapshot,
        probe_ofd_write_lock, retain_sqlite_source_directory_authority, LockProbe,
        RootHandleSqliteVfsError, SqliteSourceAccessError, SqliteSourceComponent,
        SqliteSourceDirectoryAuthority, SqliteSourceReadSnapshot,
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

    fn read_value(snapshot: &SqliteSourceReadSnapshot) -> String {
        snapshot
            .connection()
            .unwrap()
            .query_row("SELECT body FROM messages", [], |row| row.get(0))
            .unwrap()
    }

    fn retain_parent(path: &Path) -> SqliteSourceDirectoryAuthority {
        let directory = File::open(path).unwrap();
        retain_sqlite_source_directory_authority(&directory).unwrap()
    }

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

    #[test]
    fn root_bound_main_identity_is_proven_before_queries() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("provider.sqlite");
        create_database(&database, "expected");
        let root_bound = File::open(&database).unwrap();

        let snapshot = open_sqlite_source_snapshot(&database, &root_bound).unwrap();
        assert_eq!(read_value(&snapshot), "expected");
        assert!(snapshot.evidence().length() > 0);
        assert_ne!(snapshot.evidence().identity(), &[0; 32]);
        assert_ne!(snapshot.finish().unwrap().revision(), &[0; 32]);
    }

    #[test]
    fn ancestor_swap_cannot_redirect_sqlite() {
        let temp = tempfile::tempdir().unwrap();
        let live = temp.path().join("live");
        let replacement = temp.path().join("replacement");
        let original = temp.path().join("original");
        fs::create_dir(&live).unwrap();
        fs::create_dir(&replacement).unwrap();
        create_database(&live.join("provider.sqlite"), "expected");
        create_database(&replacement.join("provider.sqlite"), "attacker");
        let connection_path = live.join("provider.sqlite");
        let root_bound = File::open(&connection_path).unwrap();

        let result = open_linux_snapshot(&connection_path, &root_bound, || {
            fs::rename(&live, &original).unwrap();
            fs::rename(&replacement, &live).unwrap();
        });
        assert!(matches!(
            result,
            Err(SqliteSourceAccessError::ConnectionIdentityMismatch)
        ));
    }

    #[test]
    fn leaf_swap_cannot_redirect_sqlite() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("provider.sqlite");
        let attacker = temp.path().join("attacker.sqlite");
        let original = temp.path().join("original.sqlite");
        create_database(&database, "expected");
        create_database(&attacker, "attacker");
        let root_bound = File::open(&database).unwrap();

        let result = open_linux_snapshot(&database, &root_bound, || {
            fs::rename(&database, &original).unwrap();
            fs::rename(&attacker, &database).unwrap();
        });
        assert!(matches!(
            result,
            Err(SqliteSourceAccessError::ConnectionIdentityMismatch)
        ));
    }

    #[test]
    fn connection_expected_identity_mismatch_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let expected = temp.path().join("expected.sqlite");
        let unrelated = temp.path().join("unrelated.sqlite");
        create_database(&expected, "expected");
        create_database(&unrelated, "unrelated");
        let root_bound = File::open(&expected).unwrap();

        let result = open_sqlite_source_snapshot(&unrelated, &root_bound);
        assert!(matches!(
            result,
            Err(SqliteSourceAccessError::ConnectionIdentityMismatch)
        ));
    }

    #[test]
    fn wal_family_is_typed_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("provider.sqlite");
        create_persistent_wal(&database);
        let root_bound = File::open(&database).unwrap();

        let result = open_sqlite_source_snapshot(&database, &root_bound);
        assert!(
            matches!(
                result,
                Err(SqliteSourceAccessError::UnsupportedSidecarIdentity {
                    component: SqliteSourceComponent::Wal,
                    ..
                })
            ),
            "{result:?}"
        );
    }

    #[test]
    fn root_handle_vfs_queries_active_wal_without_writes() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("provider.sqlite");
        create_persistent_wal(&database);
        let wal = database.with_file_name("provider.sqlite-wal");
        let shm = database.with_file_name("provider.sqlite-shm");
        let before_database = fs::read(&database).unwrap();
        let before_wal = fs::read(&wal).unwrap();
        let before_shm = fs::read(&shm).unwrap();
        let parent = retain_parent(temp.path());

        let snapshot =
            open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite"))
                .unwrap();
        assert_eq!(read_value(&snapshot), "from-wal");
        assert!(
            snapshot
                .connection()
                .unwrap()
                .execute_batch("COMMIT")
                .is_err(),
            "consumers may not replace the guard-owned read transaction"
        );
        assert_eq!(read_value(&snapshot), "from-wal");
        assert!(snapshot.evidence().wal_length().is_some());
        assert!(snapshot.evidence().shared_memory_length().is_some());
        snapshot.finish().unwrap();
        assert_eq!(fs::read(&database).unwrap(), before_database);
        assert_eq!(fs::read(&wal).unwrap(), before_wal);
        assert_eq!(fs::read(&shm).unwrap(), before_shm);
    }

    #[test]
    fn root_handle_vfs_keeps_a_snapshot_while_wal_appends() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("provider.sqlite");
        let writer = Connection::open(&database).unwrap();
        let mode: String = writer
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        writer
            .execute("CREATE TABLE messages (body TEXT NOT NULL)", [])
            .unwrap();
        writer
            .execute("INSERT INTO messages (body) VALUES ('pinned')", [])
            .unwrap();
        writer
            .set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
            .unwrap();
        let parent = retain_parent(temp.path());
        let snapshot =
            open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite"))
                .unwrap();

        writer
            .execute("INSERT INTO messages (body) VALUES ('later')", [])
            .unwrap();
        let values = snapshot
            .connection()
            .unwrap()
            .prepare("SELECT body FROM messages ORDER BY rowid")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(values, ["pinned"]);
        snapshot.finish().unwrap();
        drop(writer);
    }

    #[test]
    fn root_handle_vfs_leaf_swap_is_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("provider.sqlite");
        let attacker = temp.path().join("attacker.sqlite");
        let admitted = temp.path().join("admitted.sqlite");
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
            Err(SqliteSourceAccessError::RootHandleVfs(
                RootHandleSqliteVfsError::SourceChanged { .. }
            ))
        ));
    }

    #[test]
    fn root_handle_vfs_ancestor_swap_cannot_redirect_reads() {
        let temp = tempfile::tempdir().unwrap();
        let live = temp.path().join("live");
        let replacement = temp.path().join("replacement");
        let admitted = temp.path().join("admitted");
        fs::create_dir(&live).unwrap();
        fs::create_dir(&replacement).unwrap();
        create_database(&live.join("provider.sqlite"), "expected");
        create_database(&replacement.join("provider.sqlite"), "attacker");
        let parent = retain_parent(&live);

        let result = open_root_handle_sqlite_source_snapshot_for_test(
            &parent,
            OsStr::new("provider.sqlite"),
            || {
                fs::rename(&live, &admitted).unwrap();
                fs::rename(&replacement, &live).unwrap();
            },
        );
        match result {
            Ok(snapshot) => {
                assert_eq!(read_value(&snapshot), "expected");
                snapshot.finish().unwrap();
            }
            Err(SqliteSourceAccessError::RootHandleVfs(
                RootHandleSqliteVfsError::SourceChanged { .. },
            )) => {}
            Err(error) => panic!("unexpected ancestor-swap result: {error:?}"),
        }
    }

    #[test]
    fn root_handle_vfs_sidecar_swap_is_rejected_before_publication() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("provider.sqlite");
        create_persistent_wal(&database);
        let wal = database.with_file_name("provider.sqlite-wal");
        let admitted_wal = database.with_file_name("admitted.sqlite-wal");
        let parent = retain_parent(temp.path());
        let snapshot =
            open_root_handle_sqlite_source_snapshot(&parent, OsStr::new("provider.sqlite"))
                .unwrap();
        assert_eq!(read_value(&snapshot), "from-wal");

        fs::rename(&wal, &admitted_wal).unwrap();
        fs::write(&wal, b"substituted WAL").unwrap();
        let result = snapshot.finish();
        assert!(matches!(
            result,
            Err(SqliteSourceAccessError::RootHandleVfs(
                RootHandleSqliteVfsError::SourceChanged { .. }
            ))
        ));
    }

    #[test]
    fn stock_unix_wal_pointer_has_no_identity_lock() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("provider.sqlite");
        create_persistent_wal(&database);
        let wal = File::open(database.with_file_name("provider.sqlite-wal")).unwrap();
        let connection = Connection::open_with_flags_and_vfs(
            &database,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_PRIVATE_CACHE
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
            "unix",
        )
        .unwrap();
        connection
            .query_row("SELECT count(*) FROM sqlite_schema", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap();

        let mut sqlite_wal = std::ptr::null_mut::<ffi::sqlite3_file>();
        let code = unsafe {
            ffi::sqlite3_file_control(
                connection.handle(),
                c"main".as_ptr(),
                ffi::SQLITE_FCNTL_JOURNAL_POINTER,
                (&mut sqlite_wal as *mut *mut ffi::sqlite3_file).cast(),
            )
        };
        assert_eq!(code, ffi::SQLITE_OK);
        assert!(!sqlite_wal.is_null());
        assert_eq!(probe_ofd_write_lock(&wal).unwrap(), LockProbe::Unlocked);

        let methods = unsafe { (*sqlite_wal).pMethods.as_ref().unwrap() };
        let lock = methods.xLock.unwrap();
        let unlock = methods.xUnlock.unwrap();
        assert_eq!(
            unsafe { lock(sqlite_wal, ffi::SQLITE_LOCK_SHARED) },
            ffi::SQLITE_OK
        );
        assert_eq!(
            probe_ofd_write_lock(&wal).unwrap(),
            LockProbe::Unlocked,
            "bundled SQLite gives WAL files nolockIoMethods, so xLock cannot link the WAL to the authorized handle"
        );
        assert_eq!(
            unsafe { unlock(sqlite_wal, ffi::SQLITE_LOCK_NONE) },
            ffi::SQLITE_OK
        );
    }
}
