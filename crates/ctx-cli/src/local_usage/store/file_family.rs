use std::{
    ffi::OsString,
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    ptr::NonNull,
    time::SystemTime,
};

use ctx_history_core::platform_security::{
    restrict_private_file_handle, verify_private_directory, verify_private_file,
    verify_private_file_handle,
};
use rusqlite::{serialize::OwnedData, Connection, DatabaseName, OpenFlags};

#[cfg(test)]
use super::usage_path;
use super::{verify_schema, UsageStoreError, MAX_DATABASE_BYTES, PAGE_SIZE_BYTES};

const MAX_FAMILY_BYTES: u64 = 8 * 1024 * 1024;

pub(super) fn protect_sqlite_files(path: &Path) -> Result<(), UsageStoreError> {
    protect_sqlite_member(path)?;
    for suffix in ["-wal", "-shm"] {
        let auxiliary = auxiliary_path(path, suffix);
        match auxiliary.symlink_metadata() {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                protect_sqlite_member(&auxiliary)?;
            }
            Ok(_) => return Err(UsageStoreError::SchemaIdentity),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub(super) fn preflight_auxiliaries(
    path: &Path,
    database_exists: bool,
) -> Result<(), UsageStoreError> {
    for suffix in ["-wal", "-shm"] {
        let auxiliary = auxiliary_path(path, suffix);
        match auxiliary.symlink_metadata() {
            Ok(_) if !database_exists => return Err(UsageStoreError::SchemaIdentity),
            Ok(_) => verify_private_file_and_owner(&auxiliary)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn auxiliary_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = OsString::from(path.as_os_str());
    value.push(suffix);
    PathBuf::from(value)
}

struct GuardedMember {
    file: File,
    len: u64,
    modified: Option<SystemTime>,
}

impl GuardedMember {
    fn from_file(file: File) -> Result<Self, UsageStoreError> {
        verify_private_file_handle(&file)?;
        verify_file_owner(&file)?;
        verify_single_link(&file)?;
        let metadata = file.metadata()?;
        Ok(Self {
            file,
            len: metadata.len(),
            modified: metadata.modified().ok(),
        })
    }

    fn recheck(&self, path: &Path, unchanged: bool) -> Result<(), UsageStoreError> {
        verify_same_file(path, &self.file)?;
        verify_private_file_handle(&self.file)?;
        verify_file_owner(&self.file)?;
        verify_single_link(&self.file)?;
        if unchanged {
            let metadata = self.file.metadata()?;
            if metadata.len() != self.len || metadata.modified().ok() != self.modified {
                return Err(UsageStoreError::UnsafeReadState);
            }
        }
        Ok(())
    }
}

pub(super) struct FamilyGuard {
    main: GuardedMember,
    wal: Option<GuardedMember>,
    shm: Option<GuardedMember>,
}

impl FamilyGuard {
    pub(super) fn main_only(file: File) -> Result<Self, UsageStoreError> {
        Ok(Self {
            main: GuardedMember::from_file(file)?,
            wal: None,
            shm: None,
        })
    }

    pub(super) fn main_file(&self) -> &File {
        &self.main.file
    }

    pub(super) fn recheck(&self, path: &Path) -> Result<(), UsageStoreError> {
        self.recheck_members(path, false)
    }

    pub(super) fn recheck_unchanged(&self, path: &Path) -> Result<(), UsageStoreError> {
        self.recheck_members(path, true)
    }

    fn recheck_members(&self, path: &Path, unchanged: bool) -> Result<(), UsageStoreError> {
        self.main.recheck(path, unchanged)?;
        recheck_optional_member(self.wal.as_ref(), &auxiliary_path(path, "-wal"), unchanged)?;
        recheck_optional_member(self.shm.as_ref(), &auxiliary_path(path, "-shm"), unchanged)?;
        verify_family_size(path)
    }

    pub(super) fn has_nonempty_auxiliary(&self) -> Result<bool, UsageStoreError> {
        Ok(self.wal.as_ref().is_some_and(|member| {
            member
                .file
                .metadata()
                .map_or(true, |metadata| metadata.len() > 0)
        }) || self.shm.as_ref().is_some_and(|member| {
            member
                .file
                .metadata()
                .map_or(true, |metadata| metadata.len() > 0)
        }))
    }
}

fn recheck_optional_member(
    guarded: Option<&GuardedMember>,
    path: &Path,
    unchanged: bool,
) -> Result<(), UsageStoreError> {
    match (guarded, path.symlink_metadata()) {
        (Some(member), Ok(_)) => member.recheck(path, unchanged),
        (None, Err(error)) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        (Some(_), Err(error)) if error.kind() == io::ErrorKind::NotFound => {
            Err(UsageStoreError::UnsafeReadState)
        }
        (None, Ok(_)) => Err(UsageStoreError::UnsafeReadState),
        (_, Err(error)) => Err(error.into()),
    }
}

pub(super) fn preflight_existing_family(
    path: &Path,
    read_only: bool,
) -> Result<FamilyGuard, UsageStoreError> {
    verify_private_file_and_owner(path)?;
    verify_family_size(path)?;
    let main = GuardedMember::from_file(open_nofollow(path, read_only)?)?;
    verify_same_file(path, &main.file)?;
    let wal = preflight_optional_member(&auxiliary_path(path, "-wal"))?;
    let shm = preflight_optional_member(&auxiliary_path(path, "-shm"))?;
    let guard = FamilyGuard { main, wal, shm };
    guard.recheck(path)?;
    Ok(guard)
}

fn preflight_optional_member(path: &Path) -> Result<Option<GuardedMember>, UsageStoreError> {
    match path.symlink_metadata() {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            verify_private_file_and_owner(path)?;
            let member = GuardedMember::from_file(open_nofollow(path, true)?)?;
            verify_same_file(path, &member.file)?;
            Ok(Some(member))
        }
        Ok(_) => Err(UsageStoreError::SchemaIdentity),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn capture_checkpointed_image(
    path: &Path,
    guard: &FamilyGuard,
    between_reads: impl FnOnce(),
) -> Result<Vec<u8>, UsageStoreError> {
    guard.recheck_unchanged(path)?;
    let first = read_bounded_image(&guard.main.file)?;
    guard.recheck_unchanged(path)?;
    between_reads();
    guard.recheck_unchanged(path)?;
    let second = read_bounded_image(&guard.main.file)?;
    guard.recheck_unchanged(path)?;
    if first != second {
        return Err(UsageStoreError::UnsafeReadState);
    }
    normalize_checkpointed_header(first)
}

fn read_bounded_image(file: &File) -> Result<Vec<u8>, UsageStoreError> {
    let expected_size =
        usize::try_from(file.metadata()?.len()).map_err(|_| UsageStoreError::GrowthLimit)?;
    if expected_size > usize::try_from(MAX_DATABASE_BYTES).unwrap_or(usize::MAX) {
        return Err(UsageStoreError::GrowthLimit);
    }
    let mut reader = file.try_clone()?;
    reader.seek(SeekFrom::Start(0))?;
    let limit = u64::try_from(MAX_DATABASE_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut image = Vec::new();
    reader.take(limit).read_to_end(&mut image)?;
    if image.len() > usize::try_from(MAX_DATABASE_BYTES).unwrap_or(usize::MAX) {
        return Err(UsageStoreError::GrowthLimit);
    }
    if image.len() != expected_size {
        return Err(UsageStoreError::UnsafeReadState);
    }
    Ok(image)
}

fn normalize_checkpointed_header(mut image: Vec<u8>) -> Result<Vec<u8>, UsageStoreError> {
    const SQLITE_HEADER: &[u8; 16] = b"SQLite format 3\0";
    if image.len() < 100
        || image.get(..SQLITE_HEADER.len()) != Some(SQLITE_HEADER.as_slice())
        || !image
            .len()
            .is_multiple_of(usize::try_from(PAGE_SIZE_BYTES).unwrap_or(usize::MAX))
    {
        return Err(UsageStoreError::SchemaIdentity);
    }
    let header_page_size = u16::from_be_bytes([image[16], image[17]]);
    if i64::from(header_page_size) != PAGE_SIZE_BYTES
        || !matches!(image[18], 1 | 2)
        || !matches!(image[19], 1 | 2)
        || image[18] != image[19]
    {
        return Err(UsageStoreError::SchemaIdentity);
    }
    // The bytes are now detached from the source. WAL-mode databases use 2 in
    // these header slots; an in-memory, read-only deserialize needs the
    // rollback-journal marker and never observes source auxiliaries.
    image[18] = 1;
    image[19] = 1;
    Ok(image)
}

pub(super) fn deserialize_read_only(image: Vec<u8>) -> Result<Connection, UsageStoreError> {
    let size = image.len();
    let allocation = unsafe { rusqlite::ffi::sqlite3_malloc64(size as u64) }.cast::<u8>();
    let allocation = NonNull::new(allocation).ok_or(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_NOMEM),
        None,
    ))?;
    unsafe {
        std::ptr::copy_nonoverlapping(image.as_ptr(), allocation.as_ptr(), size);
    }
    let data = unsafe { OwnedData::from_raw_nonnull(allocation, size) };
    let mut conn = Connection::open_in_memory_with_flags(
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    conn.deserialize(DatabaseName::Main, data, true)?;
    verify_schema(&conn)?;
    Ok(conn)
}

#[cfg(test)]
pub(in crate::local_usage) fn capture_with_between_reads_for_test(
    data_root: &Path,
    between_reads: impl FnOnce(),
) -> Result<(), UsageStoreError> {
    let path = usage_path(data_root);
    let guard = preflight_existing_family(&path, true)?;
    if guard.has_nonempty_auxiliary()? {
        return Err(UsageStoreError::UnsafeReadState);
    }
    let image = capture_checkpointed_image(&path, &guard, between_reads)?;
    drop(deserialize_read_only(image)?);
    guard.recheck_unchanged(&path)
}

fn protect_sqlite_member(path: &Path) -> Result<(), UsageStoreError> {
    let file = open_nofollow(path, false)?;
    restrict_private_file_handle(&file)?;
    verify_file_owner(&file)?;
    verify_same_file(path, &file)
}

pub(super) fn open_nofollow(path: &Path, read_only: bool) -> Result<File, UsageStoreError> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(!read_only);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };

        // Omitting FILE_SHARE_DELETE keeps the admitted pathname bound to the
        // retained handle while SQLite reopens it by name.
        options
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    Ok(options.open(path)?)
}

pub(super) fn verify_private_directory_and_owner(path: &Path) -> Result<(), UsageStoreError> {
    verify_private_directory(path)?;
    verify_metadata_owner(&path.symlink_metadata()?)
}

fn verify_private_file_and_owner(path: &Path) -> Result<(), UsageStoreError> {
    verify_private_file(path)?;
    verify_metadata_owner(&path.symlink_metadata()?)
}

pub(super) fn verify_file_owner(file: &File) -> Result<(), UsageStoreError> {
    verify_metadata_owner(&file.metadata()?)
}

pub(super) fn verify_metadata_owner(metadata: &fs::Metadata) -> Result<(), UsageStoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err(UsageStoreError::SchemaIdentity);
        }
    }
    let _ = metadata;
    Ok(())
}

