use std::{
    ffi::{c_void, OsStr, OsString},
    fs::{File, Metadata, OpenOptions},
    io,
    mem::size_of,
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        fs::OpenOptionsExt,
        io::{AsRawHandle, FromRawHandle},
    },
    path::{Component, Path, PathBuf, Prefix},
};

use windows_sys::Win32::Foundation::{
    FreeLibrary, ERROR_CLOUD_FILE_NOT_UNDER_SYNC_ROOT, ERROR_HANDLE_EOF, ERROR_INVALID_FUNCTION,
};
use windows_sys::Win32::Storage::FileSystem::{
    FileAttributeTagInfo, FileBasicInfo, FileIdBothDirectoryInfo, FileIdBothDirectoryRestartInfo,
    FileIdInfo, GetDriveTypeW, GetFileInformationByHandleEx, GetFileType,
    GetFinalPathNameByHandleW, GetVolumeInformationByHandleW, ReadFile, FILE_ATTRIBUTE_OFFLINE,
    FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS, FILE_ATTRIBUTE_RECALL_ON_OPEN,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO, FILE_BASIC_INFO,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_BOTH_DIR_INFO, FILE_ID_INFO,
    FILE_NAME_NORMALIZED, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TYPE_DISK,
    VOLUME_NAME_DOS,
};
use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
};
use windows_sys::Win32::System::IO::{OVERLAPPED, OVERLAPPED_0, OVERLAPPED_0_0};

use sha2::{Digest, Sha256};

use super::{AuthorityOpenError, DirectoryEntryVisitError};

#[path = "windows/access.rs"]
mod access;
use access::ProviderSourceOpenKind;

const DIRECTORY_QUERY_BUFFER_BYTES: usize = 64 * 1024;
const ERROR_NO_MORE_FILES: i32 = 18;
const ERROR_CLOUD_FILE_NOT_UNDER_SYNC_ROOT_HRESULT: i32 =
    win32_error_hresult(ERROR_CLOUD_FILE_NOT_UNDER_SYNC_ROOT);
const ERROR_INVALID_FUNCTION_HRESULT: i32 = win32_error_hresult(ERROR_INVALID_FUNCTION);
const CF_SYNC_ROOT_INFO_BASIC: i32 = 0;

const OBJ_CASE_INSENSITIVE: u32 = 0x0000_0040;
const DRIVE_FIXED: u32 = 3;

const fn win32_error_hresult(error: u32) -> i32 {
    (0x8007_0000_u32 | (error & 0x0000_FFFF)) as i32
}

#[derive(Debug)]
struct WindowsIoContext {
    operation: &'static str,
    source: io::Error,
}

impl std::fmt::Display for WindowsIoContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.operation, self.source)
    }
}

impl std::error::Error for WindowsIoContext {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

fn contextual_windows_io(operation: &'static str, source: io::Error) -> io::Error {
    io::Error::new(source.kind(), WindowsIoContext { operation, source })
}

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtOpenFile(
        file_handle: *mut *mut c_void,
        desired_access: u32,
        object_attributes: *const ObjectAttributes,
        io_status_block: *mut IoStatusBlock,
        share_access: u32,
        open_options: u32,
    ) -> i32;
    fn RtlNtStatusToDosError(status: i32) -> u32;
}

type CfGetSyncRootInfoByHandle = unsafe extern "system" fn(
    file_handle: *mut c_void,
    info_class: i32,
    info_buffer: *mut c_void,
    info_buffer_length: u32,
    returned_length: *mut u32,
) -> i32;

#[repr(C)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *mut u16,
}

#[repr(C)]
struct ObjectAttributes {
    length: u32,
    root_directory: *mut c_void,
    object_name: *mut UnicodeString,
    attributes: u32,
    security_descriptor: *mut c_void,
    security_quality_of_service: *mut c_void,
}

#[repr(C)]
struct IoStatusBlock {
    status_or_pointer: *mut c_void,
    information: usize,
}

