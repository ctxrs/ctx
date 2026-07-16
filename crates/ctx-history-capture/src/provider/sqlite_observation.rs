use std::{
    fs::{File, Metadata},
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(not(any(unix, windows)))]
use sha2::{Digest, Sha256};

pub const SQLITE_GENERATION_MAX_ATTEMPTS: usize = 3;
pub const SQLITE_SNAPSHOT_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const SQLITE_HEADER_BYTES: usize = 100;
const WAL_HEADER_BYTES: usize = 32;
const WAL_FRAME_HEADER_BYTES: usize = 24;
const WAL_FORMAT_VERSION: u32 = 3_007_000;
const JOURNAL_SENTINEL_BYTES: usize = 64;
const MAX_SUPER_JOURNAL_NAME_BYTES: u64 = 64 * 1024;
pub(super) const JOURNAL_MAGIC: [u8; 8] = [0xd9, 0xd5, 0x05, 0xf9, 0x20, 0xa1, 0x63, 0xd7];
const WAL_CHURN_REASON: &str = "SQLite WAL has an incomplete or changing valid generation";
const WAL_RESOURCE_REASON: &str = "SQLite WAL valid prefix exceeds the snapshot resource ceiling";
const WAL_FORMAT_VERSION_REASON: &str = "SQLite WAL format version is unsupported or corrupt";
const WAL_HEADER_CHECKSUM_REASON: &str = "SQLite WAL header checksum is invalid";
const WAL_FRAME_CHECKSUM_REASON: &str = "SQLite WAL frame checksum is invalid";
const SUPER_JOURNAL_REASON: &str =
    "SQLite rollback journal belongs to an unsupported multi-database transaction";

#[derive(Debug, Clone)]
pub struct SqliteObservedFile {
    path: PathBuf,
    source_file: Arc<File>,
    #[cfg(windows)]
    _path_guards: Arc<Vec<File>>,
    #[cfg(windows)]
    pinned_path: PathBuf,
    len: u64,
    modified_at: SystemTime,
    modified_secs: u64,
    modified_nanos: u32,
    sentinel: Vec<u8>,
    snapshot_relevant: bool,
    snapshot_len: u64,
    deferred_reason: Option<&'static str>,
}

impl PartialEq for SqliteObservedFile {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
            && self.len == other.len
            && self.modified_at == other.modified_at
            && self.modified_secs == other.modified_secs
            && self.modified_nanos == other.modified_nanos
            && self.sentinel == other.sentinel
            && self.snapshot_relevant == other.snapshot_relevant
            && self.snapshot_len == other.snapshot_len
            && self.deferred_reason == other.deferred_reason
    }
}

impl Eq for SqliteObservedFile {}

impl SqliteObservedFile {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn modified_at(&self) -> SystemTime {
        self.modified_at
    }

    pub fn modified_secs(&self) -> u64 {
        self.modified_secs
    }

    pub fn modified_nanos(&self) -> u32 {
        self.modified_nanos
    }

    pub fn sentinel(&self) -> &[u8] {
        &self.sentinel
    }

    pub(crate) fn snapshot_len(&self) -> u64 {
        self.snapshot_len
    }

    pub(crate) fn snapshot_reader(&self) -> io::Result<File> {
        let mut file = self.source_file.try_clone()?;
        file.seek(SeekFrom::Start(0))?;
        Ok(file)
    }

    pub(crate) fn pinned_open_path(&self) -> io::Result<PathBuf> {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            use std::os::fd::AsRawFd;

            let descriptor_root = Path::new("/proc/self/fd");
            if !descriptor_root.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "identity-preserving SQLite opens require /proc/self/fd",
                ));
            }
            Ok(descriptor_root.join(self.source_file.as_raw_fd().to_string()))
        }

        #[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
        {
            use std::os::fd::AsRawFd;

            let descriptor_root = Path::new("/dev/fd");
            if !descriptor_root.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "identity-preserving SQLite opens require /dev/fd",
                ));
            }
            Ok(descriptor_root.join(self.source_file.as_raw_fd().to_string()))
        }

        #[cfg(windows)]
        {
            // Every component handle in `_path_guards`, plus `source_file`, was
            // opened without FILE_SHARE_DELETE. The original disk or UNC path
            // therefore remains bound to this observed identity until SQLite
            // finishes opening it.
            Ok(self.pinned_path.clone())
        }

        #[cfg(not(any(unix, windows)))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "identity-preserving SQLite opens are unsupported on this platform",
            ))
        }
    }
}

