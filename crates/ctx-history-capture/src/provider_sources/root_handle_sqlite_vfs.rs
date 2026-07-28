//! Narrow read-only SQLite VFS backed by root-authorized file handles.
//!
//! This module intentionally does not traverse an authority root. Its input is
//! a retained parent-directory handle supplied by the ordinary source-authority
//! layer plus one leaf name. Main, WAL, SHM, and journal names are opened
//! relative to that exact directory with `O_NOFOLLOW`.
//!
//! # Safety argument
//!
//! Admission retains the authorized parent and every existing SQLite family
//! member. A fresh procfd open-file description is accepted only after its
//! inode, device, mode, and mount identity match the retained member. `xOpen`
//! can duplicate only those descriptions, and the connection's public
//! `sqlite3_file` is checked against the VFS context and admitted main handle
//! before any provider query.
//!
//! A SQLite shared lock pins the read transaction. Existing WAL bytes are
//! hashed after that pin; WAL append and SHM coordination may continue, but
//! identities, the pinned WAL prefix, current authority-relative names, and
//! journal absence must revalidate before observations can be published.
//! `xWrite`, `xTruncate`, `xSync`, `xDelete`, recovery journals, temporary
//! files, and unknown opens all fail closed.
//!
//! The implementation is Linux-only. It requires procfd identity, OFD locks
//! that conflict with SQLite's POSIX locks, coherent shared read-only mmap, and
//! a qualified local filesystem. Other platforms and filesystems return typed
//! unsupported/unsafe errors rather than falling back to pathname checks.

use std::{ffi::OsStr, fs::File, io, path::Path};

use thiserror::Error;

#[cfg(target_os = "linux")]
use std::{
    ffi::{CStr, CString, OsString},
    mem::size_of,
    os::{
        fd::{AsRawFd, FromRawFd, RawFd},
        raw::{c_char, c_int, c_void},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::Component,
    ptr,
    sync::atomic::{fence, AtomicU64, Ordering},
};

#[cfg(target_os = "linux")]
use rusqlite::ffi;
#[cfg(target_os = "linux")]
use sha2::{Digest, Sha256};

#[cfg(target_os = "linux")]
const EVIDENCE_DOMAIN: &[u8] = b"ctx-root-handle-sqlite-family-v1\0";
#[cfg(target_os = "linux")]
const UNIX_SHM_BASE: libc::off_t = ((22 + ffi::SQLITE_SHM_NLOCK) * 4) as libc::off_t;
#[cfg(target_os = "linux")]
const UNIX_SHM_DMS: libc::off_t = UNIX_SHM_BASE + ffi::SQLITE_SHM_NLOCK as libc::off_t;
#[cfg(target_os = "linux")]
const PENDING_BYTE: libc::off_t = 0x4000_0000;
#[cfg(target_os = "linux")]
const RESERVED_BYTE: libc::off_t = PENDING_BYTE + 1;
#[cfg(target_os = "linux")]
const SHARED_FIRST: libc::off_t = PENDING_BYTE + 2;
#[cfg(target_os = "linux")]
const SHARED_SIZE: libc::off_t = 510;

#[cfg(target_os = "linux")]
static NEXT_VFS_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RootHandleSqliteComponent {
    Database,
    Wal,
    SharedMemory,
    RollbackJournal,
}

impl std::fmt::Display for RootHandleSqliteComponent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Database => "database",
            Self::Wal => "WAL",
            Self::SharedMemory => "SHM",
            Self::RollbackJournal => "rollback journal",
        })
    }
}

