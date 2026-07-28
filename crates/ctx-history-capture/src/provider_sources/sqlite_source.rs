//! SQLite connection binding for root-authorized provider source files.
//!
//! Path containment belongs to `common::io::root_handle`. This module accepts
//! the retained [`std::fs::File`] from that layer and separately accepts the
//! pathname SQLite must open. It does not canonicalize or independently walk
//! the authority root.
//!
//! On Linux, the bundled `unix` VFS is opened read-only. Before any provider SQL
//! is exposed, this module obtains SQLite's public `sqlite3_file`, asks that
//! exact VFS object to take a POSIX shared lock, and uses `F_OFD_GETLK` on the
//! caller's root-bound handle to prove that the lock is on the same file.
//! Ambiguous pre-existing lock state fails closed. A read transaction is then
//! pinned and the expected handle is revalidated before the guard is returned
//! and again when it is finished.
//!
//! This first, intentionally narrow checkpoint accepts main-file-only
//! snapshots. A connection that retains WAL or journal family files is rejected
//! with [`SqliteSourceAccessError::UnsupportedSourceFamily`]; it is never
//! exposed to provider queries.
//!
//! # Adapter migration
//!
//! 1. Open the database with `OpenedProviderSourceFile`.
//! 2. Pass its `file()` and the original SQLite pathname to
//!    [`open_sqlite_source_snapshot`].
//! 3. Run provider SQL only through [`SqliteSourceReadSnapshot::connection`].
//! 4. Call [`SqliteSourceReadSnapshot::finish`] before publishing observations,
//!    then call `OpenedProviderSourceFile::revalidate` to certify the named
//!    authority route.

use std::{
    fs::File,
    path::{Path, PathBuf},
};

use rusqlite::Connection;
use thiserror::Error;

#[cfg(target_os = "linux")]
use std::{
    ffi::CStr,
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    sync::Mutex,
};

#[cfg(target_os = "linux")]
use rusqlite::{config::DbConfig, ffi, OpenFlags};

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
    UnexpectedVfs {
        expected: &'static str,
        actual: String,
    },
    #[error("SQLite source connection is not read-only")]
    ConnectionNotReadOnly,
    #[error("SQLite main connection identity does not match the root-bound file")]
    ConnectionIdentityMismatch,
    #[error("SQLite main identity lock challenge encountered ambiguous lock state")]
    AmbiguousLockState,
    #[error("SQLite source file changed while its read snapshot was active")]
    SourceChanged,
    #[error("SQLite WAL/journal family snapshots are not supported by this guard")]
    UnsupportedSourceFamily,
    #[error("SQLite source snapshot transaction is no longer active")]
    SnapshotNotActive,
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
}

/// A read-only SQLite connection whose `main` file has already been matched to
/// the caller's retained, root-authorized file handle.
#[must_use = "call finish() after provider queries and before publishing observations"]
#[derive(Debug)]
pub(crate) struct SqliteSourceReadSnapshot {
    connection: Option<Connection>,
    evidence: SqliteSourceEvidence,
    #[cfg(target_os = "linux")]
    expected_file: File,
    #[cfg(target_os = "linux")]
    expected_state: LinuxFileState,
}

impl SqliteSourceReadSnapshot {
    pub(crate) fn connection(&self) -> SqliteSourceAccessResult<&Connection> {
        self.connection
            .as_ref()
            .ok_or(SqliteSourceAccessError::SnapshotNotActive)
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
            verify_sqlite_file_has_not_moved(connection)?;
            verify_expected_file(&self.expected_file, &self.expected_state)?;
            connection.execute_batch("ROLLBACK").map_err(|source| {
                SqliteSourceAccessError::Sqlite {
                    operation: "ending the provider read snapshot",
                    source,
                }
            })?;
            self.connection.take();
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
            let _ = connection.execute_batch("ROLLBACK");
        }
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

fn unsupported_platform() -> SqliteSourceAccessError {
    SqliteSourceAccessError::UnsupportedPlatform {
        platform: std::env::consts::OS,
        capability:
            "SQLite's public Unix VFS ABI does not expose a portable native main-file identity",
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

    verify_requested_vfs(&connection)?;
    verify_connection_read_only(&connection)?;
    verify_sqlite_file_has_not_moved(&connection)?;

    // This is the decisive pre-query proof. Paths and descriptor-table
    // snapshots are not part of it.
    verify_main_identity_with_lock_challenge(&connection, &expected_file)?;

    configure_and_pin_snapshot(&connection)?;
    verify_expected_file(&expected_file, &expected_state)?;

    Ok(SqliteSourceReadSnapshot {
        connection: Some(connection),
        evidence: expected_state.evidence(),
        expected_file,
        expected_state,
    })
}

#[cfg(target_os = "linux")]
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
    let journal_mode: String = connection
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .map_err(|source| sqlite_error("reading the provider journal mode", source))?;
    if journal_mode.eq_ignore_ascii_case("wal") {
        return Err(SqliteSourceAccessError::UnsupportedSourceFamily);
    }
    connection
        .execute_batch("BEGIN DEFERRED")
        .map_err(|source| sqlite_error("starting the provider read snapshot", source))?;
    connection
        .query_row("SELECT count(*) FROM sqlite_schema", [], |row| {
            row.get::<_, i64>(0)
        })
        .map(|_| ())
        .map_err(|source| sqlite_error("pinning the provider read snapshot", source))
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
fn verify_requested_vfs(connection: &Connection) -> SqliteSourceAccessResult<()> {
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
    if actual == UNIX_VFS {
        Ok(())
    } else {
        Err(SqliteSourceAccessError::UnexpectedVfs {
            expected: UNIX_VFS,
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
    use std::{fs, fs::File, path::Path};

    use rusqlite::{params, Connection};

    use super::{
        open_linux_snapshot, open_sqlite_source_snapshot, SqliteSourceAccessError,
        SqliteSourceReadSnapshot,
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
        let writer = Connection::open(&database).unwrap();
        let mode: String = writer
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        writer
            .execute("CREATE TABLE messages (body TEXT NOT NULL)", [])
            .unwrap();
        writer
            .set_db_config(
                rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
                true,
            )
            .unwrap();
        drop(writer);
        assert!(database.with_file_name("provider.sqlite-wal").exists());
        let root_bound = File::open(&database).unwrap();

        let result = open_sqlite_source_snapshot(&database, &root_bound);
        assert!(
            matches!(
                result,
                Err(SqliteSourceAccessError::UnsupportedSourceFamily)
            ),
            "{result:?}"
        );
    }
}