struct OpenSourceFile {
    file: File,
    #[cfg(windows)]
    path_guards: Vec<File>,
    #[cfg(windows)]
    pinned_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteSourceGeneration {
    main: SqliteObservedFile,
    wal: Option<SqliteObservedFile>,
    journal: Option<SqliteObservedFile>,
}

impl SqliteSourceGeneration {
    pub fn main(&self) -> &SqliteObservedFile {
        &self.main
    }

    pub fn files(&self) -> Vec<&SqliteObservedFile> {
        let mut files = vec![&self.main];
        files.extend(self.wal.iter());
        files.extend(self.journal.iter());
        files
    }

    pub(crate) fn snapshot_files(&self) -> Vec<&SqliteObservedFile> {
        let mut files = vec![&self.main];
        files.extend(self.wal.iter().filter(|file| file.snapshot_relevant));
        files.extend(self.journal.iter().filter(|file| file.snapshot_relevant));
        files
    }

    pub(crate) fn requires_snapshot(&self) -> bool {
        self.main.snapshot_relevant
            || self
                .wal
                .iter()
                .chain(self.journal.iter())
                .any(|file| file.snapshot_relevant)
    }

    pub(crate) fn deferred_reason(&self) -> Option<&'static str> {
        self.wal
            .iter()
            .chain(self.journal.iter())
            .find_map(|file| file.deferred_reason)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SqliteCorruptObservation {
    path: PathBuf,
    reason: &'static str,
    fingerprint: Vec<u8>,
}

impl SqliteCorruptObservation {
    fn into_error(self) -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: {}", self.reason, self.path.display()),
        )
    }
}

enum SqliteGenerationObservation {
    Generation(Box<SqliteSourceGeneration>),
    Corrupt(SqliteCorruptObservation),
}

pub fn observe_sqlite_source_generation(path: &Path) -> io::Result<SqliteSourceGeneration> {
    let mut retryable_error = None;
    let mut stable_invalid_error = None;
    let mut stable_not_found_error = None;
    let mut previous_corruption = None;
    let mut invalid_attempts = 0;
    let mut not_found_attempts = 0;
    for _ in 0..SQLITE_GENERATION_MAX_ATTEMPTS {
        let before = match observe_generation_once(path) {
            Ok(SqliteGenerationObservation::Generation(generation)) => {
                previous_corruption = None;
                *generation
            }
            Ok(SqliteGenerationObservation::Corrupt(corruption)) => {
                if previous_corruption.as_ref() == Some(&corruption) {
                    return Err(corruption.into_error());
                }
                previous_corruption = Some(corruption);
                continue;
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::UnexpectedEof
                ) =>
            {
                previous_corruption = None;
                let error = retryable_observation_error(error);
                retryable_error = Some(error);
                continue;
            }
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => {
                previous_corruption = None;
                stable_invalid_error = Some(error);
                invalid_attempts += 1;
                continue;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                previous_corruption = None;
                stable_not_found_error = Some(error);
                not_found_attempts += 1;
                continue;
            }
            Err(error) => return Err(error),
        };
        if before.deferred_reason() == Some(WAL_RESOURCE_REASON) {
            return Ok(before);
        }
        let after = match observe_generation_once(path) {
            Ok(SqliteGenerationObservation::Generation(generation)) => {
                previous_corruption = None;
                *generation
            }
            Ok(SqliteGenerationObservation::Corrupt(corruption)) => {
                if previous_corruption.as_ref() == Some(&corruption) {
                    return Err(corruption.into_error());
                }
                previous_corruption = Some(corruption);
                continue;
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::UnexpectedEof
                ) =>
            {
                previous_corruption = None;
                let error = retryable_observation_error(error);
                retryable_error = Some(error);
                continue;
            }
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => {
                previous_corruption = None;
                stable_invalid_error = Some(error);
                invalid_attempts += 1;
                continue;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                previous_corruption = None;
                stable_not_found_error = Some(error);
                not_found_attempts += 1;
                continue;
            }
            Err(error) => return Err(error),
        };
        if before == after {
            if after.deferred_reason() == Some(WAL_CHURN_REASON) {
                retryable_error = Some(io::Error::new(io::ErrorKind::WouldBlock, WAL_CHURN_REASON));
                continue;
            }
            return Ok(after);
        }
    }
    if invalid_attempts == SQLITE_GENERATION_MAX_ATTEMPTS {
        return Err(stable_invalid_error.unwrap_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "SQLite source path remained invalid during observation",
            )
        }));
    }
    if not_found_attempts == SQLITE_GENERATION_MAX_ATTEMPTS {
        return Err(stable_not_found_error.unwrap_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "SQLite source remained absent during observation",
            )
        }));
    }
    if let Some(error) = retryable_error {
        return Err(error);
    }
    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        format!(
            "SQLite source generation kept changing while observing {}",
            path.display()
        ),
    ))
}

