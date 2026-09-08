use std::{
    ffi::OsStr,
    fs::{self, File, Metadata, OpenOptions},
    io,
    mem::MaybeUninit,
    os::windows::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
        io::{AsRawHandle, FromRawHandle, RawHandle},
    },
    path::Path,
};

use windows_sys::{
    Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem::{
            NtCreateFile, FILE_CREATE, FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN,
            FILE_OPEN_NO_RECALL, FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT,
        },
    },
    Win32::{
        Foundation::{RtlNtStatusToDosError, HANDLE, OBJ_CASE_INSENSITIVE, UNICODE_STRING},
        Storage::FileSystem::{
            FileDispositionInfo, FileDispositionInfoEx, GetDiskFreeSpaceExW,
            GetFileInformationByHandle, SetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
            DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_OFFLINE,
            FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS, FILE_ATTRIBUTE_RECALL_ON_OPEN,
            FILE_ATTRIBUTE_REPARSE_POINT, FILE_DISPOSITION_FLAG_DELETE,
            FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
            FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX, FILE_FLAG_BACKUP_SEMANTICS,
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
            FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE, FILE_WRITE_ATTRIBUTES, READ_CONTROL, SYNCHRONIZE, WRITE_DAC,
        },
        System::IO::IO_STATUS_BLOCK,
    },
};

use super::{entry_kind, require_regular, ObjectIdentity};
use crate::{GenerationError as IndexError, Result};

const DIRECTORY_SHARE_MODE: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
const REGULAR_FILE_SHARE_MODE: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
const DESTINATION_FILE_SHARE_MODE: u32 = FILE_SHARE_READ;

pub(super) fn open_directory_path(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        // Root topology guards authenticate the route but never delete the
        // guarded object. Requesting DELETE here makes this handle
        // incompatible with the no-delete-sharing read-root capability used
        // by generation leases during publication. Ordinary child topology
        // opens below follow the same rule; the discardable destination path
        // retains DELETE because CandidateGuard::discard owns it.
        .access_mode(FILE_GENERIC_READ)
        .share_mode(DIRECTORY_SHARE_MODE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

pub(super) fn open_directory_at(
    parent: &File,
    _parent_path: &Path,
    name: &Path,
) -> io::Result<File> {
    nt_open_at(
        parent,
        name,
        FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
        FILE_OPEN,
        FILE_ATTRIBUTE_DIRECTORY,
        FILE_DIRECTORY_FILE,
    )
}

pub(super) fn open_discardable_directory_at(
    parent: &File,
    _parent_path: &Path,
    name: &Path,
) -> io::Result<File> {
    nt_open_at(
        parent,
        name,
        FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES | DELETE | SYNCHRONIZE,
        FILE_OPEN,
        FILE_ATTRIBUTE_DIRECTORY,
        FILE_DIRECTORY_FILE,
    )
}

pub(super) fn create_directory_at(
    parent: &File,
    _parent_path: &Path,
    name: &Path,
) -> io::Result<File> {
    nt_open_at(
        parent,
        name,
        FILE_LIST_DIRECTORY
            | FILE_READ_ATTRIBUTES
            | FILE_WRITE_ATTRIBUTES
            | READ_CONTROL
            | WRITE_DAC
            | DELETE
            | SYNCHRONIZE,
        FILE_CREATE,
        FILE_ATTRIBUTE_DIRECTORY,
        FILE_DIRECTORY_FILE,
    )
}

pub(super) fn open_regular_file_at(
    parent: &File,
    _parent_path: &Path,
    name: &Path,
) -> io::Result<File> {
    nt_open_at(
        parent,
        name,
        FILE_GENERIC_READ | SYNCHRONIZE,
        FILE_OPEN,
        FILE_ATTRIBUTE_NORMAL,
        FILE_NON_DIRECTORY_FILE,
    )
}

pub(super) fn create_regular_file_at(
    parent: &File,
    _parent_path: &Path,
    name: &Path,
) -> io::Result<File> {
    nt_open_at(
        parent,
        name,
        FILE_GENERIC_READ | FILE_GENERIC_WRITE | SYNCHRONIZE,
        FILE_CREATE,
        FILE_ATTRIBUTE_NORMAL,
        FILE_NON_DIRECTORY_FILE,
    )
}

fn open_regular_for_delete(parent: &File, name: &Path) -> io::Result<File> {
    nt_open_at(
        parent,
        name,
        FILE_GENERIC_READ | FILE_WRITE_ATTRIBUTES | DELETE | SYNCHRONIZE,
        FILE_OPEN,
        FILE_ATTRIBUTE_NORMAL,
        FILE_NON_DIRECTORY_FILE,
    )
}

fn nt_open_at(
    parent: &File,
    name: &Path,
    access: u32,
    disposition: u32,
    file_attributes: u32,
    kind: u32,
) -> io::Result<File> {
    let mut wide = name.as_os_str().encode_wide().collect::<Vec<_>>();
    let byte_len = wide
        .len()
        .checked_mul(2)
        .and_then(|length| u16::try_from(length).ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path component is too long"))?;
    let mut unicode = UNICODE_STRING {
        Length: byte_len,
        MaximumLength: byte_len,
        Buffer: wide.as_mut_ptr(),
    };
    let object_attributes = OBJECT_ATTRIBUTES {
        Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.as_raw_handle() as HANDLE,
        ObjectName: &mut unicode,
        Attributes: OBJ_CASE_INSENSITIVE,
        SecurityDescriptor: std::ptr::null(),
        SecurityQualityOfService: std::ptr::null(),
    };
    let mut status_block = IO_STATUS_BLOCK::default();
    let mut handle: HANDLE = std::ptr::null_mut();
    let recall_option = if kind == FILE_DIRECTORY_FILE {
        0
    } else {
        FILE_OPEN_NO_RECALL
    };
    let share_mode = if kind == FILE_DIRECTORY_FILE {
        DIRECTORY_SHARE_MODE
    } else if disposition == FILE_CREATE {
        DESTINATION_FILE_SHARE_MODE
    } else {
        REGULAR_FILE_SHARE_MODE
    };
    // SAFETY: all structures and the relative UTF-16 name remain live for
    // the call, and a successful handle is transferred into `File` once.
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            access,
            &object_attributes,
            &mut status_block,
            std::ptr::null(),
            file_attributes,
            share_mode,
            disposition,
            kind | FILE_OPEN_REPARSE_POINT | recall_option | FILE_SYNCHRONOUS_IO_NONALERT,
            std::ptr::null(),
            0,
        )
    };
    if status < 0 {
        // SAFETY: the conversion accepts every NTSTATUS value.
        return Err(io::Error::from_raw_os_error(
            unsafe { RtlNtStatusToDosError(status) } as i32,
        ));
    }
    // SAFETY: `NtCreateFile` returned a newly owned handle.
    Ok(unsafe { File::from_raw_handle(handle as RawHandle) })
}

pub(super) fn directory_entries(
    _file: &File,
    path: &Path,
    maximum: usize,
) -> Result<Vec<std::ffi::OsString>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let actual = entries
            .len()
            .checked_add(1)
            .ok_or(IndexError::CountOverflow)?;
        if actual > maximum {
            return Err(IndexError::CurrentRepublishFileLimit { actual, maximum });
        }
        entries.push(entry.file_name());
    }
    entries.sort();
    Ok(entries)
}

