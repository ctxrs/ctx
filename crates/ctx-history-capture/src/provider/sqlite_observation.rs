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
    immutable_path: PathBuf,
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

    pub(crate) fn immutable_open_path(&self) -> io::Result<PathBuf> {
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
            Ok(self.immutable_path.clone())
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
    immutable_path: PathBuf,
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
        self.wal
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
        immutable_path,
    } = source;
    let modified_at = metadata.modified().unwrap_or(UNIX_EPOCH);
    let modified = modified_at.duration_since(UNIX_EPOCH).unwrap_or_default();
    run_observation_test_hook(path, SqliteObservationTestPhase::AfterMetadata);
    let sentinel_observation = match kind {
        SentinelKind::Main => WalSentinel::Observed(
            main_header_sentinel(&mut file, &metadata)?,
            false,
            metadata.len(),
            None,
        ),
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
        immutable_path,
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
        immutable_path: opened_path,
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

fn main_header_sentinel(file: &mut File, metadata: &Metadata) -> io::Result<Vec<u8>> {
    let header = read_prefix(file, SQLITE_HEADER_BYTES)?;
    let mut sentinel = b"sqlite-main-v2".to_vec();
    if header.starts_with(b"SQLite format 3\0") && header.len() >= SQLITE_HEADER_BYTES {
        for range in [24..32, 40..48, 60..64, 92..100] {
            sentinel.extend_from_slice(&header[range]);
        }
    } else {
        sentinel.extend_from_slice(&header);
    }
    append_main_file_identity(&mut sentinel, file, metadata)?;
    Ok(sentinel)
}

#[cfg(unix)]
fn append_main_file_identity(
    sentinel: &mut Vec<u8>,
    _file: &File,
    metadata: &Metadata,
) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;

    sentinel.extend_from_slice(b"unix-file-id-v1");
    sentinel.extend_from_slice(&metadata.dev().to_le_bytes());
    sentinel.extend_from_slice(&metadata.ino().to_le_bytes());
    sentinel.extend_from_slice(&metadata.ctime().to_le_bytes());
    sentinel.extend_from_slice(&metadata.ctime_nsec().to_le_bytes());
    Ok(())
}