fn retryable_observation_error(error: io::Error) -> io::Error {
    if error.kind() == io::ErrorKind::UnexpectedEof {
        io::Error::new(
            io::ErrorKind::WouldBlock,
            "SQLite source changed after file metadata was sampled",
        )
    } else {
        error
    }
}

fn observe_generation_once(path: &Path) -> io::Result<SqliteGenerationObservation> {
    let main = observe_required_file(path, SentinelKind::Main)?;
    let wal = match observe_optional_file(&sidecar_path(path, "-wal"), SentinelKind::Wal)? {
        Some(FileObservation::File(file)) => Some(file),
        Some(FileObservation::Corrupt(mut corruption)) => {
            corruption.fingerprint.extend_from_slice(b"sqlite-main-v1");
            corruption
                .fingerprint
                .extend_from_slice(&main.len.to_le_bytes());
            corruption
                .fingerprint
                .extend_from_slice(&main.modified_secs.to_le_bytes());
            corruption
                .fingerprint
                .extend_from_slice(&main.modified_nanos.to_le_bytes());
            corruption.fingerprint.extend_from_slice(&main.sentinel);
            return Ok(SqliteGenerationObservation::Corrupt(corruption));
        }
        None => None,
    };
    let journal =
        match observe_optional_file(&sidecar_path(path, "-journal"), SentinelKind::Journal)? {
            Some(FileObservation::File(file)) => Some(file),
            Some(FileObservation::Corrupt(corruption)) => return Err(corruption.into_error()),
            None => None,
        };
    Ok(SqliteGenerationObservation::Generation(Box::new(
        SqliteSourceGeneration { main, wal, journal },
    )))
}

#[derive(Clone, Copy)]
enum SentinelKind {
    Main,
    Wal,
    Journal,
}

enum FileObservation {
    File(SqliteObservedFile),
    Corrupt(SqliteCorruptObservation),
}

fn observe_required_file(path: &Path, kind: SentinelKind) -> io::Result<SqliteObservedFile> {
    run_observation_test_hook(path, SqliteObservationTestPhase::BeforeOpen);
    let source = open_source_file_no_follow(path)?;
    let metadata = source.file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("SQLite source is not a regular file: {}", path.display()),
        ));
    }
    validate_open_source_file(&source.file)?;
    match observe_file(path, source, metadata, kind)? {
        FileObservation::File(file) => Ok(file),
        FileObservation::Corrupt(corruption) => Err(corruption.into_error()),
    }
}

fn observe_optional_file(path: &Path, kind: SentinelKind) -> io::Result<Option<FileObservation>> {
    run_observation_test_hook(path, SqliteObservationTestPhase::BeforeOpen);
    let source = match open_source_file_no_follow(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let metadata = source.file.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("SQLite sidecar is not a regular file: {}", path.display()),
        ));
    }
    validate_open_source_file(&source.file)?;
    observe_file(path, source, metadata, kind).map(Some)
}