#[repr(C)]
#[derive(Default)]
struct CfSyncRootBasicInfo {
    sync_root_file_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ObjectStamp {
    volume_serial_number: u64,
    file_id: [u8; 16],
    creation_time: i64,
    last_write_time: i64,
    change_time: i64,
    length: u64,
    attributes: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FilesystemIdentity {
    volume_serial_number: u64,
    filesystem_name: String,
}

pub(super) enum OpenedPath {
    File {
        file: File,
        metadata: Metadata,
        filesystem: FilesystemIdentity,
    },
    Directory {
        file: File,
        metadata: Metadata,
        filesystem: FilesystemIdentity,
    },
}

pub(super) fn normalize_authority_path(path: &Path) -> PathBuf {
    path.to_path_buf()
}

pub(super) fn open_absolute(path: &Path) -> Result<OpenedPath, AuthorityOpenError> {
    let (drive, names) = qualified_drive_components(path)?;
    let drive_root = format!("{}:\\", char::from(drive));
    let drive_root_path = Path::new(&drive_root);
    let drive_root_wide = drive_root_path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    if unsafe { GetDriveTypeW(drive_root_wide.as_ptr()) } != DRIVE_FIXED {
        return Err(AuthorityOpenError::Rejected(
            "Windows provider source roots require a fixed local drive",
        ));
    }

    let root_kind = if names.is_empty() {
        ProviderSourceOpenKind::Target
    } else {
        ProviderSourceOpenKind::AncestorDirectory
    };
    let root = OpenOptions::new()
        .access_mode(root_kind.desired_access())
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(drive_root_path)
        .map_err(|source| AuthorityOpenError::SystemIo {
            operation: "provider source drive-root open",
            source,
        })?;
    let mut current = classify_opened(root)?;
    verify_root_drive(&current, drive)?;
    let root_filesystem = opened_filesystem(&current).clone();

    let final_component = names.len().saturating_sub(1);
    for (index, name) in names.into_iter().enumerate() {
        let parent = match &current {
            OpenedPath::Directory { file, .. } => file,
            OpenedPath::File { .. } => {
                return Err(AuthorityOpenError::Rejected(
                    "provider source ancestor components must be directories",
                ));
            }
        };
        let kind = if index == final_component {
            ProviderSourceOpenKind::Target
        } else {
            ProviderSourceOpenKind::AncestorDirectory
        };
        current = open_child_with_kind(parent, name, &root_filesystem, kind)?;
    }
    Ok(current)
}

pub(super) fn open_child(
    parent: &File,
    name: &OsStr,
    filesystem: &FilesystemIdentity,
) -> Result<OpenedPath, AuthorityOpenError> {
    open_child_with_kind(parent, name, filesystem, ProviderSourceOpenKind::Target)
}

fn open_child_with_kind(
    parent: &File,
    name: &OsStr,
    filesystem: &FilesystemIdentity,
    kind: ProviderSourceOpenKind,
) -> Result<OpenedPath, AuthorityOpenError> {
    validate_child_name(name)?;
    let file = nt_open_child(parent, name, kind)?;
    let opened = classify_opened(file)?;
    if opened_filesystem(&opened) != filesystem {
        return Err(AuthorityOpenError::Rejected(
            "provider source descendants may not cross filesystem volumes",
        ));
    }
    Ok(opened)
}

pub(super) fn directory_entries(
    directory: &File,
    maximum_entries: usize,
) -> Result<Vec<OsString>, AuthorityOpenError> {
    let mut entries = Vec::new();
    match visit_directory_entries(directory, &mut |name| {
        if entries.len() >= maximum_entries {
            return Err(AuthorityOpenError::Rejected(
                "provider source directory exceeds its bounded entry budget",
            ));
        }
        entries.push(name);
        Ok(())
    }) {
        Ok(()) => {}
        Err(DirectoryEntryVisitError::Authority(error))
        | Err(DirectoryEntryVisitError::Visitor(error)) => return Err(error),
    }
    entries.sort_by(|left, right| comparable_name(left).cmp(&comparable_name(right)));
    entries.dedup_by(|left, right| comparable_name(left) == comparable_name(right));
    Ok(entries)
}

pub(super) fn visit_directory_entries<E>(
    directory: &File,
    visit: &mut impl FnMut(OsString) -> Result<(), E>,
) -> Result<(), DirectoryEntryVisitError<E>> {
    let mut restart = true;
    let mut buffer = vec![0_u64; DIRECTORY_QUERY_BUFFER_BYTES / size_of::<u64>()];
    loop {
        let class = if restart {
            FileIdBothDirectoryRestartInfo
        } else {
            FileIdBothDirectoryInfo
        };
        let result = unsafe {
            GetFileInformationByHandleEx(
                directory.as_raw_handle(),
                class,
                buffer.as_mut_ptr().cast(),
                (buffer.len() * size_of::<u64>()) as u32,
            )
        };
        if result == 0 {
            let cause = io::Error::last_os_error();
            if cause.raw_os_error() == Some(ERROR_NO_MORE_FILES) {
                break;
            }
            return Err(DirectoryEntryVisitError::Authority(
                AuthorityOpenError::SystemIo {
                    operation: "provider source directory enumeration",
                    source: cause,
                },
            ));
        }
        restart = false;
        visit_directory_buffer(&buffer, visit)?;
    }
    Ok(())
}

pub(super) fn object_stamp(file: &File, metadata: &Metadata) -> io::Result<ObjectStamp> {
    let details = handle_details(file)?;
    Ok(ObjectStamp {
        volume_serial_number: details.id.VolumeSerialNumber,
        file_id: details.id.FileId.Identifier,
        creation_time: details.basic.CreationTime,
        last_write_time: details.basic.LastWriteTime,
        change_time: details.basic.ChangeTime,
        length: metadata.len(),
        attributes: details.attributes,
    })
}

pub(super) fn object_fingerprint(stamp: &ObjectStamp) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"ctx.retained-authority.windows-object-v1\0");
    digest.update(stamp.volume_serial_number.to_be_bytes());
    digest.update(stamp.file_id);
    digest.update(stamp.creation_time.to_be_bytes());
    digest.update(stamp.last_write_time.to_be_bytes());
    digest.update(stamp.change_time.to_be_bytes());
    digest.update(stamp.length.to_be_bytes());
    digest.update(stamp.attributes.to_be_bytes());
    digest.finalize().into()
}