#[derive(Debug, Error)]
pub(crate) enum RootHandleSqliteVfsError {
    #[error("root-handle SQLite VFS is unsupported on {platform}: {capability}")]
    Unsupported {
        platform: &'static str,
        capability: &'static str,
    },
    #[error("unsafe root-handle SQLite source: {reason}")]
    UnsafeSource { reason: &'static str },
    #[error("root-handle SQLite {component} I/O failed during {operation}: {source}")]
    Io {
        component: RootHandleSqliteComponent,
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("root-handle SQLite family has an existing rollback journal")]
    RollbackJournalPresent,
    #[error("root-handle SQLite family has a WAL without an existing SHM file")]
    WalWithoutSharedMemory,
    #[error("root-handle SQLite VFS registration failed with SQLite code {code}")]
    Registration { code: i32 },
    #[error("root-handle SQLite source family changed at {component}")]
    SourceChanged {
        component: RootHandleSqliteComponent,
    },
}

pub(crate) type RootHandleSqliteVfsResult<T> = Result<T, RootHandleSqliteVfsError>;

/// Retained parent-directory capability handed off by the ordinary authority
/// layer. The SQLite opener accepts this type instead of a database pathname or
/// database file handle.
#[derive(Debug)]
pub(crate) struct SqliteSourceDirectoryAuthority {
    directory: File,
    #[cfg(target_os = "linux")]
    opened: FileStamp,
}

impl SqliteSourceDirectoryAuthority {
    pub(crate) fn retain(authorized_parent: &File) -> RootHandleSqliteVfsResult<Self> {
        let directory =
            authorized_parent
                .try_clone()
                .map_err(|source| RootHandleSqliteVfsError::Io {
                    component: RootHandleSqliteComponent::Database,
                    operation: "retaining the authorized parent directory",
                    source,
                })?;
        let metadata = directory
            .metadata()
            .map_err(|source| RootHandleSqliteVfsError::Io {
                component: RootHandleSqliteComponent::Database,
                operation: "checking the authorized parent directory",
                source,
            })?;
        if !metadata.is_dir() {
            return Err(RootHandleSqliteVfsError::UnsafeSource {
                reason: "the SQLite authority parent must be a directory handle",
            });
        }
        #[cfg(target_os = "linux")]
        let opened = FileStamp::read(&directory, RootHandleSqliteComponent::Database)?;
        Ok(Self {
            directory,
            #[cfg(target_os = "linux")]
            opened,
        })
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileStamp {
    device: u64,
    inode: u64,
    mode: u32,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

#[cfg(target_os = "linux")]
impl FileStamp {
    fn read(file: &File, component: RootHandleSqliteComponent) -> RootHandleSqliteVfsResult<Self> {
        let metadata = file
            .metadata()
            .map_err(|source| RootHandleSqliteVfsError::Io {
                component,
                operation: "reading retained metadata",
                source,
            })?;
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            length: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        })
    }

    fn identity_eq(&self, other: &Self) -> bool {
        self.device == other.device && self.inode == other.inode && self.mode == other.mode
    }

    fn read_descriptor(
        descriptor: RawFd,
        component: RootHandleSqliteComponent,
    ) -> RootHandleSqliteVfsResult<Self> {
        let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
        if descriptor < 0 || unsafe { libc::fstat(descriptor, stat.as_mut_ptr()) } != 0 {
            return Err(RootHandleSqliteVfsError::Io {
                component,
                operation: "reading SQLite's opened component identity",
                source: io::Error::last_os_error(),
            });
        }
        let stat = unsafe { stat.assume_init() };
        Ok(Self {
            device: stat.st_dev as u64,
            inode: stat.st_ino as u64,
            mode: stat.st_mode as u32,
            length: stat.st_size as u64,
            modified_seconds: stat.st_mtime as i64,
            modified_nanoseconds: stat.st_mtime_nsec as i64,
            changed_seconds: stat.st_ctime as i64,
            changed_nanoseconds: stat.st_ctime_nsec as i64,
        })
    }

    fn hash_into(&self, digest: &mut Sha256) {
        digest.update(self.device.to_le_bytes());
        digest.update(self.inode.to_le_bytes());
        digest.update(self.mode.to_le_bytes());
        digest.update(self.length.to_le_bytes());
        digest.update(self.modified_seconds.to_le_bytes());
        digest.update(self.modified_nanoseconds.to_le_bytes());
        digest.update(self.changed_seconds.to_le_bytes());
        digest.update(self.changed_nanoseconds.to_le_bytes());
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
pub(crate) struct RootHandleSqliteSource {
    parent: File,
    database_name: OsString,
    database: File,
    wal: Option<File>,
    shared_memory: Option<File>,
    parent_opened: FileStamp,
}

#[cfg(not(target_os = "linux"))]
#[derive(Debug)]
pub(crate) struct RootHandleSqliteSource;

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RootHandleSqliteFamilyEvidence {
    parent: FileStamp,
    database: FileStamp,
    wal: Option<FileStamp>,
    shared_memory: Option<FileStamp>,
    wal_prefix_digest: Option<[u8; 32]>,
    revision: [u8; 32],
}

#[cfg(not(target_os = "linux"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RootHandleSqliteFamilyEvidence {
    revision: [u8; 32],
}

impl RootHandleSqliteFamilyEvidence {
    pub(crate) fn revision(&self) -> &[u8; 32] {
        &self.revision
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn database_length(&self) -> u64 {
        self.database.length
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn wal_length(&self) -> Option<u64> {
        self.wal.as_ref().map(|stamp| stamp.length)
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn shared_memory_length(&self) -> Option<u64> {
        self.shared_memory.as_ref().map(|stamp| stamp.length)
    }
}

impl RootHandleSqliteSource {
    /// Opens one SQLite family relative to an already-authorized parent handle.
    pub(crate) fn open(
        authority: &SqliteSourceDirectoryAuthority,
        database_name: &OsStr,
    ) -> RootHandleSqliteVfsResult<Self> {
        #[cfg(target_os = "linux")]
        {
            validate_leaf(database_name)?;
            let parent =
                authority
                    .directory
                    .try_clone()
                    .map_err(|source| RootHandleSqliteVfsError::Io {
                        component: RootHandleSqliteComponent::Database,
                        operation: "retaining the authorized parent directory",
                        source,
                    })?;
            let parent_opened = FileStamp::read(&parent, RootHandleSqliteComponent::Database)?;
            if parent_opened != authority.opened {
                return Err(changed(RootHandleSqliteComponent::Database));
            }
            let database =
                open_required_at(&parent, database_name, RootHandleSqliteComponent::Database)?;
            let wal_name = with_suffix(database_name, "-wal");
            let shm_name = with_suffix(database_name, "-shm");
            let journal_name = with_suffix(database_name, "-journal");
            let wal = open_optional_at(&parent, &wal_name, RootHandleSqliteComponent::Wal)?;
            let shared_memory = open_optional_at_mode(
                &parent,
                &shm_name,
                RootHandleSqliteComponent::SharedMemory,
                true,
            )?;
            let journal = open_optional_at(
                &parent,
                &journal_name,
                RootHandleSqliteComponent::RollbackJournal,
            )?;
            if journal.is_some() {
                return Err(RootHandleSqliteVfsError::RollbackJournalPresent);
            }
            if wal.is_some() && shared_memory.is_none() {
                return Err(RootHandleSqliteVfsError::WalWithoutSharedMemory);
            }
            ensure_regular(&database, RootHandleSqliteComponent::Database)?;
            if let Some(file) = wal.as_ref() {
                ensure_regular(file, RootHandleSqliteComponent::Wal)?;
            }
            if let Some(file) = shared_memory.as_ref() {
                ensure_regular(file, RootHandleSqliteComponent::SharedMemory)?;
            }
            ensure_supported_filesystem(&database)?;
            ensure_same_filesystem(&parent, &database)?;
            if let Some(file) = wal.as_ref() {
                ensure_same_filesystem(&database, file)?;
            }
            if let Some(file) = shared_memory.as_ref() {
                ensure_same_filesystem(&database, file)?;
            }
            return Ok(Self {
                parent,
                database_name: database_name.to_os_string(),
                database,
                wal,
                shared_memory,
                parent_opened,
            });
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (authority, database_name);
            Err(unsupported())
        }
    }

    pub(crate) fn register_vfs(&self) -> RootHandleSqliteVfsResult<RootHandleSqliteVfs> {
        RootHandleSqliteVfs::register(self)
    }

    pub(crate) fn capture_evidence(
        &self,
    ) -> RootHandleSqliteVfsResult<RootHandleSqliteFamilyEvidence> {
        #[cfg(target_os = "linux")]
        {
            let parent = FileStamp::read(&self.parent, RootHandleSqliteComponent::Database)?;
            let database = FileStamp::read(&self.database, RootHandleSqliteComponent::Database)?;
            let wal = self
                .wal
                .as_ref()
                .map(|file| FileStamp::read(file, RootHandleSqliteComponent::Wal))
                .transpose()?;
            let shared_memory = self
                .shared_memory
                .as_ref()
                .map(|file| FileStamp::read(file, RootHandleSqliteComponent::SharedMemory))
                .transpose()?;
            let wal_prefix_digest = match (self.wal.as_ref(), wal.as_ref()) {
                (Some(file), Some(stamp)) => Some(hash_prefix(file, stamp.length)?),
                _ => None,
            };
            let revision = family_revision(
                &parent,
                &database,
                wal.as_ref(),
                shared_memory.as_ref(),
                wal_prefix_digest.as_ref(),
            );
            return Ok(RootHandleSqliteFamilyEvidence {
                parent,
                database,
                wal,
                shared_memory,
                wal_prefix_digest,
                revision,
            });
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(unsupported())
        }
    }

    /// Revalidates retained handles and their current authority-relative names.
    ///
    /// WAL append and SHM coordination updates are allowed. Existing WAL bytes
    /// through the pinned prefix must remain identical.
    pub(crate) fn revalidate(
        &self,
        evidence: &RootHandleSqliteFamilyEvidence,
    ) -> RootHandleSqliteVfsResult<()> {
        #[cfg(target_os = "linux")]
        {
            let parent = FileStamp::read(&self.parent, RootHandleSqliteComponent::Database)?;
            if parent != evidence.parent || parent != self.parent_opened {
                return Err(changed(RootHandleSqliteComponent::Database));
            }
            let database = FileStamp::read(&self.database, RootHandleSqliteComponent::Database)?;
            if database != evidence.database {
                return Err(changed(RootHandleSqliteComponent::Database));
            }
            revalidate_named(
                &self.parent,
                &self.database_name,
                Some(&evidence.database),
                RootHandleSqliteComponent::Database,
            )?;

            let wal_name = with_suffix(&self.database_name, "-wal");
            let current_wal = self
                .wal
                .as_ref()
                .map(|file| FileStamp::read(file, RootHandleSqliteComponent::Wal))
                .transpose()?;
            match (current_wal.as_ref(), evidence.wal.as_ref()) {
                (Some(current), Some(expected))
                    if current.identity_eq(expected) && current.length >= expected.length =>
                {
                    let digest =
                        hash_prefix(self.wal.as_ref().expect("matched WAL"), expected.length)?;
                    if Some(&digest) != evidence.wal_prefix_digest.as_ref() {
                        return Err(changed(RootHandleSqliteComponent::Wal));
                    }
                }
                (None, None) => {}
                _ => return Err(changed(RootHandleSqliteComponent::Wal)),
            }
            revalidate_named(
                &self.parent,
                &wal_name,
                evidence.wal.as_ref(),
                RootHandleSqliteComponent::Wal,
            )?;

            let shm_name = with_suffix(&self.database_name, "-shm");
            let current_shm = self
                .shared_memory
                .as_ref()
                .map(|file| FileStamp::read(file, RootHandleSqliteComponent::SharedMemory))
                .transpose()?;
            match (current_shm.as_ref(), evidence.shared_memory.as_ref()) {
                (Some(current), Some(expected))
                    if current.identity_eq(expected) && current.length >= expected.length => {}
                (None, None) => {}
                _ => return Err(changed(RootHandleSqliteComponent::SharedMemory)),
            }
            revalidate_named(
                &self.parent,
                &shm_name,
                evidence.shared_memory.as_ref(),
                RootHandleSqliteComponent::SharedMemory,
            )?;

            let journal_name = with_suffix(&self.database_name, "-journal");
            revalidate_named(
                &self.parent,
                &journal_name,
                None,
                RootHandleSqliteComponent::RollbackJournal,
            )
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = evidence;
            Err(unsupported())
        }
    }
}

#[cfg(target_os = "linux")]
fn changed(component: RootHandleSqliteComponent) -> RootHandleSqliteVfsError {
    RootHandleSqliteVfsError::SourceChanged { component }
}

#[cfg(not(target_os = "linux"))]
fn unsupported() -> RootHandleSqliteVfsError {
    RootHandleSqliteVfsError::Unsupported {
        platform: std::env::consts::OS,
        capability: "the root-handle VFS currently requires Linux procfd, mmap, and OFD locks",
    }
}

#[cfg(target_os = "linux")]
fn validate_leaf(name: &OsStr) -> RootHandleSqliteVfsResult<()> {
    let path = Path::new(name);
    if name.is_empty()
        || path.components().count() != 1
        || !matches!(path.components().next(), Some(Component::Normal(_)))
    {
        return Err(RootHandleSqliteVfsError::UnsafeSource {
            reason: "the SQLite database name must be one normal leaf component",
        });
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn with_suffix(name: &OsStr, suffix: &str) -> OsString {
    let mut value = name.to_os_string();
    value.push(suffix);
    value
}

#[cfg(target_os = "linux")]
fn open_required_at(
    parent: &File,
    name: &OsStr,
    component: RootHandleSqliteComponent,
) -> RootHandleSqliteVfsResult<File> {
    open_optional_at(parent, name, component)?.ok_or_else(|| RootHandleSqliteVfsError::Io {
        component,
        operation: "opening required source component",
        source: io::Error::from(io::ErrorKind::NotFound),
    })
}

#[cfg(target_os = "linux")]
fn open_optional_at(
    parent: &File,
    name: &OsStr,
    component: RootHandleSqliteComponent,
) -> RootHandleSqliteVfsResult<Option<File>> {
    open_optional_at_mode(parent, name, component, false)
}

#[cfg(target_os = "linux")]
fn open_optional_at_mode(
    parent: &File,
    name: &OsStr,
    component: RootHandleSqliteComponent,
    read_write_for_locks: bool,
) -> RootHandleSqliteVfsResult<Option<File>> {
    let name =
        CString::new(name.as_bytes()).map_err(|_| RootHandleSqliteVfsError::UnsafeSource {
            reason: "SQLite source names may not contain NUL bytes",
        })?;
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            (if read_write_for_locks {
                libc::O_RDWR
            } else {
                libc::O_RDONLY
            }) | libc::O_CLOEXEC
                | libc::O_NOFOLLOW
                | libc::O_NONBLOCK,
        )
    };
    if descriptor < 0 {
        let source = io::Error::last_os_error();
        if source.kind() == io::ErrorKind::NotFound {
            return Ok(None);
        }
        if source.raw_os_error() == Some(libc::ELOOP) {
            return Err(RootHandleSqliteVfsError::UnsafeSource {
                reason: "SQLite source family components may not be symlinks",
            });
        }
        return Err(RootHandleSqliteVfsError::Io {
            component,
            operation: "opening source component relative to the authorized parent",
            source,
        });
    }
    Ok(Some(unsafe { File::from_raw_fd(descriptor) }))
}

#[cfg(target_os = "linux")]
fn ensure_regular(
    file: &File,
    component: RootHandleSqliteComponent,
) -> RootHandleSqliteVfsResult<()> {
    let metadata = file
        .metadata()
        .map_err(|source| RootHandleSqliteVfsError::Io {
            component,
            operation: "checking source component type",
            source,
        })?;
    if metadata.is_file() {
        Ok(())
    } else {
        Err(RootHandleSqliteVfsError::UnsafeSource {
            reason: "SQLite source family components must be regular files",
        })
    }
}

#[cfg(target_os = "linux")]
fn ensure_same_filesystem(left: &File, right: &File) -> RootHandleSqliteVfsResult<()> {
    let left_mount = mount_id(left)?;
    let right_mount = mount_id(right)?;
    let left_stamp = FileStamp::read(left, RootHandleSqliteComponent::Database)?;
    let right_stamp = FileStamp::read(right, RootHandleSqliteComponent::Database)?;
    if left_stamp.device == right_stamp.device && left_mount == right_mount {
        Ok(())
    } else {
        Err(RootHandleSqliteVfsError::UnsafeSource {
            reason: "SQLite source family components may not cross filesystems",
        })
    }
}

#[cfg(target_os = "linux")]
fn ensure_supported_filesystem(file: &File) -> RootHandleSqliteVfsResult<()> {
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    if unsafe { libc::fstatfs(file.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(RootHandleSqliteVfsError::Io {
            component: RootHandleSqliteComponent::Database,
            operation: "identifying the source filesystem",
            source: io::Error::last_os_error(),
        });
    }
    let filesystem_type = unsafe { stat.assume_init() }.f_type as u64;
    match filesystem_type {
        0xEF53 | 0x5846_5342 | 0x9123_683E | 0xF2F5_2010 | 0x2FC1_2FC1 | 0x0102_1994
        | 0x8584_58F6 | 0x794C_7630 | 0x2405_1905 | 0x3153_464A => Ok(()),
        _ => Err(RootHandleSqliteVfsError::UnsafeSource {
            reason: "the SQLite source filesystem lacks qualified local lock semantics",
        }),
    }
}

#[cfg(target_os = "linux")]
fn mount_id(file: &File) -> RootHandleSqliteVfsResult<u64> {
    mount_id_descriptor(file.as_raw_fd())
}

#[cfg(target_os = "linux")]
fn mount_id_descriptor(descriptor: RawFd) -> RootHandleSqliteVfsResult<u64> {
    let info =
        std::fs::read_to_string(format!("/proc/self/fdinfo/{descriptor}")).map_err(|source| {
            RootHandleSqliteVfsError::Io {
                component: RootHandleSqliteComponent::Database,
                operation: "reading descriptor mount identity",
                source,
            }
        })?;
    info.lines()
        .find_map(|line| line.strip_prefix("mnt_id:").map(str::trim))
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or(RootHandleSqliteVfsError::UnsafeSource {
            reason: "the retained SQLite source mount identity could not be established",
        })
}

#[cfg(target_os = "linux")]
fn reopen_file_description(file: &File, access_mode: c_int) -> io::Result<File> {
    let path = CString::new(format!("/proc/self/fd/{}", file.as_raw_fd()))
        .expect("numeric procfd paths contain no NUL");
    let descriptor = unsafe { libc::open(path.as_ptr(), access_mode | libc::O_CLOEXEC) };
    if descriptor < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }
}

#[cfg(target_os = "linux")]
fn reopen_admitted_file(
    file: &File,
    access_mode: c_int,
    component: RootHandleSqliteComponent,
) -> RootHandleSqliteVfsResult<File> {
    // A fresh open-file description is required because OFD locks belong to
    // the description, not the descriptor. Reopen only the retained procfd,
    // then prove that Linux returned the admitted object before SQLite can see
    // the VFS.
    let reopened = reopen_file_description(file, access_mode).map_err(|source| {
        RootHandleSqliteVfsError::Io {
            component,
            operation: "reopening the admitted component through its retained descriptor",
            source,
        }
    })?;
    let admitted_stamp = FileStamp::read(file, component)?;
    let reopened_stamp = FileStamp::read(&reopened, component)?;
    if !admitted_stamp.identity_eq(&reopened_stamp) || mount_id(file)? != mount_id(&reopened)? {
        return Err(changed(component));
    }
    Ok(reopened)
}

#[cfg(target_os = "linux")]
fn revalidate_named(
    parent: &File,
    name: &OsStr,
    expected: Option<&FileStamp>,
    component: RootHandleSqliteComponent,
) -> RootHandleSqliteVfsResult<()> {
    let current = open_optional_at(parent, name, component)?;
    let current = current
        .as_ref()
        .map(|file| FileStamp::read(file, component))
        .transpose()?;
    match (current.as_ref(), expected) {
        (None, None) => Ok(()),
        (Some(current), Some(expected)) if current.identity_eq(expected) => Ok(()),
        _ => Err(changed(component)),
    }
}

#[cfg(target_os = "linux")]
fn hash_prefix(file: &File, length: u64) -> RootHandleSqliteVfsResult<[u8; 32]> {
    use std::os::unix::fs::FileExt;

    let mut digest = Sha256::new();
    digest.update(EVIDENCE_DOMAIN);
    digest.update(b"wal-prefix\0");
    digest.update(length.to_le_bytes());
    let mut offset = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    while offset < length {
        let remaining = length - offset;
        let wanted = usize::try_from(remaining.min(buffer.len() as u64))
            .expect("bounded by the fixed buffer length");
        let mut filled = 0;
        while filled < wanted {
            let read = file
                .read_at(&mut buffer[filled..wanted], offset + filled as u64)
                .map_err(|source| RootHandleSqliteVfsError::Io {
                    component: RootHandleSqliteComponent::Wal,
                    operation: "hashing the pinned WAL prefix",
                    source,
                })?;
            if read == 0 {
                return Err(changed(RootHandleSqliteComponent::Wal));
            }
            filled += read;
        }
        digest.update(&buffer[..wanted]);
        offset += wanted as u64;
    }
    Ok(digest.finalize().into())
}

#[cfg(target_os = "linux")]
fn family_revision(
    parent: &FileStamp,
    database: &FileStamp,
    wal: Option<&FileStamp>,
    shared_memory: Option<&FileStamp>,
    wal_prefix_digest: Option<&[u8; 32]>,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(EVIDENCE_DOMAIN);
    parent.hash_into(&mut digest);
    database.hash_into(&mut digest);
    for component in [wal, shared_memory] {
        match component {
            Some(stamp) => {
                digest.update(b"present\0");
                stamp.hash_into(&mut digest);
            }
            None => digest.update(b"absent\0"),
        }
    }
    if let Some(prefix) = wal_prefix_digest {
        digest.update(prefix);
    }
    digest.finalize().into()
}

/// Registered per-snapshot VFS. It must outlive the SQLite connection.
pub(crate) struct RootHandleSqliteVfs {
    #[cfg(target_os = "linux")]
    registration: Box<VfsRegistration>,
}

impl std::fmt::Debug for RootHandleSqliteVfs {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RootHandleSqliteVfs")
            .field("name", &self.name())
            .finish()
    }
}

impl RootHandleSqliteVfs {
    fn register(source: &RootHandleSqliteSource) -> RootHandleSqliteVfsResult<Self> {
        #[cfg(target_os = "linux")]
        {
            let base_vfs = unsafe { ffi::sqlite3_vfs_find(c"unix".as_ptr()) };
            if base_vfs.is_null() {
                return Err(RootHandleSqliteVfsError::Unsupported {
                    platform: "linux",
                    capability: "bundled SQLite did not register the unix VFS",
                });
            }
            let id = NEXT_VFS_ID.fetch_add(1, Ordering::Relaxed);
            let name = CString::new(format!("ctx-root-handle-{id}"))
                .expect("generated VFS names contain no NUL");
            let virtual_path = CString::new(format!("/ctx-root-handle/{id}/database.sqlite"))
                .expect("generated virtual paths contain no NUL");
            let context = Box::new(VfsContext {
                base_vfs,
                virtual_path: virtual_path.clone(),
                database: reopen_admitted_file(
                    &source.database,
                    libc::O_RDONLY,
                    RootHandleSqliteComponent::Database,
                )?,
                wal: source
                    .wal
                    .as_ref()
                    .map(|file| {
                        reopen_admitted_file(file, libc::O_RDONLY, RootHandleSqliteComponent::Wal)
                    })
                    .transpose()?,
                shared_memory: source
                    .shared_memory
                    .as_ref()
                    .map(|file| {
                        reopen_admitted_file(
                            file,
                            libc::O_RDWR,
                            RootHandleSqliteComponent::SharedMemory,
                        )
                    })
                    .transpose()?,
            });
            let total_size = c_int::try_from(size_of::<WrappedSqliteFile>()).map_err(|_| {
                RootHandleSqliteVfsError::Unsupported {
                    platform: "linux",
                    capability: "the root-handle sqlite3_file size overflowed",
                }
            })?;
            let mut registration = Box::new(VfsRegistration {
                context,
                name,
                virtual_path,
                vfs: ffi::sqlite3_vfs {
                    iVersion: 1,
                    szOsFile: total_size,
                    mxPathname: 1024,
                    pNext: ptr::null_mut(),
                    zName: ptr::null(),
                    pAppData: ptr::null_mut(),
                    xOpen: Some(vfs_open),
                    xDelete: Some(vfs_delete),
                    xAccess: Some(vfs_access),
                    xFullPathname: Some(vfs_full_pathname),
                    xDlOpen: Some(vfs_dl_open),
                    xDlError: Some(vfs_dl_error),
                    xDlSym: Some(vfs_dl_sym),
                    xDlClose: Some(vfs_dl_close),
                    xRandomness: Some(vfs_randomness),
                    xSleep: Some(vfs_sleep),
                    xCurrentTime: Some(vfs_current_time),
                    xGetLastError: Some(vfs_get_last_error),
                    xCurrentTimeInt64: None,
                    xSetSystemCall: None,
                    xGetSystemCall: None,
                    xNextSystemCall: None,
                },
                registered: false,
            });
            registration.vfs.zName = registration.name.as_ptr();
            registration.vfs.pAppData = (&*registration.context as *const VfsContext)
                .cast_mut()
                .cast();
            let code = unsafe { ffi::sqlite3_vfs_register(&mut registration.vfs, 0) };
            if code != ffi::SQLITE_OK {
                return Err(RootHandleSqliteVfsError::Registration { code });
            }
            registration.registered = true;
            return Ok(Self { registration });
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = source;
            Err(unsupported())
        }
    }

    pub(crate) fn name(&self) -> &str {
        #[cfg(target_os = "linux")]
        {
            self.registration
                .name
                .to_str()
                .expect("generated VFS names are UTF-8")
        }
        #[cfg(not(target_os = "linux"))]
        {
            ""
        }
    }

    pub(crate) fn virtual_path(&self) -> &Path {
        #[cfg(target_os = "linux")]
        {
            Path::new(OsStr::from_bytes(self.registration.virtual_path.as_bytes()))
        }
        #[cfg(not(target_os = "linux"))]
        {
            Path::new("")
        }
    }

    /// Proves that SQLite's actual `main` file is this VFS's admitted database.
    ///
    /// Call this immediately after `sqlite3_open_v2` and before any provider
    /// query. The methods/context checks prevent interpreting a foreign VFS
    /// object as our wrapper; descriptor identity and mount identity then bind
    /// the live connection back to the retained authority handle.
    #[cfg(target_os = "linux")]
    pub(crate) fn connection_main_matches(
        &self,
        sqlite_file: *mut ffi::sqlite3_file,
        source: &RootHandleSqliteSource,
    ) -> RootHandleSqliteVfsResult<bool> {
        if sqlite_file.is_null() || unsafe { (*sqlite_file).pMethods } != &ROOT_HANDLE_IO_METHODS {
            return Ok(false);
        }
        let wrapper = unsafe { &*sqlite_file.cast::<WrappedSqliteFile>() };
        let expected_context = &*self.registration.context as *const VfsContext;
        if wrapper.context != expected_context
            || wrapper.kind != ffi::SQLITE_OPEN_MAIN_DB
            || wrapper.descriptor < 0
        {
            return Ok(false);
        }
        let actual =
            FileStamp::read_descriptor(wrapper.descriptor, RootHandleSqliteComponent::Database)?;
        let admitted = FileStamp::read(&source.database, RootHandleSqliteComponent::Database)?;
        Ok(actual.identity_eq(&admitted)
            && mount_id_descriptor(wrapper.descriptor)? == mount_id(&source.database)?)
    }
}

#[cfg(target_os = "linux")]
struct VfsContext {
    base_vfs: *mut ffi::sqlite3_vfs,
    virtual_path: CString,
    database: File,
    wal: Option<File>,
    shared_memory: Option<File>,
}

#[cfg(target_os = "linux")]
struct VfsRegistration {
    context: Box<VfsContext>,
    name: CString,
    virtual_path: CString,
    vfs: ffi::sqlite3_vfs,
    registered: bool,
}

#[cfg(target_os = "linux")]
impl Drop for VfsRegistration {
    fn drop(&mut self) {
        if self.registered {
            let code = unsafe { ffi::sqlite3_vfs_unregister(&mut self.vfs) };
            debug_assert_eq!(code, ffi::SQLITE_OK);
            self.registered = false;
        }
    }
}

#[cfg(target_os = "linux")]
#[repr(C)]
struct WrappedSqliteFile {
    base: ffi::sqlite3_file,
    context: *const VfsContext,
    descriptor: RawFd,
    kind: c_int,
    lock_level: c_int,
    shm_state: *mut ShmState,
}

#[cfg(target_os = "linux")]
struct ShmRegion {
    address: *mut c_void,
    length: usize,
}

#[cfg(target_os = "linux")]
struct ShmState {
    regions: Vec<Option<ShmRegion>>,
    shared_mask: u16,
    exclusive_mask: u16,
    dms_locked: bool,
}

#[cfg(target_os = "linux")]
unsafe fn wrapped<'a>(file: *mut ffi::sqlite3_file) -> &'a mut WrappedSqliteFile {
    unsafe { &mut *file.cast::<WrappedSqliteFile>() }
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn vfs_open(
    vfs: *mut ffi::sqlite3_vfs,
    name: ffi::sqlite3_filename,
    file: *mut ffi::sqlite3_file,
    flags: c_int,
    out_flags: *mut c_int,
) -> c_int {
    if vfs.is_null() || file.is_null() {
        return ffi::SQLITE_CANTOPEN;
    }
    unsafe {
        (*file).pMethods = ptr::null();
    }
    if flags & ffi::SQLITE_OPEN_DELETEONCLOSE != 0 {
        return ffi::SQLITE_READONLY;
    }
    let context = unsafe { &*((*vfs).pAppData.cast::<VfsContext>()) };
    let actual_name = if name.is_null() {
        &[][..]
    } else {
        unsafe { CStr::from_ptr(name) }.to_bytes()
    };
    let main_name = context.virtual_path.as_bytes();
    let (source, kind) = if flags & ffi::SQLITE_OPEN_MAIN_DB != 0 {
        if actual_name != main_name {
            return ffi::SQLITE_CANTOPEN;
        }
        (&context.database, ffi::SQLITE_OPEN_MAIN_DB)
    } else if flags & ffi::SQLITE_OPEN_WAL != 0 {
        if actual_name.len() != main_name.len() + 4
            || !actual_name.starts_with(main_name)
            || !actual_name.ends_with(b"-wal")
        {
            return ffi::SQLITE_CANTOPEN;
        }
        let Some(wal) = context.wal.as_ref() else {
            return ffi::SQLITE_CANTOPEN;
        };
        (wal, ffi::SQLITE_OPEN_WAL)
    } else if flags & ffi::SQLITE_OPEN_MAIN_JOURNAL != 0 {
        return ffi::SQLITE_READONLY;
    } else {
        return ffi::SQLITE_CANTOPEN;
    };
    let descriptor = unsafe { libc::fcntl(source.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
    if descriptor < 0 {
        return ffi::SQLITE_CANTOPEN;
    }
    unsafe { ptr::write_bytes(file.cast::<u8>(), 0, (*vfs).szOsFile as usize) };
    let wrapper = file.cast::<WrappedSqliteFile>();
    unsafe {
        (*wrapper).base.pMethods = &ROOT_HANDLE_IO_METHODS;
        (*wrapper).context = context;
        (*wrapper).descriptor = descriptor;
        (*wrapper).kind = kind;
        (*wrapper).lock_level = ffi::SQLITE_LOCK_NONE;
        (*wrapper).shm_state = ptr::null_mut();
        if !out_flags.is_null() {
            *out_flags = (flags & !(ffi::SQLITE_OPEN_READWRITE | ffi::SQLITE_OPEN_CREATE))
                | ffi::SQLITE_OPEN_READONLY;
        }
    }
    ffi::SQLITE_OK
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn vfs_delete(
    _vfs: *mut ffi::sqlite3_vfs,
    _name: *const c_char,
    _sync_dir: c_int,
) -> c_int {
    ffi::SQLITE_READONLY
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn vfs_access(
    vfs: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    flags: c_int,
    result: *mut c_int,
) -> c_int {
    if vfs.is_null() || result.is_null() {
        return ffi::SQLITE_IOERR;
    }
    if !matches!(
        flags,
        ffi::SQLITE_ACCESS_EXISTS | ffi::SQLITE_ACCESS_READWRITE | ffi::SQLITE_ACCESS_READ
    ) {
        return ffi::SQLITE_IOERR_ACCESS;
    }
    let context = unsafe { &*((*vfs).pAppData.cast::<VfsContext>()) };
    let bytes = if name.is_null() {
        &[][..]
    } else {
        unsafe { CStr::from_ptr(name) }.to_bytes()
    };
    let main_name = context.virtual_path.as_bytes();
    let present = if bytes.len() == main_name.len() + 4
        && bytes.starts_with(main_name)
        && bytes.ends_with(b"-wal")
    {
        context.wal.is_some()
    } else if bytes.len() == main_name.len() + 8
        && bytes.starts_with(main_name)
        && bytes.ends_with(b"-journal")
    {
        false
    } else if bytes.len() == main_name.len() + 4
        && bytes.starts_with(main_name)
        && bytes.ends_with(b"-shm")
    {
        context.shared_memory.is_some()
    } else {
        bytes == main_name
    };
    unsafe {
        *result = if flags == ffi::SQLITE_ACCESS_READWRITE {
            0
        } else {
            c_int::from(present)
        };
    }
    ffi::SQLITE_OK
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn vfs_full_pathname(
    _vfs: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    output_length: c_int,
    output: *mut c_char,
) -> c_int {
    if name.is_null() || output.is_null() || output_length <= 0 {
        return ffi::SQLITE_CANTOPEN;
    }
    let source = unsafe { CStr::from_ptr(name) }.to_bytes_with_nul();
    if source.len() > output_length as usize {
        return ffi::SQLITE_CANTOPEN;
    }
    unsafe {
        ptr::copy_nonoverlapping(source.as_ptr().cast(), output, source.len());
    }
    ffi::SQLITE_OK
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn vfs_dl_open(_vfs: *mut ffi::sqlite3_vfs, _name: *const c_char) -> *mut c_void {
    ptr::null_mut()
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn vfs_dl_error(
    _vfs: *mut ffi::sqlite3_vfs,
    length: c_int,
    message: *mut c_char,
) {
    if !message.is_null() && length > 0 {
        unsafe {
            *message = 0;
        }
    }
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn vfs_dl_sym(
    _vfs: *mut ffi::sqlite3_vfs,
    _handle: *mut c_void,
    _symbol: *const c_char,
) -> Option<unsafe extern "C" fn(*mut ffi::sqlite3_vfs, *mut c_void, *const c_char)> {
    None
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn vfs_dl_close(_vfs: *mut ffi::sqlite3_vfs, _handle: *mut c_void) {}

#[cfg(target_os = "linux")]
unsafe extern "C" fn vfs_randomness(
    vfs: *mut ffi::sqlite3_vfs,
    length: c_int,
    output: *mut c_char,
) -> c_int {
    let context = unsafe { &*((*vfs).pAppData.cast::<VfsContext>()) };
    match unsafe { (*context.base_vfs).xRandomness } {
        Some(callback) => unsafe { callback(context.base_vfs, length, output) },
        None => 0,
    }
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn vfs_sleep(vfs: *mut ffi::sqlite3_vfs, micros: c_int) -> c_int {
    let context = unsafe { &*((*vfs).pAppData.cast::<VfsContext>()) };
    match unsafe { (*context.base_vfs).xSleep } {
        Some(callback) => unsafe { callback(context.base_vfs, micros) },
        None => micros,
    }
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn vfs_current_time(vfs: *mut ffi::sqlite3_vfs, value: *mut f64) -> c_int {
    let context = unsafe { &*((*vfs).pAppData.cast::<VfsContext>()) };
    match unsafe { (*context.base_vfs).xCurrentTime } {
        Some(callback) => unsafe { callback(context.base_vfs, value) },
        None => ffi::SQLITE_IOERR,
    }
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn vfs_get_last_error(
    vfs: *mut ffi::sqlite3_vfs,
    length: c_int,
    message: *mut c_char,
) -> c_int {
    let context = unsafe { &*((*vfs).pAppData.cast::<VfsContext>()) };
    match unsafe { (*context.base_vfs).xGetLastError } {
        Some(callback) => unsafe { callback(context.base_vfs, length, message) },
        None => 0,
    }
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn io_close(file: *mut ffi::sqlite3_file) -> c_int {
    let _ = unsafe { io_shm_unmap(file, 0) };
    let wrapper = unsafe { wrapped(file) };
    let _ = unlock_main(wrapper);
    let code = if wrapper.descriptor >= 0 && unsafe { libc::close(wrapper.descriptor) } != 0 {
        ffi::SQLITE_IOERR_CLOSE
    } else {
        ffi::SQLITE_OK
    };
    wrapper.base.pMethods = ptr::null();
    wrapper.descriptor = -1;
    code
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn io_read(
    file: *mut ffi::sqlite3_file,
    output: *mut c_void,
    amount: c_int,
    offset: ffi::sqlite3_int64,
) -> c_int {
    let wrapper = unsafe { wrapped(file) };
    if output.is_null() || amount < 0 || offset < 0 || wrapper.descriptor < 0 {
        return ffi::SQLITE_IOERR_READ;
    }
    let mut filled = 0_usize;
    while filled < amount as usize {
        let Some(read_offset) = offset.checked_add(filled as ffi::sqlite3_int64) else {
            return ffi::SQLITE_IOERR_READ;
        };
        let read = unsafe {
            libc::pread(
                wrapper.descriptor,
                output.cast::<u8>().add(filled).cast(),
                amount as usize - filled,
                read_offset as libc::off_t,
            )
        };
        if read < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return ffi::SQLITE_IOERR_READ;
        }
        if read == 0 {
            unsafe {
                ptr::write_bytes(output.cast::<u8>().add(filled), 0, amount as usize - filled);
            }
            return ffi::SQLITE_IOERR_SHORT_READ;
        }
        filled += read as usize;
    }
    ffi::SQLITE_OK
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn io_write(
    _file: *mut ffi::sqlite3_file,
    _input: *const c_void,
    _amount: c_int,
    _offset: ffi::sqlite3_int64,
) -> c_int {
    ffi::SQLITE_READONLY
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn io_truncate(
    _file: *mut ffi::sqlite3_file,
    _size: ffi::sqlite3_int64,
) -> c_int {
    ffi::SQLITE_READONLY
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn io_sync(_file: *mut ffi::sqlite3_file, _flags: c_int) -> c_int {
    ffi::SQLITE_READONLY
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn io_file_size(
    file: *mut ffi::sqlite3_file,
    size: *mut ffi::sqlite3_int64,
) -> c_int {
    let wrapper = unsafe { wrapped(file) };
    if size.is_null() || wrapper.descriptor < 0 {
        return ffi::SQLITE_IOERR_FSTAT;
    }
    let mut stat = std::mem::MaybeUninit::<libc::stat>::zeroed();
    if unsafe { libc::fstat(wrapper.descriptor, stat.as_mut_ptr()) } != 0 {
        return ffi::SQLITE_IOERR_FSTAT;
    }
    unsafe {
        *size = stat.assume_init().st_size as ffi::sqlite3_int64;
    }
    ffi::SQLITE_OK
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn io_lock(file: *mut ffi::sqlite3_file, level: c_int) -> c_int {
    let wrapper = unsafe { wrapped(file) };
    if wrapper.kind != ffi::SQLITE_OPEN_MAIN_DB {
        return ffi::SQLITE_OK;
    }
    if level <= wrapper.lock_level {
        return ffi::SQLITE_OK;
    }
    if level != ffi::SQLITE_LOCK_SHARED || wrapper.lock_level != ffi::SQLITE_LOCK_NONE {
        return ffi::SQLITE_BUSY;
    }
    let pending = set_descriptor_ofd_lock(
        wrapper.descriptor,
        libc::F_RDLCK as i16,
        PENDING_BYTE,
        1,
        ffi::SQLITE_IOERR_LOCK,
    );
    if pending != ffi::SQLITE_OK {
        return pending;
    }
    let shared = set_descriptor_ofd_lock(
        wrapper.descriptor,
        libc::F_RDLCK as i16,
        SHARED_FIRST,
        SHARED_SIZE,
        ffi::SQLITE_IOERR_LOCK,
    );
    let _ = set_descriptor_ofd_lock(
        wrapper.descriptor,
        libc::F_UNLCK as i16,
        PENDING_BYTE,
        1,
        ffi::SQLITE_IOERR_UNLOCK,
    );
    if shared == ffi::SQLITE_OK {
        wrapper.lock_level = ffi::SQLITE_LOCK_SHARED;
    }
    shared
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn io_unlock(file: *mut ffi::sqlite3_file, level: c_int) -> c_int {
    let wrapper = unsafe { wrapped(file) };
    if wrapper.kind != ffi::SQLITE_OPEN_MAIN_DB {
        return ffi::SQLITE_OK;
    }
    if level == ffi::SQLITE_LOCK_NONE {
        unlock_main(wrapper)
    } else if level == ffi::SQLITE_LOCK_SHARED && wrapper.lock_level == ffi::SQLITE_LOCK_SHARED {
        ffi::SQLITE_OK
    } else {
        ffi::SQLITE_IOERR_UNLOCK
    }
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn io_check_reserved(file: *mut ffi::sqlite3_file, result: *mut c_int) -> c_int {
    let wrapper = unsafe { wrapped(file) };
    if result.is_null() || wrapper.descriptor < 0 {
        return ffi::SQLITE_IOERR_CHECKRESERVEDLOCK;
    }
    let mut lock = libc::flock {
        l_type: libc::F_WRLCK as i16,
        l_whence: libc::SEEK_SET as i16,
        l_start: RESERVED_BYTE,
        l_len: 1,
        l_pid: 0,
    };
    if unsafe { libc::fcntl(wrapper.descriptor, libc::F_OFD_GETLK, &mut lock) } != 0 {
        return ffi::SQLITE_IOERR_CHECKRESERVEDLOCK;
    }
    unsafe {
        *result = c_int::from(lock.l_type != libc::F_UNLCK as i16);
    }
    ffi::SQLITE_OK
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn io_file_control(
    file: *mut ffi::sqlite3_file,
    operation: c_int,
    argument: *mut c_void,
) -> c_int {
    let wrapper = unsafe { wrapped(file) };
    match operation {
        ffi::SQLITE_FCNTL_LOCKSTATE if !argument.is_null() => {
            unsafe {
                *argument.cast::<c_int>() = wrapper.lock_level;
            }
            ffi::SQLITE_OK
        }
        ffi::SQLITE_FCNTL_HAS_MOVED if !argument.is_null() => {
            unsafe {
                *argument.cast::<c_int>() = 0;
            }
            ffi::SQLITE_OK
        }
        _ => ffi::SQLITE_NOTFOUND,
    }
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn io_sector_size(file: *mut ffi::sqlite3_file) -> c_int {
    let _ = file;
    4096
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn io_device_characteristics(file: *mut ffi::sqlite3_file) -> c_int {
    let _ = file;
    0
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn io_shm_map(
    file: *mut ffi::sqlite3_file,
    region_index: c_int,
    region_size: c_int,
    _extend: c_int,
    output: *mut *mut c_void,
) -> c_int {
    if output.is_null() {
        return ffi::SQLITE_IOERR_SHMMAP;
    }
    unsafe {
        *output = ptr::null_mut();
    }
    if region_index < 0 || region_size <= 0 {
        return ffi::SQLITE_IOERR_SHMMAP;
    }
    let wrapper = unsafe { wrapped(file) };
    if wrapper.kind != ffi::SQLITE_OPEN_MAIN_DB || wrapper.context.is_null() {
        return ffi::SQLITE_IOERR_SHMMAP;
    }
    let context = unsafe { &*wrapper.context };
    let Some(shm) = context.shared_memory.as_ref() else {
        return ffi::SQLITE_IOERR_SHMOPEN;
    };
    if wrapper.shm_state.is_null() {
        let dms = set_ofd_lock(shm, libc::F_RDLCK as i16, UNIX_SHM_DMS, 1);
        if dms != ffi::SQLITE_OK {
            return dms;
        }
        let state = Box::new(ShmState {
            regions: Vec::new(),
            shared_mask: 0,
            exclusive_mask: 0,
            dms_locked: true,
        });
        wrapper.shm_state = Box::into_raw(state);
    }
    let state = unsafe { &mut *wrapper.shm_state };
    let index = region_index as usize;
    if state.regions.len() <= index {
        state.regions.resize_with(index + 1, || None);
    }
    if state.regions[index].is_none() {
        let offset = region_index as i64 * region_size as i64;
        let end = match offset.checked_add(region_size as i64) {
            Some(value) => value,
            None => return ffi::SQLITE_IOERR_SHMMAP,
        };
        let length = match shm.metadata() {
            Ok(metadata) => metadata.len(),
            Err(_) => return ffi::SQLITE_IOERR_SHMSIZE,
        };
        if offset < 0 || end < 0 || end as u64 > length {
            return ffi::SQLITE_READONLY;
        }
        let address = unsafe {
            libc::mmap(
                ptr::null_mut(),
                region_size as usize,
                libc::PROT_READ,
                libc::MAP_SHARED,
                shm.as_raw_fd(),
                offset as libc::off_t,
            )
        };
        if address == libc::MAP_FAILED {
            return ffi::SQLITE_IOERR_SHMMAP;
        }
        state.regions[index] = Some(ShmRegion {
            address,
            length: region_size as usize,
        });
    }
    unsafe {
        *output = state.regions[index]
            .as_ref()
            .expect("mapped region")
            .address;
    }
    ffi::SQLITE_READONLY
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn io_shm_lock(
    file: *mut ffi::sqlite3_file,
    offset: c_int,
    count: c_int,
    flags: c_int,
) -> c_int {
    if !matches!(
        flags,
        value if value == ffi::SQLITE_SHM_LOCK | ffi::SQLITE_SHM_SHARED
            || value == ffi::SQLITE_SHM_LOCK | ffi::SQLITE_SHM_EXCLUSIVE
            || value == ffi::SQLITE_SHM_UNLOCK | ffi::SQLITE_SHM_SHARED
            || value == ffi::SQLITE_SHM_UNLOCK | ffi::SQLITE_SHM_EXCLUSIVE
    ) {
        return ffi::SQLITE_IOERR_SHMLOCK;
    }
    let range_end = offset.checked_add(count);
    if offset < 0
        || count <= 0
        || !matches!(range_end, Some(end) if end <= ffi::SQLITE_SHM_NLOCK)
        || (count != 1 && flags & ffi::SQLITE_SHM_SHARED != 0)
    {
        return ffi::SQLITE_IOERR_SHMLOCK;
    }
    let wrapper = unsafe { wrapped(file) };
    if wrapper.shm_state.is_null() || wrapper.context.is_null() {
        return ffi::SQLITE_IOERR_SHMLOCK;
    }
    let context = unsafe { &*wrapper.context };
    let Some(shm) = context.shared_memory.as_ref() else {
        return ffi::SQLITE_IOERR_SHMLOCK;
    };
    let state = unsafe { &mut *wrapper.shm_state };
    let mask = (((1_u32 << count) - 1) << offset) as u16;
    let lock_type = if flags & ffi::SQLITE_SHM_UNLOCK != 0 {
        libc::F_UNLCK as i16
    } else if flags & ffi::SQLITE_SHM_SHARED != 0 {
        libc::F_RDLCK as i16
    } else if flags & ffi::SQLITE_SHM_EXCLUSIVE != 0 {
        libc::F_WRLCK as i16
    } else {
        return ffi::SQLITE_IOERR_SHMLOCK;
    };
    let code = set_ofd_lock(
        shm,
        lock_type,
        UNIX_SHM_BASE + offset as libc::off_t,
        count as libc::off_t,
    );
    if code != ffi::SQLITE_OK {
        return code;
    }
    if flags & ffi::SQLITE_SHM_UNLOCK != 0 {
        state.shared_mask &= !mask;
        state.exclusive_mask &= !mask;
    } else if flags & ffi::SQLITE_SHM_SHARED != 0 {
        state.shared_mask |= mask;
    } else {
        state.exclusive_mask |= mask;
    }
    ffi::SQLITE_OK
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn io_shm_barrier(_file: *mut ffi::sqlite3_file) {
    fence(Ordering::SeqCst);
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn io_shm_unmap(file: *mut ffi::sqlite3_file, _delete: c_int) -> c_int {
    if file.is_null() {
        return ffi::SQLITE_OK;
    }
    let wrapper = unsafe { wrapped(file) };
    if wrapper.shm_state.is_null() {
        return ffi::SQLITE_OK;
    }
    let mut state = unsafe { Box::from_raw(wrapper.shm_state) };
    wrapper.shm_state = ptr::null_mut();
    for region in state.regions.drain(..).flatten() {
        unsafe {
            libc::munmap(region.address, region.length);
        }
    }
    let mut result = ffi::SQLITE_OK;
    if !wrapper.context.is_null() {
        let context = unsafe { &*wrapper.context };
        if let Some(shm) = context.shared_memory.as_ref() {
            let locks = set_ofd_lock(
                shm,
                libc::F_UNLCK as i16,
                UNIX_SHM_BASE,
                ffi::SQLITE_SHM_NLOCK as libc::off_t,
            );
            if locks != ffi::SQLITE_OK {
                result = locks;
            }
            if state.dms_locked {
                let dms = set_ofd_lock(shm, libc::F_UNLCK as i16, UNIX_SHM_DMS, 1);
                if result == ffi::SQLITE_OK && dms != ffi::SQLITE_OK {
                    result = dms;
                }
            }
        }
    }
    result
}

#[cfg(target_os = "linux")]
fn set_ofd_lock(file: &File, lock_type: i16, start: libc::off_t, length: libc::off_t) -> c_int {
    set_descriptor_ofd_lock(
        file.as_raw_fd(),
        lock_type,
        start,
        length,
        ffi::SQLITE_IOERR_SHMLOCK,
    )
}

#[cfg(target_os = "linux")]
fn set_descriptor_ofd_lock(
    descriptor: RawFd,
    lock_type: i16,
    start: libc::off_t,
    length: libc::off_t,
    io_error: c_int,
) -> c_int {
    let lock = libc::flock {
        l_type: lock_type,
        l_whence: libc::SEEK_SET as i16,
        l_start: start,
        l_len: length,
        l_pid: 0,
    };
    if descriptor >= 0 && unsafe { libc::fcntl(descriptor, libc::F_OFD_SETLK, &lock) } == 0 {
        ffi::SQLITE_OK
    } else {
        match io::Error::last_os_error().raw_os_error() {
            Some(libc::EACCES | libc::EAGAIN) => ffi::SQLITE_BUSY,
            _ => io_error,
        }
    }
}

#[cfg(target_os = "linux")]
fn unlock_main(wrapper: &mut WrappedSqliteFile) -> c_int {
    if wrapper.kind != ffi::SQLITE_OPEN_MAIN_DB || wrapper.lock_level == ffi::SQLITE_LOCK_NONE {
        return ffi::SQLITE_OK;
    }
    let code = set_descriptor_ofd_lock(
        wrapper.descriptor,
        libc::F_UNLCK as i16,
        SHARED_FIRST,
        SHARED_SIZE,
        ffi::SQLITE_IOERR_UNLOCK,
    );
    if code == ffi::SQLITE_OK {
        wrapper.lock_level = ffi::SQLITE_LOCK_NONE;
    }
    code
}

#[cfg(target_os = "linux")]
static ROOT_HANDLE_IO_METHODS: ffi::sqlite3_io_methods = ffi::sqlite3_io_methods {
    iVersion: 2,
    xClose: Some(io_close),
    xRead: Some(io_read),
    xWrite: Some(io_write),
    xTruncate: Some(io_truncate),
    xSync: Some(io_sync),
    xFileSize: Some(io_file_size),
    xLock: Some(io_lock),
    xUnlock: Some(io_unlock),
    xCheckReservedLock: Some(io_check_reserved),
    xFileControl: Some(io_file_control),
    xSectorSize: Some(io_sector_size),
    xDeviceCharacteristics: Some(io_device_characteristics),
    xShmMap: Some(io_shm_map),
    xShmLock: Some(io_shm_lock),
    xShmBarrier: Some(io_shm_barrier),
    xShmUnmap: Some(io_shm_unmap),
    xFetch: None,
    xUnfetch: None,
};