fn observe_file(
    path: &Path,
    source: OpenSourceFile,
    metadata: Metadata,
    kind: SentinelKind,
) -> io::Result<FileObservation> {
    let OpenSourceFile {
        mut file,
        #[cfg(windows)]
        path_guards,
        #[cfg(windows)]
        pinned_path,
    } = source;
    let modified_at = metadata.modified().unwrap_or(UNIX_EPOCH);
    let modified = modified_at.duration_since(UNIX_EPOCH).unwrap_or_default();
    run_observation_test_hook(path, SqliteObservationTestPhase::AfterMetadata);
    let sentinel_observation = match kind {
        SentinelKind::Main => {
            let (sentinel, uses_wal_mode) = main_header_sentinel(&mut file, &metadata)?;
            WalSentinel::Observed(sentinel, uses_wal_mode, metadata.len(), None)
        }
        SentinelKind::Wal => wal_sentinel(path, &mut file, metadata.len())?,
        SentinelKind::Journal => {
            let (sentinel, snapshot_relevant, snapshot_len, deferred_reason) =
                journal_sentinel(path, &mut file, metadata.len())?;
            WalSentinel::Observed(sentinel, snapshot_relevant, snapshot_len, deferred_reason)
        }
    };
    let (sentinel, snapshot_relevant, snapshot_len, deferred_reason) = match sentinel_observation {
        WalSentinel::Observed(sentinel, snapshot_relevant, snapshot_len, deferred_reason) => {
            (sentinel, snapshot_relevant, snapshot_len, deferred_reason)
        }
        WalSentinel::Corrupt {
            reason,
            mut fingerprint,
        } => {
            fingerprint.extend_from_slice(&metadata.len().to_le_bytes());
            fingerprint.extend_from_slice(&modified.as_secs().to_le_bytes());
            fingerprint.extend_from_slice(&modified.subsec_nanos().to_le_bytes());
            return Ok(FileObservation::Corrupt(SqliteCorruptObservation {
                path: path.to_path_buf(),
                reason,
                fingerprint,
            }));
        }
    };
    Ok(FileObservation::File(SqliteObservedFile {
        path: path.to_path_buf(),
        source_file: Arc::new(file),
        #[cfg(windows)]
        _path_guards: Arc::new(path_guards),
        #[cfg(windows)]
        pinned_path,
        len: metadata.len(),
        modified_at,
        modified_secs: modified.as_secs(),
        modified_nanos: modified.subsec_nanos(),
        sentinel,
        snapshot_relevant,
        snapshot_len,
        deferred_reason,
    }))
}

#[cfg(unix)]
fn open_source_file_no_follow(path: &Path) -> io::Result<OpenSourceFile> {
    use std::{
        ffi::CString,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::ffi::OsStrExt,
        },
        path::Component,
    };

    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::Prefix(_) => {}
            Component::CurDir | Component::ParentDir | Component::Normal(_) => {
                components.push(component.as_os_str())
            }
        }
    }
    let Some((file_name, parents)) = components.split_last() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "SQLite source path has no file component",
        ));
    };
    let base = if path.is_absolute() { b"/\0" } else { b".\0" };
    let base_fd = unsafe {
        libc::open(
            base.as_ptr().cast(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if base_fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut directory = unsafe { File::from_raw_fd(base_fd) };
    for component in parents {
        let component = CString::new(component.as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "SQLite source path contains an interior NUL byte",
            )
        })?;
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                component.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(normalize_no_follow_open_error(io::Error::last_os_error()));
        }
        directory = unsafe { File::from_raw_fd(fd) };
    }
    let file_name = CString::new(file_name.as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "SQLite source path contains an interior NUL byte",
        )
    })?;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            file_name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        return Err(normalize_no_follow_open_error(io::Error::last_os_error()));
    }
    Ok(OpenSourceFile {
        file: unsafe { File::from_raw_fd(fd) },
    })
}

#[cfg(unix)]
fn normalize_no_follow_open_error(error: io::Error) -> io::Error {
    if error
        .raw_os_error()
        .is_some_and(|code| code == libc::ELOOP || code == libc::ENOTDIR)
    {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "SQLite source path contains a symbolic link or non-directory component",
        )
    } else {
        error
    }
}