pub(super) fn same_object(left: &ObjectStamp, right: &ObjectStamp) -> bool {
    left.volume_serial_number == right.volume_serial_number
        && left.file_id == right.file_id
        && left.creation_time == right.creation_time
}

pub(super) fn object_change_token(stamp: &ObjectStamp) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(super::ORDINARY_FILE_V2_TOKEN_DOMAIN);
    digest.update(b"windows\0");
    digest.update(stamp.volume_serial_number.to_le_bytes());
    digest.update(stamp.file_id);
    digest.update(stamp.change_time.to_le_bytes());
    digest.update(stamp.last_write_time.to_le_bytes());
    digest.update(stamp.length.to_le_bytes());
    digest.finalize().into()
}

pub(super) fn retained_file_identity(
    file: &File,
    metadata: &Metadata,
    version: super::RetainedFileIdentityVersion,
) -> Result<Option<([u8; 32], [u8; 32])>, AuthorityOpenError> {
    let mut basic = FILE_BASIC_INFO::default();
    query_handle_info(
        file,
        FileBasicInfo,
        &mut basic,
        "GetFileInformationByHandleEx(FileBasicInfo)",
    )
    .map_err(|source| AuthorityOpenError::SystemIo {
        operation: "provider source retained-identity query",
        source,
    })?;
    if basic.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(AuthorityOpenError::Rejected(
            "reparse-point provider transcript files are rejected",
        ));
    }
    let mut id = FILE_ID_INFO::default();
    query_handle_info(
        file,
        FileIdInfo,
        &mut id,
        "GetFileInformationByHandleEx(FileIdInfo)",
    )
    .map_err(|source| AuthorityOpenError::SystemIo {
        operation: "provider source retained-identity query",
        source,
    })?;
    let mut stable = Sha256::new();
    let mut change = Sha256::new();
    match version {
        super::RetainedFileIdentityVersion::SharedJsonlV1 => {
            stable.update(b"ctx-jsonl-retained-file-identity-v1\0windows-stable\0");
            change.update(b"ctx-jsonl-retained-file-identity-v1\0windows-change\0");
        }
        super::RetainedFileIdentityVersion::OrdinaryFileV2 => {
            stable.update(b"ctx-ordinary-file-observation-v2\0windows-stable\0");
            change.update(b"ctx-ordinary-file-observation-v2\0windows-change\0");
        }
    }
    stable.update(id.VolumeSerialNumber.to_le_bytes());
    stable.update(id.FileId.Identifier);
    if version == super::RetainedFileIdentityVersion::OrdinaryFileV2 {
        stable.update(basic.CreationTime.to_le_bytes());
        change.update(id.VolumeSerialNumber.to_le_bytes());
        change.update(id.FileId.Identifier);
    }
    change.update(basic.ChangeTime.to_le_bytes());
    change.update(basic.LastWriteTime.to_le_bytes());
    match version {
        super::RetainedFileIdentityVersion::SharedJsonlV1 => {
            change.update(basic.FileAttributes.to_le_bytes());
        }
        super::RetainedFileIdentityVersion::OrdinaryFileV2 => {
            change.update(metadata.len().to_le_bytes());
        }
    }
    Ok(Some((stable.finalize().into(), change.finalize().into())))
}