#[cfg(windows)]
fn append_main_file_identity(
    sentinel: &mut Vec<u8>,
    file: &File,
    _metadata: &Metadata,
) -> io::Result<()> {
    use std::{mem::MaybeUninit, os::windows::io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        FileBasicInfo, FileIdInfo, GetFileInformationByHandleEx, FILE_BASIC_INFO, FILE_ID_INFO,
    };

    let handle = file.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
    let mut id = MaybeUninit::<FILE_ID_INFO>::zeroed();
    let id_ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            id.as_mut_ptr().cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if id_ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut basic = MaybeUninit::<FILE_BASIC_INFO>::zeroed();
    let basic_ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileBasicInfo,
            basic.as_mut_ptr().cast(),
            std::mem::size_of::<FILE_BASIC_INFO>() as u32,
        )
    };
    if basic_ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let id = unsafe { id.assume_init() };
    let basic = unsafe { basic.assume_init() };
    sentinel.extend_from_slice(b"windows-file-id-v1");
    sentinel.extend_from_slice(&id.VolumeSerialNumber.to_le_bytes());
    sentinel.extend_from_slice(&id.FileId.Identifier);
    sentinel.extend_from_slice(&basic.ChangeTime.to_le_bytes());
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn append_main_file_identity(
    sentinel: &mut Vec<u8>,
    file: &File,
    _metadata: &Metadata,
) -> io::Result<()> {
    let mut file = file.try_clone()?;
    file.seek(SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    sentinel.extend_from_slice(b"full-sha256-fallback-v1");
    sentinel.extend_from_slice(&hasher.finalize());
    Ok(())
}

enum WalSentinel {
    Observed(Vec<u8>, bool, u64, Option<&'static str>),
    Corrupt {
        reason: &'static str,
        fingerprint: Vec<u8>,
    },
}

fn wal_sentinel(path: &Path, file: &mut File, len: u64) -> io::Result<WalSentinel> {
    let header = read_prefix(file, WAL_HEADER_BYTES)?;
    let mut sentinel = b"sqlite-wal-v2".to_vec();
    sentinel.extend_from_slice(&header);
    let wal_header = match parse_wal_header(&header) {
        WalHeaderState::Valid(header) => header,
        WalHeaderState::Ignore => return Ok(WalSentinel::Observed(sentinel, false, 0, None)),
        WalHeaderState::Defer => {
            return Ok(WalSentinel::Observed(
                sentinel,
                false,
                0,
                Some(WAL_CHURN_REASON),
            ))
        }
        WalHeaderState::Corrupt(reason) => {
            return Ok(WalSentinel::Corrupt {
                reason,
                fingerprint: sentinel,
            })
        }
    };
    let frame_size = u64::from(wal_header.page_size) + WAL_FRAME_HEADER_BYTES as u64;
    let physical_frames = len.saturating_sub(WAL_HEADER_BYTES as u64) / frame_size;
    let trailing_bytes = len.saturating_sub(WAL_HEADER_BYTES as u64) % frame_size;
    let mut checksum = wal_header.checksum;
    let mut page = vec![0_u8; wal_header.page_size as usize];
    let mut valid_frames = 0_u64;
    let mut last_commit = None;
    let mut stale_suffix = false;
    let mut churning_suffix = false;
    let mut corrupt_suffix = None;
    for frame in 1..=physical_frames {
        let offset = wal_frame_offset(frame, frame_size)?;
        run_observation_test_hook(path, SqliteObservationTestPhase::BeforeWalFrameRead);
        let frame_header = read_wal_frame_header(file, offset)?;
        if frame_header[8..16] != wal_header.salts {
            stale_suffix = true;
            break;
        }
        wal_frame_end_within_snapshot_ceiling(offset, frame_size)?;
        file.read_exact(&mut page)?;
        if be_u32(&frame_header[0..4]) == 0 {
            churning_suffix = true;
            break;
        }
        checksum = wal_checksum(wal_header.checksum_order, &frame_header[..8], checksum);
        checksum = wal_checksum(wal_header.checksum_order, &page, checksum);
        if checksum != [be_u32(&frame_header[16..20]), be_u32(&frame_header[20..24])] {
            let mut fingerprint = sentinel.clone();
            fingerprint.extend_from_slice(&frame.to_le_bytes());
            fingerprint.extend_from_slice(&frame_header);
            fingerprint.extend_from_slice(&checksum[0].to_le_bytes());
            fingerprint.extend_from_slice(&checksum[1].to_le_bytes());
            corrupt_suffix = Some(fingerprint);
            break;
        }
        valid_frames = frame;
        if be_u32(&frame_header[4..8]) != 0 {
            last_commit = Some((frame, checksum));
        }
    }

    sentinel.extend_from_slice(&valid_frames.to_le_bytes());
    if let Some((frame, checksum)) = last_commit {
        sentinel.extend_from_slice(&frame.to_le_bytes());
        sentinel.extend_from_slice(&checksum[0].to_le_bytes());
        sentinel.extend_from_slice(&checksum[1].to_le_bytes());
        let committed_len = wal_frame_offset(frame + 1, frame_size)?;
        return Ok(WalSentinel::Observed(sentinel, true, committed_len, None));
    }
    if let Some(fingerprint) = corrupt_suffix {
        return Ok(WalSentinel::Corrupt {
            reason: WAL_FRAME_CHECKSUM_REASON,
            fingerprint,
        });
    }
    if churning_suffix || (trailing_bytes != 0 && !stale_suffix) {
        return Ok(WalSentinel::Observed(
            sentinel,
            false,
            0,
            Some(WAL_CHURN_REASON),
        ));
    }
    Ok(WalSentinel::Observed(sentinel, false, 0, None))
}

#[derive(Clone, Copy)]
struct WalHeader {
    page_size: u32,
    salts: [u8; 8],
    checksum_order: WalChecksumOrder,
    checksum: [u32; 2],
}

enum WalHeaderState {
    Valid(WalHeader),
    Ignore,
    Defer,
    Corrupt(&'static str),
}

#[derive(Clone, Copy)]
enum WalChecksumOrder {
    LittleEndian,
    BigEndian,
}

fn parse_wal_header(header: &[u8]) -> WalHeaderState {
    if header.is_empty() {
        return WalHeaderState::Ignore;
    }
    if header.len() >= 4 && !matches!(be_u32(&header[0..4]), 0x377f_0682 | 0x377f_0683) {
        return WalHeaderState::Ignore;
    }
    if header.len() < WAL_HEADER_BYTES {
        return WalHeaderState::Defer;
    }
    let checksum_order = match be_u32(&header[0..4]) {
        0x377f_0682 => WalChecksumOrder::LittleEndian,
        0x377f_0683 => WalChecksumOrder::BigEndian,
        _ => return WalHeaderState::Ignore,
    };
    if be_u32(&header[4..8]) != WAL_FORMAT_VERSION {
        return WalHeaderState::Corrupt(WAL_FORMAT_VERSION_REASON);
    }
    let page_size = be_u32(&header[8..12]);
    if !page_size.is_power_of_two() || !(512..=65_536).contains(&page_size) {
        return WalHeaderState::Ignore;
    }
    let checksum = wal_checksum(checksum_order, &header[..24], [0, 0]);
    if checksum != [be_u32(&header[24..28]), be_u32(&header[28..32])] {
        return WalHeaderState::Corrupt(WAL_HEADER_CHECKSUM_REASON);
    }
    WalHeaderState::Valid(WalHeader {
        page_size,
        salts: header[16..24].try_into().unwrap_or_default(),
        checksum_order,
        checksum,
    })
}

fn wal_frame_offset(frame: u64, frame_size: u64) -> io::Result<u64> {
    (WAL_HEADER_BYTES as u64)
        .checked_add(
            frame
                .saturating_sub(1)
                .checked_mul(frame_size)
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "SQLite WAL frame offset overflow",
                    )
                })?,
        )
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "SQLite WAL frame offset overflow",
            )
        })
}

fn wal_frame_end_within_snapshot_ceiling(offset: u64, frame_size: u64) -> io::Result<u64> {
    let frame_end = offset
        .checked_add(frame_size)
        .ok_or_else(|| io::Error::new(io::ErrorKind::WouldBlock, WAL_RESOURCE_REASON))?;
    if frame_end > SQLITE_SNAPSHOT_MAX_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            WAL_RESOURCE_REASON,
        ));
    }
    Ok(frame_end)
}

fn read_wal_frame_header(file: &mut File, offset: u64) -> io::Result<[u8; 24]> {
    let mut header = [0_u8; WAL_FRAME_HEADER_BYTES];
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(&mut header)?;
    Ok(header)
}