#[cfg(windows)]
fn open_source_file_no_follow(path: &Path) -> io::Result<OpenSourceFile> {
    use std::{
        ffi::OsString,
        os::windows::fs::OpenOptionsExt,
        path::{Component, Prefix},
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
        FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE, SYNCHRONIZE,
    };

    let absolute = std::path::absolute(path)?;
    let mut components = absolute.components();
    let prefix = match components.next() {
        Some(Component::Prefix(prefix))
            if matches!(
                prefix.kind(),
                Prefix::Disk(_)
                    | Prefix::VerbatimDisk(_)
                    | Prefix::UNC(_, _)
                    | Prefix::VerbatimUNC(_, _)
            ) =>
        {
            prefix
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "SQLite source path has an unsupported Windows prefix",
            ))
        }
    };
    let root_component = match components.next() {
        Some(Component::RootDir) => Component::RootDir,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "SQLite source path is not fully qualified",
            ))
        }
    };
    let mut root_path = PathBuf::from(prefix.as_os_str());
    root_path.push(root_component.as_os_str());
    let names = components
        .map(|component| match component {
            Component::Normal(name) => Ok(OsString::from(name)),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "SQLite source path contains an unsupported Windows component",
            )),
        })
        .collect::<io::Result<Vec<_>>>()?;
    let Some((file_name, parents)) = names.split_last() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "SQLite source path has no file component",
        ));
    };

    let mut options = std::fs::OpenOptions::new();
    options
        .access_mode(FILE_READ_ATTRIBUTES | FILE_TRAVERSE | SYNCHRONIZE)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    let mut directory = options.open(&root_path)?;
    validate_windows_path_component(&directory, true)?;

    // Windows has no descriptor pathname that SQLite's default VFS can open.
    // Retaining every no-share-delete component handle makes a second open of
    // the original disk/UNC path identity-preserving: no component can be
    // renamed away and replaced by a junction while these guards are alive.
    let mut path_guards = Vec::with_capacity(parents.len() + 1);
    let mut opened_path = root_path;
    for component in parents {
        let next = open_windows_component(&directory, component, true)?;
        validate_windows_path_component(&next, true)?;
        path_guards.push(directory);
        directory = next;
        opened_path.push(component);
        run_observation_test_hook(&opened_path, SqliteObservationTestPhase::AfterParentOpen);
    }

    let file = open_windows_component(&directory, file_name, false)?;
    validate_windows_path_component(&file, false)?;
    path_guards.push(directory);
    opened_path.push(file_name);
    Ok(OpenSourceFile {
        file,
        path_guards,
        pinned_path: opened_path,
    })
}

#[cfg(windows)]
fn open_windows_component(
    parent: &File,
    name: &std::ffi::OsStr,
    directory: bool,
) -> io::Result<File> {
    use std::{
        mem,
        os::windows::{ffi::OsStrExt, io::AsRawHandle, io::FromRawHandle},
        ptr,
    };
    use windows_sys::{
        Wdk::{
            Foundation::OBJECT_ATTRIBUTES,
            Storage::FileSystem::{
                NtCreateFile, FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN,
                FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT,
            },
        },
        Win32::{
            Foundation::{
                RtlNtStatusToDosError, HANDLE, OBJ_CASE_INSENSITIVE, STATUS_FILE_IS_A_DIRECTORY,
                STATUS_NOT_A_DIRECTORY, STATUS_REPARSE_POINT_ENCOUNTERED, UNICODE_STRING,
            },
            Storage::FileSystem::{
                FILE_GENERIC_READ, FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE,
                FILE_TRAVERSE, SYNCHRONIZE,
            },
            System::IO::IO_STATUS_BLOCK,
        },
    };

    let mut name = name.encode_wide().collect::<Vec<_>>();
    let byte_len = name
        .len()
        .checked_mul(mem::size_of::<u16>())
        .and_then(|len| u16::try_from(len).ok())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "SQLite source path component is too long for Windows",
            )
        })?;
    if name.is_empty() || name.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "SQLite source path contains an empty component or interior NUL byte",
        ));
    }
    let object_name = UNICODE_STRING {
        Length: byte_len,
        MaximumLength: byte_len,
        Buffer: name.as_mut_ptr(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.as_raw_handle() as HANDLE,
        ObjectName: &object_name,
        Attributes: OBJ_CASE_INSENSITIVE,
        SecurityDescriptor: ptr::null(),
        SecurityQualityOfService: ptr::null(),
    };
    let mut handle: HANDLE = ptr::null_mut();
    let mut status_block = IO_STATUS_BLOCK::default();
    let desired_access = if directory {
        FILE_READ_ATTRIBUTES | FILE_TRAVERSE | SYNCHRONIZE
    } else {
        FILE_GENERIC_READ
    };
    let create_options = FILE_OPEN_REPARSE_POINT
        | FILE_SYNCHRONOUS_IO_NONALERT
        | if directory {
            FILE_DIRECTORY_FILE
        } else {
            FILE_NON_DIRECTORY_FILE
        };
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            desired_access,
            &attributes,
            &mut status_block,
            ptr::null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            FILE_OPEN,
            create_options,
            ptr::null(),
            0,
        )
    };
    if status < 0 {
        if matches!(
            status,
            STATUS_REPARSE_POINT_ENCOUNTERED | STATUS_NOT_A_DIRECTORY | STATUS_FILE_IS_A_DIRECTORY
        ) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "SQLite source path contains a reparse point or invalid component type",
            ));
        }
        return Err(io::Error::from_raw_os_error(unsafe {
            RtlNtStatusToDosError(status) as i32
        }));
    }
    Ok(unsafe { File::from_raw_handle(handle) })
}