pub(super) fn read_exact_at(file: &File, mut bytes: &mut [u8], mut offset: u64) -> io::Result<()> {
    while !bytes.is_empty() {
        let read = read_at(file, bytes, offset)?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "provider source changed during exact range read",
            ));
        }
        offset = offset
            .checked_add(read as u64)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "range offset overflow"))?;
        bytes = &mut bytes[read..];
    }
    Ok(())
}

fn read_at(file: &File, bytes: &mut [u8], offset: u64) -> io::Result<usize> {
    if bytes.is_empty() {
        return Ok(0);
    }
    let requested = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
    let mut read = 0_u32;
    let mut operation = OVERLAPPED {
        Internal: 0,
        InternalHigh: 0,
        Anonymous: OVERLAPPED_0 {
            Anonymous: OVERLAPPED_0_0 {
                Offset: offset as u32,
                OffsetHigh: (offset >> 32) as u32,
            },
        },
        hEvent: std::ptr::null_mut(),
    };
    // Every handle admitted by this module is synchronous. Supplying an
    // OVERLAPPED value selects the exact 64-bit offset without reopening the
    // named path; ReadFile still completes before this stack buffer is released.
    let result = unsafe {
        ReadFile(
            file.as_raw_handle(),
            bytes.as_mut_ptr(),
            requested,
            &mut read,
            &mut operation,
        )
    };
    if result != 0 {
        if read > requested {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Windows returned an oversized provider source read",
            ));
        }
        Ok(read as usize)
    } else {
        let cause = io::Error::last_os_error();
        if cause.raw_os_error() == Some(ERROR_HANDLE_EOF as i32) {
            Ok(0)
        } else {
            Err(contextual_windows_io("ReadFile", cause))
        }
    }
}