fn wal_checksum(order: WalChecksumOrder, bytes: &[u8], initial: [u32; 2]) -> [u32; 2] {
    debug_assert_eq!(bytes.len() % 8, 0);
    let mut s1 = initial[0];
    let mut s2 = initial[1];
    for words in bytes.chunks_exact(8) {
        let first = match order {
            WalChecksumOrder::LittleEndian => {
                u32::from_le_bytes(words[0..4].try_into().unwrap_or_default())
            }
            WalChecksumOrder::BigEndian => {
                u32::from_be_bytes(words[0..4].try_into().unwrap_or_default())
            }
        };
        let second = match order {
            WalChecksumOrder::LittleEndian => {
                u32::from_le_bytes(words[4..8].try_into().unwrap_or_default())
            }
            WalChecksumOrder::BigEndian => {
                u32::from_be_bytes(words[4..8].try_into().unwrap_or_default())
            }
        };
        s1 = s1.wrapping_add(first).wrapping_add(s2);
        s2 = s2.wrapping_add(second).wrapping_add(s1);
    }
    [s1, s2]
}

fn journal_sentinel(
    journal_path: &Path,
    file: &mut File,
    len: u64,
) -> io::Result<(Vec<u8>, bool, u64, Option<&'static str>)> {
    let prefix = read_prefix(file, JOURNAL_SENTINEL_BYTES)?;
    let mut sentinel = b"sqlite-journal-v2".to_vec();
    sentinel.extend_from_slice(&prefix);
    run_observation_test_hook(
        journal_path,
        SqliteObservationTestPhase::BeforeJournalTailRead,
    );
    sentinel.extend_from_slice(&read_tail(file, len, JOURNAL_SENTINEL_BYTES)?);
    let hot = hot_journal_header(&prefix, len);
    if hot {
        if let Some(super_journal) = super_journal_path(journal_path, file, len)? {
            match super_journal.try_exists() {
                Ok(true) => {
                    sentinel.extend_from_slice(b"super-journal-present");
                    return Ok((sentinel, false, 0, Some(SUPER_JOURNAL_REASON)));
                }
                Ok(false) => {
                    sentinel.extend_from_slice(b"super-journal-missing");
                    return Ok((sentinel, false, 0, None));
                }
                Err(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "SQLite super-journal presence could not be established",
                    ));
                }
            }
        }
    }
    Ok((sentinel, hot, if hot { len } else { 0 }, None))
}

fn hot_journal_header(prefix: &[u8], len: u64) -> bool {
    if len <= 512 || prefix.len() < 28 || !prefix.starts_with(&JOURNAL_MAGIC) {
        return false;
    }
    let sector_size = be_u32(&prefix[20..24]);
    let page_size = be_u32(&prefix[24..28]);
    sector_size.is_power_of_two()
        && (512..=65_536).contains(&sector_size)
        && page_size.is_power_of_two()
        && (512..=65_536).contains(&page_size)
        && len >= u64::from(sector_size)
}

fn super_journal_path(
    journal_path: &Path,
    file: &mut File,
    len: u64,
) -> io::Result<Option<PathBuf>> {
    if len < 16 {
        return Ok(None);
    }
    run_observation_test_hook(
        journal_path,
        SqliteObservationTestPhase::BeforeJournalTrailerRead,
    );
    let trailer = read_at(file, len - 16, 16)?;
    if trailer[8..16] != JOURNAL_MAGIC {
        return Ok(None);
    }
    let name_len = u64::from(be_u32(&trailer[0..4]));
    if name_len == 0 || name_len > MAX_SUPER_JOURNAL_NAME_BYTES || name_len > len.saturating_sub(16)
    {
        return Ok(None);
    }
    let name = read_at(file, len - 16 - name_len, name_len as usize)?;
    let expected = be_u32(&trailer[4..8]);
    let actual = name.iter().fold(0_u32, |sum, byte| {
        sum.wrapping_add((*byte as i8 as i32) as u32)
    });
    if actual != expected || name.contains(&0) {
        return Ok(None);
    }
    let path = native_super_journal_path(name)?;
    if path.is_absolute() {
        Ok(Some(path))
    } else {
        Ok(Some(
            journal_path
                .parent()
                .unwrap_or_else(|| Path::new(""))
                .join(path),
        ))
    }
}

#[cfg(unix)]
fn native_super_journal_path(name: Vec<u8>) -> io::Result<PathBuf> {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};

    Ok(PathBuf::from(OsString::from_vec(name)))
}

#[cfg(windows)]
fn native_super_journal_path(name: Vec<u8>) -> io::Result<PathBuf> {
    let name = std::str::from_utf8(&name).map_err(|_| {
        io::Error::new(
            io::ErrorKind::WouldBlock,
            "SQLite super-journal path is not valid native UTF-8",
        )
    })?;
    Ok(PathBuf::from(name))
}

#[cfg(not(any(unix, windows)))]
fn native_super_journal_path(name: Vec<u8>) -> io::Result<PathBuf> {
    let name = std::str::from_utf8(&name).map_err(|_| {
        io::Error::new(
            io::ErrorKind::WouldBlock,
            "SQLite super-journal path is not valid native UTF-8",
        )
    })?;
    Ok(PathBuf::from(name))
}

fn read_prefix(file: &mut File, limit: usize) -> io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = vec![0_u8; limit];
    let read = file.read(&mut bytes)?;
    bytes.truncate(read);
    Ok(bytes)
}

fn read_tail(file: &mut File, len: u64, limit: usize) -> io::Result<Vec<u8>> {
    let count = usize::try_from(len.min(limit as u64)).unwrap_or(limit);
    file.seek(SeekFrom::Start(len.saturating_sub(count as u64)))?;
    let mut bytes = vec![0_u8; count];
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn read_at(file: &mut File, offset: u64, len: usize) -> io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = vec![0_u8; len];
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn be_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes(bytes.try_into().unwrap_or_default())
}