pub(super) fn object_identity(file: &File) -> io::Result<ObjectIdentity> {
    let mut information = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
    // SAFETY: `file` owns a live handle and `information` is a valid out pointer.
    if unsafe {
        GetFileInformationByHandle(file.as_raw_handle() as HANDLE, information.as_mut_ptr())
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful call initialized the structure.
    let information = unsafe { information.assume_init() };
    if information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "republish path is a Windows reparse point",
        ));
    }
    Ok(ObjectIdentity {
        first: u64::from(information.dwVolumeSerialNumber),
        second: (u64::from(information.nFileIndexHigh) << 32)
            | u64::from(information.nFileIndexLow),
    })
}

pub(super) fn permission_identity(metadata: &Metadata) -> bool {
    metadata.permissions().readonly()
}

pub(super) fn is_unsafe_link_or_provider(metadata: &Metadata) -> bool {
    has_unsafe_file_attributes(metadata.file_attributes())
}

fn has_unsafe_file_attributes(attributes: u32) -> bool {
    attributes
        & (FILE_ATTRIBUTE_REPARSE_POINT
            | FILE_ATTRIBUTE_OFFLINE
            | FILE_ATTRIBUTE_RECALL_ON_OPEN
            | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS)
        != 0
}

pub(super) fn is_nofollow_rejection(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::InvalidData
}

pub(super) fn available_bytes(_file: &File, path: &Path) -> io::Result<u64> {
    let wide = wide_path(path.as_os_str())?;
    let mut available = 0_u64;
    // SAFETY: `wide` is NUL-terminated and `available` is a valid out pointer.
    if unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(available)
}

fn wide_path(path: &OsStr) -> io::Result<Vec<u16>> {
    let mut wide = path.encode_wide().collect::<Vec<_>>();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path contains an interior NUL",
        ));
    }
    wide.push(0);
    Ok(wide)
}

pub(super) fn restrict_destination_directory(file: &File) -> io::Result<()> {
    ctx_history_platform::platform_security::restrict_private_directory_handle(file)
}

pub(super) fn sync_directory(_file: &File) -> io::Result<()> {
    // Windows publication uses write-through replacement. Directory-handle
    // flushing is not a reliable durability primitive on supported targets.
    Ok(())
}