fn qualified_drive_components(path: &Path) -> Result<(u8, Vec<&OsStr>), AuthorityOpenError> {
    let mut components = path.components();
    let drive = match components.next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => drive,
            _ => {
                return Err(AuthorityOpenError::Rejected(
                    "UNC, device, and unsupported Windows source roots are rejected",
                ));
            }
        },
        _ => {
            return Err(AuthorityOpenError::Rejected(
                "Windows provider source roots must use an absolute drive path",
            ));
        }
    };
    if !matches!(components.next(), Some(Component::RootDir)) {
        return Err(AuthorityOpenError::Rejected(
            "drive-relative Windows provider source roots are rejected",
        ));
    }
    let mut names = Vec::new();
    for component in components {
        match component {
            Component::Normal(name) => {
                validate_child_name(name)?;
                names.push(name);
            }
            Component::CurDir => {}
            _ => {
                return Err(AuthorityOpenError::Rejected(
                    "Windows provider source paths contain an unsupported component",
                ));
            }
        }
    }
    Ok((drive.to_ascii_uppercase(), names))
}

fn validate_child_name(name: &OsStr) -> Result<(), AuthorityOpenError> {
    let units = name.encode_wide().collect::<Vec<_>>();
    if units.is_empty()
        || units.iter().any(|unit| {
            *unit == 0 || *unit == b'\\' as u16 || *unit == b'/' as u16 || *unit == b':' as u16
        })
        || name == "."
        || name == ".."
    {
        return Err(AuthorityOpenError::Rejected(
            "Windows provider source child names must be single non-ADS components",
        ));
    }
    let byte_length = units
        .len()
        .checked_mul(size_of::<u16>())
        .and_then(|length| u16::try_from(length).ok());
    if byte_length.is_none() {
        return Err(AuthorityOpenError::Rejected(
            "Windows provider source path components are too long",
        ));
    }
    Ok(())
}

fn nt_open_child(
    parent: &File,
    name: &OsStr,
    kind: ProviderSourceOpenKind,
) -> Result<File, AuthorityOpenError> {
    let mut units = name.encode_wide().collect::<Vec<_>>();
    let length = units
        .len()
        .checked_mul(size_of::<u16>())
        .and_then(|length| u16::try_from(length).ok())
        .ok_or(AuthorityOpenError::Rejected(
            "Windows provider source path components are too long",
        ))?;
    let mut unicode = UnicodeString {
        length,
        maximum_length: length,
        buffer: units.as_mut_ptr(),
    };
    let attributes_length = u32::try_from(size_of::<ObjectAttributes>())
        .map_err(|_| AuthorityOpenError::Rejected("Windows object attributes are too large"))?;
    let attributes = ObjectAttributes {
        length: attributes_length,
        root_directory: parent.as_raw_handle(),
        object_name: &mut unicode,
        attributes: OBJ_CASE_INSENSITIVE,
        security_descriptor: std::ptr::null_mut(),
        security_quality_of_service: std::ptr::null_mut(),
    };
    let mut io_status = IoStatusBlock {
        status_or_pointer: std::ptr::null_mut(),
        information: 0,
    };
    let mut handle = std::ptr::null_mut();
    let status = unsafe {
        NtOpenFile(
            &mut handle,
            kind.desired_access(),
            &attributes,
            &mut io_status,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            kind.open_options(),
        )
    };
    if status < 0 {
        let windows_error = unsafe { RtlNtStatusToDosError(status) };
        let windows_error = i32::try_from(windows_error).unwrap_or(i32::MAX);
        return Err(AuthorityOpenError::SystemIo {
            operation: match kind {
                ProviderSourceOpenKind::AncestorDirectory => {
                    "provider source ancestor-directory open"
                }
                ProviderSourceOpenKind::Target => "provider source target open",
            },
            source: io::Error::from_raw_os_error(windows_error),
        });
    }
    if handle.is_null() {
        return Err(AuthorityOpenError::Rejected(
            "Windows returned an invalid provider source handle",
        ));
    }
    Ok(unsafe { File::from_raw_handle(handle) })
}