pub(crate) fn sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut sidecar = path.as_os_str().to_owned();
    sidecar.push(suffix);
    PathBuf::from(sidecar)
}

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
mod tests {
    use std::{cell::Cell, fs, fs::FileTimes, rc::Rc, thread, time::Duration};

    use rusqlite::Connection;

    use super::*;

    #[test]
    fn real_wal_validates_supported_page_sizes_and_both_checksum_orders() {
        for page_size in [512_u32, 65_536] {
            let fixture = real_wal_fixture(page_size);
            let generation = observe_sqlite_source_generation(&fixture.db).unwrap();
            assert!(generation.requires_snapshot(), "page size {page_size}");
            let wal = generation.wal.as_ref().unwrap();
            assert!(wal.snapshot_len() <= wal.len());

            let alternate = fixture
                .temp
                .path()
                .join(format!("alternate-{page_size}.db"));
            fs::copy(&fixture.db, &alternate).unwrap();
            let mut wal_bytes = fs::read(sidecar_path(&fixture.db, "-wal")).unwrap();
            let order = match be_u32(&wal_bytes[0..4]) {
                0x377f_0682 => WalChecksumOrder::BigEndian,
                0x377f_0683 => WalChecksumOrder::LittleEndian,
                magic => panic!("unexpected SQLite WAL magic {magic:#x}"),
            };
            rewrite_wal_checksum_order(&mut wal_bytes, order);
            fs::write(sidecar_path(&alternate, "-wal"), wal_bytes).unwrap();
            assert!(
                observe_sqlite_source_generation(&alternate)
                    .unwrap()
                    .requires_snapshot(),
                "alternate checksum order for page size {page_size}"
            );
        }
    }

    #[test]
    fn real_wal_classifies_stable_corruption_stale_suffix_and_partial_frame() {
        let fixture = real_wal_fixture(512);
        let original = fs::read(sidecar_path(&fixture.db, "-wal")).unwrap();
        assert!(original.len() > WAL_HEADER_BYTES + WAL_FRAME_HEADER_BYTES);

        type WalCorruptionMutation = (&'static str, fn(&mut Vec<u8>));
        let corruptions: [WalCorruptionMutation; 4] = [
            ("header-checksum", |bytes: &mut Vec<u8>| bytes[24] ^= 0x01),
            ("salt", |bytes: &mut Vec<u8>| bytes[40] ^= 0x01),
            ("frame-checksum", |bytes: &mut Vec<u8>| bytes[48] ^= 0x01),
            ("partial-frame", |bytes: &mut Vec<u8>| {
                bytes.pop();
            }),
        ];
        for (label, mutate) in corruptions {
            let db = fixture.temp.path().join(format!("bad-{label}.db"));
            fs::copy(&fixture.db, &db).unwrap();
            let mut bytes = original.clone();
            mutate(&mut bytes);
            fs::write(sidecar_path(&db, "-wal"), bytes).unwrap();
            if label == "salt" {
                let generation = observe_sqlite_source_generation(&db).unwrap();
                assert!(!generation.requires_snapshot(), "{label}");
                assert!(generation.deferred_reason().is_none(), "{label}");
            } else {
                let error = observe_sqlite_source_generation(&db).unwrap_err();
                let expected = if label == "partial-frame" {
                    io::ErrorKind::WouldBlock
                } else {
                    io::ErrorKind::InvalidData
                };
                assert_eq!(error.kind(), expected, "{label}");
            }
        }
    }

    #[test]
    fn transient_bad_wal_header_checksum_retries_when_the_generation_is_repaired() {
        let fixture = real_wal_fixture(512);
        let original = fs::read(sidecar_path(&fixture.db, "-wal")).unwrap();
        let db = fixture.temp.path().join("transient-header-checksum.db");
        fs::copy(&fixture.db, &db).unwrap();
        let wal = sidecar_path(&db, "-wal");
        let mut corrupt = original.clone();
        corrupt[24] ^= 0x01;
        fs::write(&wal, corrupt).unwrap();
        let wal_opens = Rc::new(Cell::new(0_usize));
        let wal_opens_for_hook = Rc::clone(&wal_opens);
        let wal_for_hook = wal.clone();
        let _hook = install_sqlite_observation_test_hook(move |path, phase| {
            if path != wal_for_hook || phase != SqliteObservationTestPhase::BeforeOpen {
                return;
            }
            let opens = wal_opens_for_hook.get() + 1;
            wal_opens_for_hook.set(opens);
            if opens == 2 {
                fs::write(path, &original).unwrap();
            }
        });

        let generation = observe_sqlite_source_generation(&db).unwrap();
        assert!(wal_opens.get() >= 3);
        assert!(generation.requires_snapshot());
    }

    #[test]
    fn stable_unsupported_wal_version_is_terminal() {
        let fixture = real_wal_fixture(512);
        let db = fixture.temp.path().join("unsupported-version.db");
        fs::copy(&fixture.db, &db).unwrap();
        let mut wal = fs::read(sidecar_path(&fixture.db, "-wal")).unwrap();
        rewrite_wal_format_version(&mut wal, WAL_FORMAT_VERSION + 1);
        fs::write(sidecar_path(&db, "-wal"), wal).unwrap();

        let error = observe_sqlite_source_generation(&db).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("format version"));
    }