pub(super) fn discard_destination(
    _parent: &File,
    _parent_path: &Path,
    _name: &Path,
    destination: &File,
    destination_path: &Path,
) -> io::Result<()> {
    for name in directory_entries(
        destination,
        destination_path,
        super::MAX_REPUBLISH_DIRECTORY_ENTRIES,
    )
    .map_err(|error| io::Error::other(error.to_string()))?
    {
        let file = open_regular_for_delete(destination, Path::new(&name))?;
        let kind =
            entry_kind(&file.metadata()?).map_err(|error| io::Error::other(error.to_string()))?;
        require_regular(kind).map_err(|error| io::Error::other(error.to_string()))?;
        delete_by_handle(&file)?;
    }
    delete_by_handle(destination)
}

fn delete_by_handle(file: &File) -> io::Result<()> {
    let disposition = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_FLAG_DELETE
            | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS
            | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
    };
    // SAFETY: `file` has DELETE access and `disposition` is the exact type
    // required for `FileDispositionInfoEx`.
    if unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle() as HANDLE,
            FileDispositionInfoEx,
            std::ptr::addr_of!(disposition).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
        )
    } != 0
    {
        return Ok(());
    }
    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: this is the documented compatibility form of handle deletion.
    if unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle() as HANDLE,
            FileDispositionInfo,
            std::ptr::addr_of!(disposition).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    } == 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inherited_destination_acl_is_protected_through_retained_handle() -> io::Result<()> {
        use ctx_history_platform::platform_security::{
            ensure_private_directory, verify_private_directory, verify_private_directory_handle,
        };

        let temporary = tempfile::tempdir()?;
        ensure_private_directory(temporary.path())?;
        let parent = open_directory_path(temporary.path())?;
        let generation_name = Path::new("generation-inherited-acl");
        let destination = create_directory_at(&parent, temporary.path(), generation_name)?;

        let inherited_error = verify_private_directory_handle(&destination).unwrap_err();
        assert_eq!(
            inherited_error.kind(),
            io::ErrorKind::PermissionDenied,
            "new inherited directory should initially lack a protected private DACL: {inherited_error}"
        );

        restrict_destination_directory(&destination)?;
        verify_private_directory_handle(&destination)?;
        drop(destination);
        verify_private_directory(&temporary.path().join(generation_name))
    }

    #[test]
    fn proof_capture_handle_blocks_destination_substitution() -> io::Result<()> {
        use std::io::Write as _;

        let temporary = tempfile::tempdir()?;
        let generation_name = Path::new("generation-test");
        fs::create_dir(temporary.path().join(generation_name))?;

        let root = open_directory_path(temporary.path())?;
        let reopened_root = open_directory_path(temporary.path())?;
        assert_eq!(object_identity(&root)?, object_identity(&reopened_root)?);

        let generation = open_directory_at(&root, temporary.path(), generation_name)?;
        let reopened_generation = open_directory_at(&root, temporary.path(), generation_name)?;
        assert_eq!(
            object_identity(&generation)?,
            object_identity(&reopened_generation)?
        );
        let segment_name = Path::new("segment.store");
        let mut segment = create_regular_file_at(&generation, temporary.path(), segment_name)?;
        segment.write_all(b"authenticated")?;
        segment.sync_all()?;
        let reopened_segment = open_regular_file_at(&generation, temporary.path(), segment_name)?;
        assert_eq!(
            object_identity(&segment)?,
            object_identity(&reopened_segment)?
        );
        assert_eq!(DESTINATION_FILE_SHARE_MODE & FILE_SHARE_WRITE, 0);
        assert_eq!(DESTINATION_FILE_SHARE_MODE & FILE_SHARE_DELETE, 0);
        assert_ne!(REGULAR_FILE_SHARE_MODE & FILE_SHARE_DELETE, 0);

        let target = temporary.path().join(generation_name).join(segment_name);
        let writer_error = OpenOptions::new()
            .write(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(&target)
            .unwrap_err();
        assert_eq!(writer_error.raw_os_error(), Some(32));
        let delete_error = OpenOptions::new()
            .access_mode(DELETE)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .open(&target)
            .unwrap_err();
        assert_eq!(delete_error.raw_os_error(), Some(32));
        let replacement = temporary
            .path()
            .join(generation_name)
            .join("replacement.store");
        let mut replacement_file = File::create(&replacement)?;
        replacement_file.write_all(b"substituted!!")?;
        replacement_file.sync_all()?;
        drop(replacement_file);
        let error = crate::durable_atomic_replace_file(&replacement, &target).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(5));
        assert_eq!(fs::read(&target)?, b"authenticated");

        drop(reopened_segment);
        drop(segment);
        fs::remove_file(replacement)?;
        Ok(())
    }

    #[test]
    fn reparse_and_remote_provider_attributes_remain_fail_closed() {
        for attributes in [
            FILE_ATTRIBUTE_REPARSE_POINT,
            FILE_ATTRIBUTE_OFFLINE,
            FILE_ATTRIBUTE_RECALL_ON_OPEN,
            FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS,
        ] {
            assert!(has_unsafe_file_attributes(attributes));
        }
        assert!(!has_unsafe_file_attributes(
            FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_NORMAL
        ));
    }
}