fn classify_opened(file: File) -> Result<OpenedPath, AuthorityOpenError> {
    let metadata = file
        .metadata()
        .map_err(|source| AuthorityOpenError::SystemIo {
            operation: "provider source metadata query",
            source,
        })?;
    let details = handle_details(&file).map_err(|source| AuthorityOpenError::SystemIo {
        operation: "provider source handle-information query",
        source,
    })?;
    ensure_handle_is_ordinary(&file, &details)?;
    let filesystem = filesystem_identity(&file, &details)?;
    ensure_not_cloud_root(&file, &filesystem)?;
    if metadata.file_type().is_file() {
        Ok(OpenedPath::File {
            file,
            metadata,
            filesystem,
        })
    } else if metadata.file_type().is_dir() {
        Ok(OpenedPath::Directory {
            file,
            metadata,
            filesystem,
        })
    } else {
        Err(AuthorityOpenError::Rejected(
            "provider source paths must be regular disk files or directories",
        ))
    }
}

struct HandleDetails {
    basic: FILE_BASIC_INFO,
    id: FILE_ID_INFO,
    attributes: u32,
}

fn handle_details(file: &File) -> io::Result<HandleDetails> {
    let mut basic = FILE_BASIC_INFO::default();
    query_handle_info(
        file,
        FileBasicInfo,
        &mut basic,
        "GetFileInformationByHandleEx(FileBasicInfo)",
    )?;
    let mut id = FILE_ID_INFO::default();
    query_handle_info(
        file,
        FileIdInfo,
        &mut id,
        "GetFileInformationByHandleEx(FileIdInfo)",
    )?;
    let mut tags = FILE_ATTRIBUTE_TAG_INFO::default();
    query_handle_info(
        file,
        FileAttributeTagInfo,
        &mut tags,
        "GetFileInformationByHandleEx(FileAttributeTagInfo)",
    )?;
    Ok(HandleDetails {
        basic,
        id,
        attributes: tags.FileAttributes,
    })
}

fn query_handle_info<T>(
    file: &File,
    class: i32,
    output: &mut T,
    operation: &'static str,
) -> io::Result<()> {
    let output_size = u32::try_from(size_of::<T>())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "handle info is too large"))?;
    let result = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            class,
            (output as *mut T).cast(),
            output_size,
        )
    };
    if result == 0 {
        Err(contextual_windows_io(operation, io::Error::last_os_error()))
    } else {
        Ok(())
    }
}

fn ensure_handle_is_ordinary(
    file: &File,
    details: &HandleDetails,
) -> Result<(), AuthorityOpenError> {
    if unsafe { GetFileType(file.as_raw_handle()) } != FILE_TYPE_DISK {
        return Err(AuthorityOpenError::Rejected(
            "provider source handles must refer to disk files",
        ));
    }
    if details.attributes
        & (FILE_ATTRIBUTE_REPARSE_POINT
            | FILE_ATTRIBUTE_OFFLINE
            | FILE_ATTRIBUTE_RECALL_ON_OPEN
            | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS)
        != 0
    {
        return Err(AuthorityOpenError::Rejected(
            super::REPARSE_PROVIDER_SOURCE_REASON,
        ));
    }
    Ok(())
}