pub(super) fn verify_same_file(path: &Path, file: &File) -> Result<(), UsageStoreError> {
    drop(reopen_same_file(path, file)?);
    Ok(())
}

pub(super) fn reopen_same_file(path: &Path, retained: &File) -> Result<File, UsageStoreError> {
    let path_metadata = path.symlink_metadata()?;
    if path_metadata.file_type().is_symlink() || !path_metadata.is_file() {
        return Err(UsageStoreError::SchemaIdentity);
    }
    let named = open_nofollow(path, true)?;
    let retained_metadata = retained.metadata()?;
    let named_metadata = named.metadata()?;
    if !retained_metadata.is_file() || !named_metadata.is_file() {
        return Err(UsageStoreError::SchemaIdentity);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if retained_metadata.dev() != named_metadata.dev()
            || retained_metadata.ino() != named_metadata.ino()
        {
            return Err(UsageStoreError::SchemaIdentity);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

        if retained_metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || named_metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        {
            return Err(UsageStoreError::SchemaIdentity);
        }
        // `same_file::Handle` keeps each clone live while comparing the
        // GetFileInformationByHandle volume serial and file index. Rust's
        // equivalent MetadataExt accessors are still unstable.
        let retained_identity = same_file::Handle::from_file(retained.try_clone()?)?;
        let named_identity = same_file::Handle::from_file(named.try_clone()?)?;
        if retained_identity != named_identity {
            return Err(UsageStoreError::SchemaIdentity);
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (retained_metadata, named_metadata);
        return Err(UsageStoreError::SchemaIdentity);
    }
    Ok(named)
}

#[cfg(all(test, windows))]
pub(in crate::local_usage) fn verify_same_file_for_test(
    path: &Path,
    retained: &File,
) -> Result<(), UsageStoreError> {
    verify_same_file(path, retained)
}

#[cfg(all(test, windows))]
pub(in crate::local_usage) fn assert_single_link_for_test(
    file: &File,
) -> Result<(), UsageStoreError> {
    verify_single_link(file)
}

pub(super) fn verify_single_link(file: &File) -> Result<(), UsageStoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if file.metadata()?.nlink() != 1 {
            return Err(UsageStoreError::SchemaIdentity);
        }
    }
    #[cfg(windows)]
    {
        use std::{mem::MaybeUninit, os::windows::io::AsRawHandle as _};
        use windows_sys::Win32::{
            Foundation::HANDLE,
            Storage::FileSystem::{GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION},
        };

        let mut information = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
        // SAFETY: `file` owns a live handle and `information` is a valid out pointer.
        if unsafe {
            GetFileInformationByHandle(file.as_raw_handle() as HANDLE, information.as_mut_ptr())
        } == 0
        {
            return Err(io::Error::last_os_error().into());
        }
        // SAFETY: the successful call initialized the structure.
        let information = unsafe { information.assume_init() };
        if information.nNumberOfLinks != 1 {
            return Err(UsageStoreError::SchemaIdentity);
        }
    }
    Ok(())
}

fn verify_family_size(path: &Path) -> Result<(), UsageStoreError> {
    let mut bytes = 0_u64;
    for member in [
        path.to_path_buf(),
        auxiliary_path(path, "-wal"),
        auxiliary_path(path, "-shm"),
    ] {
        match member.symlink_metadata() {
            Ok(metadata) => {
                bytes = bytes
                    .checked_add(metadata.len())
                    .ok_or(UsageStoreError::GrowthLimit)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    if bytes >= MAX_FAMILY_BYTES {
        return Err(UsageStoreError::GrowthLimit);
    }
    Ok(())
}