    #[test]
    fn transient_unsupported_wal_version_retries_when_repaired() {
        let fixture = real_wal_fixture(512);
        let original = fs::read(sidecar_path(&fixture.db, "-wal")).unwrap();
        let db = fixture.temp.path().join("transient-version.db");
        fs::copy(&fixture.db, &db).unwrap();
        let wal = sidecar_path(&db, "-wal");
        let mut unsupported = original.clone();
        rewrite_wal_format_version(&mut unsupported, WAL_FORMAT_VERSION + 1);
        fs::write(&wal, unsupported).unwrap();
        let wal_opens = Rc::new(Cell::new(0_usize));
        let wal_opens_for_hook = Rc::clone(&wal_opens);
        let wal_for_hook = wal.clone();
        let _hook = install_sqlite_observation_test_hook(move |path, phase| {
            if path != wal_for_hook || phase != SqliteObservationTestPhase::BeforeOpen {
                return;
            }
            let opens = wal_opens_for_hook.get() + 1;
            wal_opens_for_hook.set(opens);
            if opens == 2 {
                fs::write(path, &original).unwrap();
            }
        });

        let generation = observe_sqlite_source_generation(&db).unwrap();
        assert!(wal_opens.get() >= 3);
        assert!(generation.requires_snapshot());
    }

    #[test]
    fn bad_frame_after_committed_wal_prefix_preserves_that_prefix() {
        let fixture = real_wal_fixture(512);
        let wal_path = sidecar_path(&fixture.db, "-wal");
        let committed_prefix_len = fs::metadata(&wal_path).unwrap().len();
        fixture
            .writer
            .execute("UPDATE entries SET value = 'sigma' WHERE id = 1", [])
            .unwrap();
        let mut wal = fs::read(&wal_path).unwrap();
        assert!(wal.len() as u64 > committed_prefix_len);
        wal[committed_prefix_len as usize + WAL_FRAME_HEADER_BYTES] ^= 0x01;

        let db = fixture.temp.path().join("valid-prefix.db");
        fs::copy(&fixture.db, &db).unwrap();
        fs::write(sidecar_path(&db, "-wal"), wal).unwrap();
        let generation = observe_sqlite_source_generation(&db).unwrap();
        let observed_wal = generation.wal.as_ref().unwrap();
        assert!(generation.requires_snapshot());
        assert_eq!(observed_wal.snapshot_len(), committed_prefix_len);
        assert!(generation.deferred_reason().is_none());
    }

    #[test]
    fn wal_reset_after_metadata_sampling_retries_instead_of_returning_unexpected_eof() {
        let WalFixture {
            temp: _temp,
            db,
            writer,
        } = real_wal_fixture(512);
        let reset = Rc::new(Cell::new(false));
        let reset_for_hook = Rc::clone(&reset);
        let wal = sidecar_path(&db, "-wal");
        let _hook = install_sqlite_observation_test_hook(move |path, phase| {
            if path == wal
                && phase == SqliteObservationTestPhase::BeforeWalFrameRead
                && !reset_for_hook.replace(true)
            {
                writer
                    .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
                    .unwrap();
            }
        });

        let generation = observe_sqlite_source_generation(&db).unwrap();
        assert!(reset.get());
        assert!(!generation.requires_snapshot());
    }

    #[test]
    fn journal_tail_truncation_after_metadata_sampling_retries() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("tail-race.db");
        fs::write(&db, b"SQLite format 3\0").unwrap();
        let journal = sidecar_path(&db, "-journal");
        fs::write(&journal, real_hot_journal_bytes(512)).unwrap();
        let truncated = Rc::new(Cell::new(false));
        let truncated_for_hook = Rc::clone(&truncated);
        let journal_for_hook = journal.clone();
        let _hook = install_sqlite_observation_test_hook(move |path, phase| {
            if path == journal_for_hook
                && phase == SqliteObservationTestPhase::BeforeJournalTailRead
                && !truncated_for_hook.replace(true)
            {
                fs::OpenOptions::new()
                    .write(true)
                    .open(path)
                    .unwrap()
                    .set_len(0)
                    .unwrap();
            }
        });