fn ensure_not_cloud_root(
    file: &File,
    verified_filesystem: &FilesystemIdentity,
) -> Result<(), AuthorityOpenError> {
    let length = u32::try_from(size_of::<CfSyncRootBasicInfo>())
        .map_err(|_| AuthorityOpenError::Rejected("cloud root info is too large"))?;
    let library_name = "cldapi.dll\0".encode_utf16().collect::<Vec<_>>();
    let library = unsafe {
        LoadLibraryExW(
            library_name.as_ptr(),
            std::ptr::null_mut(),
            LOAD_LIBRARY_SEARCH_SYSTEM32,
        )
    };
    if library.is_null() {
        return Err(AuthorityOpenError::Rejected(
            "Windows Cloud Files API is unavailable",
        ));
    }
    let entry = unsafe { GetProcAddress(library, c"CfGetSyncRootInfoByHandle".as_ptr().cast()) };
    let Some(entry) = entry else {
        unsafe {
            FreeLibrary(library);
        }
        return Err(AuthorityOpenError::Rejected(
            "Windows Cloud Files API is unavailable",
        ));
    };
    // SAFETY: `entry` was resolved from the system copy of cldapi.dll using
    // the documented CfGetSyncRootInfoByHandle export name and ABI.
    let get_sync_root_info: CfGetSyncRootInfoByHandle = unsafe { std::mem::transmute(entry) };

    let mut basic = CfSyncRootBasicInfo::default();
    let mut returned = 0_u32;
    let result = unsafe {
        get_sync_root_info(
            file.as_raw_handle(),
            CF_SYNC_ROOT_INFO_BASIC,
            (&mut basic as *mut CfSyncRootBasicInfo).cast(),
            length,
            &mut returned,
        )
    };
    unsafe {
        FreeLibrary(library);
    }
    qualify_sync_root_query(result, verified_filesystem)
}

fn qualify_sync_root_query(
    result: i32,
    verified_filesystem: &FilesystemIdentity,
) -> Result<(), AuthorityOpenError> {
    if result >= 0 {
        return Err(AuthorityOpenError::Rejected(
            "cloud-synchronized provider source roots are rejected",
        ));
    }
    if result == ERROR_CLOUD_FILE_NOT_UNDER_SYNC_ROOT_HRESULT {
        return Ok(());
    }
    // Windows returns ERROR_INVALID_FUNCTION when Cloud Files is unavailable
    // on an ordinary fixed NTFS volume. Admit that result only after the
    // caller has proved this handle is a disk object without reparse, offline,
    // or recall attributes and has independently qualified its volume as NTFS.
    if result == ERROR_INVALID_FUNCTION_HRESULT
        && verified_filesystem
            .filesystem_name
            .eq_ignore_ascii_case("NTFS")
    {
        return Ok(());
    }
    Err(AuthorityOpenError::Rejected(
        "Windows could not qualify the provider source as non-cloud storage",
    ))
}

fn filesystem_identity(
    file: &File,
    details: &HandleDetails,
) -> Result<FilesystemIdentity, AuthorityOpenError> {
    let mut filesystem_name = [0_u16; 32];
    let result = unsafe {
        GetVolumeInformationByHandleW(
            file.as_raw_handle(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            filesystem_name.as_mut_ptr(),
            filesystem_name.len() as u32,
        )
    };
    if result == 0 {
        return Err(AuthorityOpenError::SystemIo {
            operation: "provider source volume-information query",
            source: io::Error::last_os_error(),
        });
    }
    let end = filesystem_name
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(filesystem_name.len());
    let filesystem_name = String::from_utf16(&filesystem_name[..end]).map_err(|_| {
        AuthorityOpenError::Rejected("Windows filesystem names must be valid UTF-16")
    })?;
    if !filesystem_name.eq_ignore_ascii_case("NTFS") {
        return Err(AuthorityOpenError::Rejected(
            "Windows provider source roots require local NTFS",
        ));
    }
    Ok(FilesystemIdentity {
        volume_serial_number: details.id.VolumeSerialNumber,
        filesystem_name,
    })
}

fn opened_filesystem(opened: &OpenedPath) -> &FilesystemIdentity {
    match opened {
        OpenedPath::File { filesystem, .. } | OpenedPath::Directory { filesystem, .. } => {
            filesystem
        }
    }
}

fn verify_root_drive(opened: &OpenedPath, expected_drive: u8) -> Result<(), AuthorityOpenError> {
    let file = match opened {
        OpenedPath::File { file, .. } | OpenedPath::Directory { file, .. } => file,
    };
    let required = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle(),
            std::ptr::null_mut(),
            0,
            FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
        )
    };
    if required == 0 {
        return Err(AuthorityOpenError::SystemIo {
            operation: "provider source drive-path qualification",
            source: io::Error::last_os_error(),
        });
    }
    let mut buffer = vec![0_u16; required as usize + 1];
    let written = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
        )
    };
    if written == 0 || written as usize >= buffer.len() {
        return Err(AuthorityOpenError::SystemIo {
            operation: "provider source drive-path qualification",
            source: io::Error::last_os_error(),
        });
    }
    buffer.truncate(written as usize);
    let prefix = [
        b'\\' as u16,
        b'\\' as u16,
        b'?' as u16,
        b'\\' as u16,
        expected_drive as u16,
        b':' as u16,
        b'\\' as u16,
    ];
    if buffer.len() < prefix.len()
        || !buffer
            .iter()
            .zip(prefix)
            .all(|(actual, expected)| ascii_upper_u16(*actual) == expected)
    {
        return Err(AuthorityOpenError::Rejected(
            "mapped, substituted, and virtual Windows source roots are rejected",
        ));
    }
    Ok(())
}