#[cfg(windows)]
fn validate_windows_path_component(file: &File, directory: bool) -> io::Result<()> {
    let metadata = file.metadata()?;
    if metadata.file_type().is_dir() != directory {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            if directory {
                "SQLite source parent component is not a directory"
            } else {
                "SQLite source is not a regular file"
            },
        ));
    }
    validate_open_source_file(file)
}

#[cfg(not(any(unix, windows)))]
fn open_source_file_no_follow(_path: &Path) -> io::Result<OpenSourceFile> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic no-follow SQLite source access is unsupported on this platform",
    ))
}

#[cfg(windows)]
fn validate_open_source_file(file: &File) -> io::Result<()> {
    use std::{mem::MaybeUninit, os::windows::io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        FileAttributeTagInfo, GetFileInformationByHandleEx, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_ATTRIBUTE_TAG_INFO,
    };

    let handle = file.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
    let mut attributes = MaybeUninit::<FILE_ATTRIBUTE_TAG_INFO>::zeroed();
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileAttributeTagInfo,
            attributes.as_mut_ptr().cast(),
            std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let attributes = unsafe { attributes.assume_init() };
    if attributes.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "SQLite source path component is a reparse point",
        ));
    }
    Ok(())
}

#[cfg(not(windows))]
fn validate_open_source_file(_file: &File) -> io::Result<()> {
    Ok(())
}

include!("sqlite_observation/sentinels.rs");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SqliteObservationTestPhase {
    BeforeOpen,
    #[cfg(windows)]
    AfterParentOpen,
    AfterMetadata,
    BeforeWalFrameRead,
    BeforeJournalTailRead,
    BeforeJournalTrailerRead,
}

#[cfg(test)]
type SqliteObservationTestHook = Box<dyn FnMut(&Path, SqliteObservationTestPhase)>;

#[cfg(test)]
thread_local! {
    static OBSERVATION_TEST_HOOK: std::cell::RefCell<Option<SqliteObservationTestHook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) struct SqliteObservationTestHookGuard;

#[cfg(test)]
impl Drop for SqliteObservationTestHookGuard {
    fn drop(&mut self) {
        OBSERVATION_TEST_HOOK.with(|hook| *hook.borrow_mut() = None);
    }
}

#[cfg(test)]
pub(crate) fn install_sqlite_observation_test_hook(
    hook: impl FnMut(&Path, SqliteObservationTestPhase) + 'static,
) -> SqliteObservationTestHookGuard {
    OBSERVATION_TEST_HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
    SqliteObservationTestHookGuard
}

#[cfg(test)]
fn run_observation_test_hook(path: &Path, phase: SqliteObservationTestPhase) {
    OBSERVATION_TEST_HOOK.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook(path, phase);
        }
    });
}

#[cfg(not(test))]
fn run_observation_test_hook(_path: &Path, _phase: SqliteObservationTestPhase) {}

#[cfg(test)]
include!("sqlite_observation/tests.rs");