        let generation = observe_sqlite_source_generation(&db).unwrap();
        assert!(truncated.get());
        assert!(!generation.requires_snapshot());
    }

    #[test]
    fn journal_trailer_truncation_after_tail_sampling_retries() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("trailer-race.db");
        fs::write(&db, b"SQLite format 3\0").unwrap();
        let journal = sidecar_path(&db, "-journal");
        let super_journal = temp.path().join("trailer-race.db-mj");
        fs::write(&super_journal, b"active").unwrap();
        let mut bytes = real_hot_journal_bytes(512);
        append_super_journal_trailer(&mut bytes, b"trailer-race.db-mj");
        fs::write(&journal, bytes).unwrap();
        let truncated = Rc::new(Cell::new(false));
        let truncated_for_hook = Rc::clone(&truncated);
        let journal_for_hook = journal.clone();
        let _hook = install_sqlite_observation_test_hook(move |path, phase| {
            if path == journal_for_hook
                && phase == SqliteObservationTestPhase::BeforeJournalTrailerRead
                && !truncated_for_hook.replace(true)
            {
                let len = path.metadata().unwrap().len();
                fs::OpenOptions::new()
                    .write(true)
                    .open(path)
                    .unwrap()
                    .set_len(len - 8)
                    .unwrap();
            }
        });

        let generation = observe_sqlite_source_generation(&db).unwrap();
        assert!(truncated.get());
        assert!(generation.requires_snapshot());
        assert!(generation.deferred_reason().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn transient_symlink_before_atomic_open_retries() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("source.db");
        fs::write(&db, b"SQLite format 3\0").unwrap();
        let held = temp.path().join("held.db");
        let outside = temp.path().join("outside.db");
        fs::write(&outside, b"outside").unwrap();
        let state = Rc::new(Cell::new(0_u8));
        let state_for_hook = Rc::clone(&state);
        let db_for_hook = db.clone();
        let _hook = install_sqlite_observation_test_hook(move |path, phase| {
            if path != db_for_hook || phase != SqliteObservationTestPhase::BeforeOpen {
                return;
            }
            match state_for_hook.get() {
                0 => {
                    fs::rename(path, &held).unwrap();
                    symlink(&outside, path).unwrap();
                    state_for_hook.set(1);
                }
                1 => {
                    fs::remove_file(path).unwrap();
                    fs::rename(&held, path).unwrap();
                    state_for_hook.set(2);
                }
                _ => {}
            }
        });

        let generation = observe_sqlite_source_generation(&db).unwrap();
        assert_eq!(state.get(), 2);
        assert_eq!(generation.main().len(), 16);
    }

    #[test]
    fn transient_required_main_delete_create_gap_retries() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("replace.db");
        let bytes = b"SQLite format 3\0".to_vec();
        fs::write(&db, &bytes).unwrap();
        let state = Rc::new(Cell::new(0_u8));
        let state_for_hook = Rc::clone(&state);
        let db_for_hook = db.clone();
        let _hook = install_sqlite_observation_test_hook(move |path, phase| {
            if path != db_for_hook || phase != SqliteObservationTestPhase::BeforeOpen {
                return;
            }
            match state_for_hook.get() {
                0 => {
                    fs::remove_file(path).unwrap();
                    state_for_hook.set(1);
                }
                1 => {
                    fs::write(path, &bytes).unwrap();
                    state_for_hook.set(2);
                }
                _ => {}
            }
        });

        let generation = observe_sqlite_source_generation(&db).unwrap();
        assert_eq!(state.get(), 2);
        assert_eq!(generation.main().len(), 16);
    }

    #[test]
    fn stable_missing_required_main_preserves_not_found() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("missing.db");
        let opens = Rc::new(Cell::new(0_usize));
        let opens_for_hook = Rc::clone(&opens);
        let _hook = install_sqlite_observation_test_hook(move |path, phase| {
            if path == db && phase == SqliteObservationTestPhase::BeforeOpen {
                opens_for_hook.set(opens_for_hook.get() + 1);
            }
        });

        let error =
            observe_sqlite_source_generation(temp.path().join("missing.db").as_path()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(opens.get(), SQLITE_GENERATION_MAX_ATTEMPTS);
    }

    #[cfg(unix)]
    #[test]
    fn stable_source_and_parent_symlinks_are_rejected() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        fs::create_dir(&real).unwrap();
        let db = real.join("source.db");
        fs::write(&db, b"SQLite format 3\0").unwrap();
        let file_link = temp.path().join("file-link.db");
        symlink(&db, &file_link).unwrap();
        let error = observe_sqlite_source_generation(&file_link).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

        let parent_link = temp.path().join("parent-link");
        symlink(&real, &parent_link).unwrap();
        let error = observe_sqlite_source_generation(&parent_link.join("source.db")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[cfg(windows)]
    #[test]
    fn stable_windows_leaf_and_parent_junctions_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        fs::create_dir(&real).unwrap();
        let db = real.join("source.db");
        fs::write(&db, b"SQLite format 3\0").unwrap();

        let junction = temp.path().join("junction");
        create_windows_junction(&junction, &real);
        let error = observe_sqlite_source_generation(&junction).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        let error = observe_sqlite_source_generation(&junction.join("source.db")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        fs::remove_dir(&junction).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_component_walk_blocks_parent_swap() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        let db = source.join("source.db");
        fs::write(&db, b"inside").unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("source.db"), b"outside").unwrap();
        let held = temp.path().join("held-source");
        let blocked = Rc::new(Cell::new(false));
        let swapped = Rc::new(Cell::new(false));
        let blocked_for_hook = Rc::clone(&blocked);
        let swapped_for_hook = Rc::clone(&swapped);
        let source_for_hook = source.clone();
        let held_for_hook = held.clone();
        let outside_for_hook = outside.clone();
        let _hook = install_sqlite_observation_test_hook(move |path, phase| {
            if phase == SqliteObservationTestPhase::AfterParentOpen
                && path == source_for_hook
                && !swapped_for_hook.replace(true)
            {
                match fs::rename(path, &held_for_hook) {
                    Ok(()) => create_windows_junction(path, &outside_for_hook),
                    Err(error)
                        if error.kind() == io::ErrorKind::PermissionDenied
                            || matches!(error.raw_os_error(), Some(5 | 32)) =>
                    {
                        swapped_for_hook.set(false);
                        blocked_for_hook.set(true);
                    }
                    Err(error) => panic!("unexpected parent swap failure: {error}"),
                }
            }
        });

        let mut opened = open_source_file_no_follow(&db).unwrap();
        let mut contents = String::new();
        opened.file.read_to_string(&mut contents).unwrap();
        assert_eq!(contents, "inside");
        assert!(blocked.get());
        drop(opened);
        if swapped.get() {
            fs::remove_dir(&source).unwrap();
            fs::rename(&held, &source).unwrap();
        }
    }

    #[test]
    fn wal_page_size_one_is_invalid() {
        let fixture = real_wal_fixture(512);
        let db = fixture.temp.path().join("page-size-one.db");
        fs::copy(&fixture.db, &db).unwrap();
        let mut wal = fs::read(sidecar_path(&fixture.db, "-wal")).unwrap();
        wal[8..12].copy_from_slice(&1_u32.to_be_bytes());
        let order = match be_u32(&wal[0..4]) {
            0x377f_0682 => WalChecksumOrder::LittleEndian,
            0x377f_0683 => WalChecksumOrder::BigEndian,
            _ => unreachable!(),
        };
        let checksum = wal_checksum(order, &wal[..24], [0, 0]);
        wal[24..28].copy_from_slice(&checksum[0].to_be_bytes());
        wal[28..32].copy_from_slice(&checksum[1].to_be_bytes());
        fs::write(sidecar_path(&db, "-wal"), wal).unwrap();

        assert!(!observe_sqlite_source_generation(&db)
            .unwrap()
            .requires_snapshot());
    }

    #[test]
    fn wal_valid_prefix_ceiling_is_retryable() {
        let error =
            wal_frame_end_within_snapshot_ceiling(SQLITE_SNAPSHOT_MAX_BYTES, 1).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(error.to_string(), WAL_RESOURCE_REASON);
    }

    #[test]
    fn wal_restart_ignores_stale_physical_frames_after_the_valid_prefix() {
        let fixture = real_wal_fixture(512);
        fixture
            .writer
            .execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE extra (id INTEGER PRIMARY KEY, value BLOB);
                 INSERT INTO extra(value) VALUES (zeroblob(262144));
                 COMMIT;",
            )
            .unwrap();
        let wal_path = sidecar_path(&fixture.db, "-wal");
        let old_wal = fs::read(&wal_path).unwrap();
        fixture
            .writer
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
            .unwrap();
        fixture
            .writer
            .execute("UPDATE entries SET value = 'sigma' WHERE id = 1", [])
            .unwrap();
        let restarted_wal = fs::read(&wal_path).unwrap();
        assert!(restarted_wal.len() < old_wal.len());
        let mut reused_wal = restarted_wal.clone();
        reused_wal.extend_from_slice(&old_wal[restarted_wal.len()..]);
        fs::write(&wal_path, reused_wal).unwrap();

        let generation = observe_sqlite_source_generation(&fixture.db).unwrap();
        let wal = generation.wal.as_ref().unwrap();
        assert!(generation.requires_snapshot());
        assert_eq!(wal.len(), old_wal.len() as u64);
        assert_eq!(wal.snapshot_len(), restarted_wal.len() as u64);
        assert!(wal.snapshot_len() < wal.len());
    }

    #[test]
    fn rollback_journal_modes_leave_no_hot_generation_after_commit() {
        for mode in ["DELETE", "TRUNCATE", "PERSIST"] {
            let temp = tempfile::tempdir().unwrap();
            let db = temp.path().join(format!("{mode}.db"));
            let conn = Connection::open(&db).unwrap();
            let actual: String = conn
                .query_row(&format!("PRAGMA journal_mode = {mode}"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(actual.to_uppercase(), mode);
            conn.execute_batch(
                "CREATE TABLE entries (id INTEGER PRIMARY KEY, value TEXT);
                 INSERT INTO entries VALUES (1, 'committed');",
            )
            .unwrap();

            let generation = observe_sqlite_source_generation(&db).unwrap();
            assert!(!generation.requires_snapshot(), "journal mode {mode}");
            assert!(
                generation.deferred_reason().is_none(),
                "journal mode {mode}"
            );
        }
    }

    #[test]
    fn super_journal_presence_controls_hot_child_journal_state() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("attached-main.db");
        fs::write(&db, b"SQLite format 3\0").unwrap();
        let journal = sidecar_path(&db, "-journal");
        let super_journal_name = b"attached-main.db-mj H8a1";
        let super_journal = temp.path().join("attached-main.db-mj H8a1");
        fs::write(&super_journal, b"active multi-database commit").unwrap();
        let mut bytes = real_hot_journal_bytes(512);
        append_super_journal_trailer(&mut bytes, super_journal_name);
        fs::write(&journal, bytes).unwrap();

        let present = observe_sqlite_source_generation(&db).unwrap();
        assert!(!present.requires_snapshot());
        assert_eq!(present.deferred_reason(), Some(SUPER_JOURNAL_REASON));

        fs::remove_file(super_journal).unwrap();
        let missing = observe_sqlite_source_generation(&db).unwrap();
        assert!(!missing.requires_snapshot());
        assert!(missing.deferred_reason().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn super_journal_uses_non_utf8_native_relative_path_without_loss() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};

        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("attached-main.db");
        fs::write(&db, b"SQLite format 3\0").unwrap();
        let journal = sidecar_path(&db, "-journal");
        let name = b"attached-main.db-mj-\x80";
        fs::write(
            temp.path().join(OsString::from_vec(name.to_vec())),
            b"active",
        )
        .unwrap();
        let mut bytes = real_hot_journal_bytes(512);
        append_super_journal_trailer(&mut bytes, name);
        fs::write(journal, bytes).unwrap();

        let generation = observe_sqlite_source_generation(&db).unwrap();
        assert_eq!(generation.deferred_reason(), Some(SUPER_JOURNAL_REASON));
    }

    #[test]
    fn same_stat_checkpointed_update_changes_main_file_identity() {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join("same-stat.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "PRAGMA page_size = 4096;
             CREATE TABLE entries (id INTEGER PRIMARY KEY, value TEXT);
             INSERT INTO entries VALUES (1, 'alpha');
             PRAGMA journal_mode = WAL;
             PRAGMA wal_autocheckpoint = 0;
             PRAGMA wal_checkpoint(TRUNCATE);",
        )
        .unwrap();
        let before_metadata = fs::metadata(&db).unwrap();
        let before_modified = before_metadata.modified().unwrap();
        let before_header = fs::read(&db).unwrap()[..SQLITE_HEADER_BYTES].to_vec();
        let before = observe_sqlite_source_generation(&db).unwrap();
        thread::sleep(Duration::from_millis(2));

        conn.execute("UPDATE entries SET value = 'omega' WHERE id = 1", [])
            .unwrap();
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
            .unwrap();
        File::options()
            .write(true)
            .open(&db)
            .unwrap()
            .set_times(FileTimes::new().set_modified(before_modified))
            .unwrap();

        let after_metadata = fs::metadata(&db).unwrap();
        assert_eq!(after_metadata.len(), before_metadata.len());
        assert_eq!(after_metadata.modified().unwrap(), before_modified);
        assert_eq!(
            &fs::read(&db).unwrap()[..SQLITE_HEADER_BYTES],
            before_header
        );
        let after = observe_sqlite_source_generation(&db).unwrap();
        assert_ne!(before.main().sentinel(), after.main().sentinel());
    }

    struct WalFixture {
        temp: tempfile::TempDir,
        db: PathBuf,
        writer: Connection,
    }

    fn real_wal_fixture(page_size: u32) -> WalFixture {
        let temp = tempfile::tempdir().unwrap();
        let db = temp.path().join(format!("wal-{page_size}.db"));
        let writer = Connection::open(&db).unwrap();
        writer
            .execute_batch(&format!(
                "PRAGMA page_size = {page_size};
                 VACUUM;
                 CREATE TABLE entries (id INTEGER PRIMARY KEY, value TEXT);
                 INSERT INTO entries VALUES (1, 'alpha');
                 PRAGMA journal_mode = WAL;
                 PRAGMA wal_autocheckpoint = 0;
                 PRAGMA wal_checkpoint(TRUNCATE);"
            ))
            .unwrap();
        writer
            .execute("UPDATE entries SET value = 'omega' WHERE id = 1", [])
            .unwrap();
        assert!(sidecar_path(&db, "-wal").is_file());
        WalFixture { temp, db, writer }
    }

    fn rewrite_wal_checksum_order(bytes: &mut [u8], order: WalChecksumOrder) {
        let magic = match order {
            WalChecksumOrder::LittleEndian => 0x377f_0682_u32,
            WalChecksumOrder::BigEndian => 0x377f_0683_u32,
        };
        bytes[0..4].copy_from_slice(&magic.to_be_bytes());
        let mut checksum = wal_checksum(order, &bytes[..24], [0, 0]);
        bytes[24..28].copy_from_slice(&checksum[0].to_be_bytes());
        bytes[28..32].copy_from_slice(&checksum[1].to_be_bytes());
        let page_size = be_u32(&bytes[8..12]) as usize;
        let frame_size = WAL_FRAME_HEADER_BYTES + page_size;
        for frame in bytes[WAL_HEADER_BYTES..].chunks_exact_mut(frame_size) {
            checksum = wal_checksum(order, &frame[..8], checksum);
            checksum = wal_checksum(order, &frame[WAL_FRAME_HEADER_BYTES..], checksum);
            frame[16..20].copy_from_slice(&checksum[0].to_be_bytes());
            frame[20..24].copy_from_slice(&checksum[1].to_be_bytes());
        }
    }

    fn rewrite_wal_format_version(bytes: &mut [u8], version: u32) {
        bytes[4..8].copy_from_slice(&version.to_be_bytes());
        let order = match be_u32(&bytes[0..4]) {
            0x377f_0682 => WalChecksumOrder::LittleEndian,
            0x377f_0683 => WalChecksumOrder::BigEndian,
            magic => panic!("unexpected SQLite WAL magic {magic:#x}"),
        };
        let checksum = wal_checksum(order, &bytes[..24], [0, 0]);
        bytes[24..28].copy_from_slice(&checksum[0].to_be_bytes());
        bytes[28..32].copy_from_slice(&checksum[1].to_be_bytes());
    }

    fn real_hot_journal_bytes(page_size: u32) -> Vec<u8> {
        let sector_size = 512_u32;
        let mut bytes = vec![0_u8; sector_size as usize + page_size as usize + 8];
        bytes[..8].copy_from_slice(&JOURNAL_MAGIC);
        bytes[8..12].copy_from_slice(&1_u32.to_be_bytes());
        bytes[12..16].copy_from_slice(&0x1234_5678_u32.to_be_bytes());
        bytes[16..20].copy_from_slice(&1_u32.to_be_bytes());
        bytes[20..24].copy_from_slice(&sector_size.to_be_bytes());
        bytes[24..28].copy_from_slice(&page_size.to_be_bytes());
        bytes[sector_size as usize..sector_size as usize + 4].copy_from_slice(&1_u32.to_be_bytes());
        bytes
    }

    fn append_super_journal_trailer(journal: &mut Vec<u8>, name: &[u8]) {
        journal.extend_from_slice(&1_048_577_u32.to_be_bytes());
        journal.extend_from_slice(name);
        journal.extend_from_slice(&(name.len() as u32).to_be_bytes());
        let checksum = name.iter().fold(0_u32, |sum, byte| {
            sum.wrapping_add((*byte as i8 as i32) as u32)
        });
        journal.extend_from_slice(&checksum.to_be_bytes());
        journal.extend_from_slice(&JOURNAL_MAGIC);
    }

    #[cfg(windows)]
    fn create_windows_junction(link: &Path, target: &Path) {
        let status = std::process::Command::new("cmd")
            .args(["/D", "/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .status()
            .unwrap();
        assert!(status.success(), "failed to create a Windows junction");
    }
}