fn visit_directory_buffer<E>(
    buffer: &[u64],
    visit: &mut impl FnMut(OsString) -> Result<(), E>,
) -> Result<(), DirectoryEntryVisitError<E>> {
    let fixed = std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, FileName);
    let buffer_bytes = std::mem::size_of_val(buffer);
    let buffer_pointer = buffer.as_ptr().cast::<u8>();
    let mut offset = 0_usize;
    loop {
        let header_end = offset
            .checked_add(fixed)
            .filter(|end| *end <= buffer_bytes)
            .ok_or_else(|| {
                DirectoryEntryVisitError::Authority(AuthorityOpenError::Rejected(
                    "Windows returned an invalid provider directory entry",
                ))
            })?;
        let entry = unsafe { &*buffer_pointer.add(offset).cast::<FILE_ID_BOTH_DIR_INFO>() };
        let name_bytes = usize::try_from(entry.FileNameLength)
            .ok()
            .filter(|length| length % size_of::<u16>() == 0)
            .ok_or_else(|| {
                DirectoryEntryVisitError::Authority(AuthorityOpenError::Rejected(
                    "Windows returned an invalid provider directory name",
                ))
            })?;
        header_end
            .checked_add(name_bytes)
            .filter(|end| *end <= buffer_bytes)
            .ok_or_else(|| {
                DirectoryEntryVisitError::Authority(AuthorityOpenError::Rejected(
                    "Windows returned an invalid provider directory entry",
                ))
            })?;
        let name_units = unsafe {
            std::slice::from_raw_parts(
                buffer_pointer.add(header_end).cast::<u16>(),
                name_bytes / size_of::<u16>(),
            )
        };
        let name = OsString::from_wide(name_units);
        if name != "." && name != ".." {
            validate_child_name(&name).map_err(DirectoryEntryVisitError::Authority)?;
            visit(name).map_err(DirectoryEntryVisitError::Visitor)?;
        }
        if entry.NextEntryOffset == 0 {
            break;
        }
        offset = usize::try_from(entry.NextEntryOffset)
            .ok()
            .and_then(|next| offset.checked_add(next))
            .filter(|next| *next > offset && *next < buffer_bytes)
            .ok_or_else(|| {
                DirectoryEntryVisitError::Authority(AuthorityOpenError::Rejected(
                    "Windows returned an invalid provider directory offset",
                ))
            })?;
    }
    Ok(())
}

fn comparable_name(value: &OsString) -> Vec<u16> {
    value.encode_wide().map(ascii_lower_u16).collect()
}

fn ascii_lower_u16(value: u16) -> u16 {
    if (b'A' as u16..=b'Z' as u16).contains(&value) {
        value + u16::from(b'a' - b'A')
    } else {
        value
    }
}

fn ascii_upper_u16(value: u16) -> u16 {
    if (b'a' as u16..=b'z' as u16).contains(&value) {
        value - u16::from(b'a' - b'A')
    } else {
        value
    }
}

#[cfg(test)]
#[path = "windows/tests.rs"]
mod tests;
