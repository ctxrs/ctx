//! Windows admission primitives for provider-owned complete content.
//!
//! Provider paths exist only at this admission boundary. Every successful
//! admission retains an opened handle, rejects reparse points and remote
//! volumes, and binds the named route to the handle's final path and file ID.

use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom},
    mem::size_of,
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        fs::OpenOptionsExt,
        io::AsRawHandle,
    },
    path::{Component, Path, PathBuf, Prefix},
};

use uuid::Uuid;
use windows_sys::Win32::Storage::FileSystem::{
    FileBasicInfo, FileIdInfo, GetDriveTypeW, GetFileInformationByHandleEx,
    GetFinalPathNameByHandleW, FILE_ATTRIBUTE_REPARSE_POINT, FILE_BASIC_INFO, FILE_ID_INFO,
    FILE_NAME_NORMALIZED, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, VOLUME_NAME_DOS,
};

use super::{map_io_error, CompleteContentError, CompleteContentErrorKind};

#[path = "windows/directory.rs"]
mod directory;

pub(super) use directory::{admit_directory, verify_named_directory_still_matches_within};
pub(crate) use directory::{
    admit_path_under_retained_directory, directory_entries, verify_named_directory_still_matches,
    AdmittedWindowsDirectory,
};

const DRIVE_UNKNOWN: u32 = 0;
const DRIVE_NO_ROOT_DIR: u32 = 1;
const DRIVE_REMOTE: u32 = 4;
const FINAL_PATH_PREFIX: &[u16] = &[b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];
const UNC_AFTER_FINAL_PREFIX: &[u16] = &[b'U' as u16, b'N' as u16, b'C' as u16, b'\\' as u16];
const MAX_FINAL_PATH_UNITS: usize = 32 * 1024;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct WindowsFileIdentity {
    pub(super) volume_serial_number: u64,
    pub(super) file_id: [u8; 16],
    pub(super) change_time: i64,
    pub(super) last_write_time: i64,
    pub(super) attributes: u32,
    length: u64,
    final_path: PathBuf,
}

impl std::fmt::Debug for WindowsFileIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WindowsFileIdentity")
            .field("volume_serial_number", &self.volume_serial_number)
            .field("file_id", &self.file_id)
            .field("change_time", &self.change_time)
            .field("last_write_time", &self.last_write_time)
            .field("attributes", &self.attributes)
            .field("length", &self.length)
            .finish_non_exhaustive()
    }
}

pub(crate) struct AdmittedWindowsFile {
    pub(crate) file: File,
    pub(crate) metadata: fs::Metadata,
    pub(crate) identity: WindowsFileIdentity,
}

pub(crate) enum AdmittedWindowsPath {
    File(AdmittedWindowsFile),
    Directory(AdmittedWindowsDirectory),
}

pub(crate) fn validate_local_qualified_path(
    path: &Path,
    event_id: Uuid,
) -> Result<(), CompleteContentError> {
    let mut components = path.components();
    let qualified = matches!(components.next(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
        && matches!(components.next(), Some(Component::RootDir));
    if !qualified
        || components.any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        })
    {
        return Err(content_error(
            event_id,
            CompleteContentErrorKind::SourceUnreadable,
        ));
    }
    reject_remote_drive(path, event_id)
}

pub(crate) fn lexical_path_is_within(path: &Path, root: &Path) -> bool {
    path_is_within_case_insensitive(path, root)
}

pub(super) fn admit_regular_file(
    path: &Path,
    containment_root: Option<&Path>,
    event_id: Uuid,
) -> Result<AdmittedWindowsFile, CompleteContentError> {
    validate_local_qualified_path(path, event_id)?;
    let file = open_path_handle(path, false, event_id)?;
    let metadata = regular_non_reparse_metadata(&file, event_id)?;
    let identity = file_identity_with_metadata(&file, &metadata, event_id)?;

    let named = open_path_handle(path, false, event_id)?;
    let named_metadata = regular_non_reparse_metadata(&named, event_id)?;
    let named_identity = file_identity_with_metadata(&named, &named_metadata, event_id)?;
    if identity != named_identity {
        return Err(content_error(
            event_id,
            CompleteContentErrorKind::SourceChanged,
        ));
    }
    validate_containment(&identity.final_path, containment_root, event_id)?;
    Ok(AdmittedWindowsFile {
        file,
        metadata,
        identity,
    })
}

pub(crate) fn admit_path(
    path: &Path,
    containment_root: Option<&Path>,
    event_id: Uuid,
) -> Result<AdmittedWindowsPath, CompleteContentError> {
    let admitted = admit_path_without_containment(path, event_id)?;
    let identity = match &admitted {
        AdmittedWindowsPath::File(file) => &file.identity,
        AdmittedWindowsPath::Directory(directory) => &directory.identity,
    };
    validate_containment(&identity.final_path, containment_root, event_id)?;
    Ok(admitted)
}

pub(crate) fn verify_named_admitted_path_still_matches(
    path: &Path,
    expected: &WindowsFileIdentity,
    event_id: Uuid,
) -> Result<(), CompleteContentError> {
    let current = admit_path_without_containment(path, event_id)?;
    let (file, identity) = match current {
        AdmittedWindowsPath::File(current) => (current.file, current.identity),
        AdmittedWindowsPath::Directory(current) => (current.file, current.identity),
    };
    let held = file_identity(&file).map_err(|cause| map_io_error(event_id, cause))?;
    if &identity != expected || &held != expected {
        return Err(content_error(
            event_id,
            CompleteContentErrorKind::SourceChanged,
        ));
    }
    Ok(())
}

pub(crate) fn read_bounded_admitted_regular_file(
    admitted: &AdmittedWindowsFile,
    maximum: usize,
    event_id: Uuid,
) -> Result<Vec<u8>, CompleteContentError> {
    let length = usize::try_from(admitted.metadata.len())
        .map_err(|_| content_error(event_id, CompleteContentErrorKind::ContentTooLarge))?;
    if length > maximum {
        return Err(content_error(
            event_id,
            CompleteContentErrorKind::ContentTooLarge,
        ));
    }
    let mut bytes = vec![0_u8; length];
    if !bytes.is_empty() {
        read_exact_at(&admitted.file, &mut bytes, 0)
            .map_err(|cause| map_io_error(event_id, cause))?;
    }
    let current = file_identity(&admitted.file).map_err(|cause| map_io_error(event_id, cause))?;
    if current != admitted.identity {
        return Err(content_error(
            event_id,
            CompleteContentErrorKind::SourceChanged,
        ));
    }
    Ok(bytes)
}

pub(super) fn admit_optional_regular_file(
    path: &Path,
    containment_root: Option<&Path>,
    event_id: Uuid,
) -> Result<Option<AdmittedWindowsFile>, CompleteContentError> {
    match admit_regular_file(path, containment_root, event_id) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind == CompleteContentErrorKind::SourceMissing => Ok(None),
        Err(error) => Err(error),
    }
}

pub(super) fn verify_named_file_still_matches(
    path: &Path,
    containment_root: Option<&Path>,
    expected: &WindowsFileIdentity,
    event_id: Uuid,
) -> Result<(), CompleteContentError> {
    // Admission errors describe the initial source state. Once a capability
    // has been admitted, any failure to reopen the same named object means the
    // route changed (including delete, replacement, or a new reparse point).
    let current = admit_regular_file(path, containment_root, event_id)
        .map_err(|_| content_error(event_id, CompleteContentErrorKind::SourceChanged))?;
    let held = file_identity(&current.file)
        .map_err(|_| content_error(event_id, CompleteContentErrorKind::SourceChanged))?;
    if &current.identity != expected || &held != expected {
        return Err(content_error(
            event_id,
            CompleteContentErrorKind::SourceChanged,
        ));
    }
    Ok(())
}

pub(super) fn verify_optional_named_file_still_matches(
    path: &Path,
    containment_root: Option<&Path>,
    expected: Option<&WindowsFileIdentity>,
    event_id: Uuid,
) -> Result<(), CompleteContentError> {
    let current = admit_optional_regular_file(path, containment_root, event_id)
        .map_err(|_| content_error(event_id, CompleteContentErrorKind::SourceChanged))?;
    if current.as_ref().map(|file| &file.identity) != expected {
        return Err(content_error(
            event_id,
            CompleteContentErrorKind::SourceChanged,
        ));
    }
    Ok(())
}

pub(super) fn copy_bounded_handle(
    source: &AdmittedWindowsFile,
    destination: &Path,
    maximum: u64,
    event_id: Uuid,
) -> Result<(), CompleteContentError> {
    if source.metadata.len() > maximum {
        return Err(content_error(
            event_id,
            CompleteContentErrorKind::ContentTooLarge,
        ));
    }
    let mut input = source
        .file
        .try_clone()
        .map_err(|cause| map_io_error(event_id, cause))?;
    input
        .seek(SeekFrom::Start(0))
        .map_err(|cause| map_io_error(event_id, cause))?;
    let mut output = File::create(destination).map_err(|cause| map_io_error(event_id, cause))?;
    let copied = io::copy(
        &mut input.by_ref().take(maximum.saturating_add(1)),
        &mut output,
    )
    .map_err(|cause| map_io_error(event_id, cause))?;
    let current = file_identity(&source.file).map_err(|cause| map_io_error(event_id, cause))?;
    if copied != source.metadata.len() || current != source.identity {
        return Err(content_error(
            event_id,
            CompleteContentErrorKind::SourceChanged,
        ));
    }
    Ok(())
}

pub(super) fn file_identity(file: &File) -> io::Result<WindowsFileIdentity> {
    let metadata = file.metadata()?;
    file_identity_io(file, &metadata)
}

pub(super) fn read_exact_at(file: &File, mut buffer: &mut [u8], mut offset: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buffer.is_empty() {
        let read = file.seek_read(buffer, offset)?;
        if read == 0 {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        offset = offset.saturating_add(read as u64);
        buffer = &mut buffer[read..];
    }
    Ok(())
}

fn open_path_handle(
    path: &Path,
    directory: bool,
    event_id: Uuid,
) -> Result<File, CompleteContentError> {
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    };
    let flags = FILE_FLAG_OPEN_REPARSE_POINT
        | if directory {
            FILE_FLAG_BACKUP_SEMANTICS
        } else {
            0
        };
    OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(flags)
        .open(path)
        .map_err(|cause| map_io_error(event_id, cause))
}

fn admit_path_without_containment(
    path: &Path,
    event_id: Uuid,
) -> Result<AdmittedWindowsPath, CompleteContentError> {
    validate_local_qualified_path(path, event_id)?;
    let file = open_path_handle(path, true, event_id)?;
    let metadata = file
        .metadata()
        .map_err(|cause| map_io_error(event_id, cause))?;
    if !metadata.file_type().is_file() && !metadata.file_type().is_dir() {
        return Err(content_error(
            event_id,
            CompleteContentErrorKind::SourceUnreadable,
        ));
    }
    let identity = file_identity_with_metadata(&file, &metadata, event_id)?;
    if identity.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(content_error(
            event_id,
            CompleteContentErrorKind::SourceChanged,
        ));
    }
    let named = open_path_handle(path, true, event_id)?;
    let named_metadata = named
        .metadata()
        .map_err(|cause| map_io_error(event_id, cause))?;
    let named_identity = file_identity_with_metadata(&named, &named_metadata, event_id)?;
    if identity != named_identity {
        return Err(content_error(
            event_id,
            CompleteContentErrorKind::SourceChanged,
        ));
    }
    if metadata.file_type().is_file() {
        Ok(AdmittedWindowsPath::File(AdmittedWindowsFile {
            file,
            metadata,
            identity,
        }))
    } else {
        Ok(AdmittedWindowsPath::Directory(AdmittedWindowsDirectory {
            file,
            identity,
        }))
    }
}

fn regular_non_reparse_metadata(
    file: &File,
    event_id: Uuid,
) -> Result<fs::Metadata, CompleteContentError> {
    let metadata = file
        .metadata()
        .map_err(|cause| map_io_error(event_id, cause))?;
    if !metadata.file_type().is_file() {
        return Err(content_error(
            event_id,
            CompleteContentErrorKind::SourceUnreadable,
        ));
    }
    let identity = file_identity_with_metadata(file, &metadata, event_id)?;
    if identity.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(content_error(
            event_id,
            CompleteContentErrorKind::SourceUnreadable,
        ));
    }
    Ok(metadata)
}

fn file_identity_with_metadata(
    file: &File,
    metadata: &fs::Metadata,
    event_id: Uuid,
) -> Result<WindowsFileIdentity, CompleteContentError> {
    file_identity_io(file, metadata).map_err(|cause| map_io_error(event_id, cause))
}

fn file_identity_io(file: &File, metadata: &fs::Metadata) -> io::Result<WindowsFileIdentity> {
    let handle = file.as_raw_handle();
    let mut basic = FILE_BASIC_INFO::default();
    let basic_result = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileBasicInfo,
            (&mut basic as *mut FILE_BASIC_INFO).cast(),
            size_of::<FILE_BASIC_INFO>() as u32,
        )
    };
    if basic_result == 0 {
        return Err(io::Error::last_os_error());
    }
    if basic.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "reparse-point source is not admissible",
        ));
    }
    let mut id = FILE_ID_INFO::default();
    let id_result = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            (&mut id as *mut FILE_ID_INFO).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if id_result == 0 {
        return Err(io::Error::last_os_error());
    }
    let final_path = final_local_path(file)?;
    Ok(WindowsFileIdentity {
        volume_serial_number: id.VolumeSerialNumber,
        file_id: id.FileId.Identifier,
        change_time: basic.ChangeTime,
        last_write_time: basic.LastWriteTime,
        attributes: basic.FileAttributes,
        length: metadata.len(),
        final_path,
    })
}

fn final_local_path(file: &File) -> io::Result<PathBuf> {
    let handle = file.as_raw_handle();
    let flags = FILE_NAME_NORMALIZED | VOLUME_NAME_DOS;
    let required = unsafe { GetFinalPathNameByHandleW(handle, std::ptr::null_mut(), 0, flags) };
    if required == 0 || required as usize > MAX_FINAL_PATH_UNITS {
        return Err(if required == 0 {
            io::Error::last_os_error()
        } else {
            io::Error::new(io::ErrorKind::InvalidData, "final source path is too long")
        });
    }
    let mut buffer = vec![0_u16; required as usize + 1];
    let written = unsafe {
        GetFinalPathNameByHandleW(handle, buffer.as_mut_ptr(), buffer.len() as u32, flags)
    };
    if written == 0 || written as usize >= buffer.len() {
        return Err(io::Error::last_os_error());
    }
    buffer.truncate(written as usize);
    let without_prefix = buffer
        .strip_prefix(FINAL_PATH_PREFIX)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "unqualified final path"))?;
    if starts_with_ascii_case_insensitive(without_prefix, UNC_AFTER_FINAL_PREFIX) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "network source paths are rejected",
        ));
    }
    let path = PathBuf::from(OsString::from_wide(without_prefix));
    validate_local_qualified_path_io(&path)?;
    reject_remote_drive_io(&path)?;
    Ok(path)
}

fn validate_containment(
    final_path: &Path,
    containment_root: Option<&Path>,
    event_id: Uuid,
) -> Result<(), CompleteContentError> {
    let Some(root) = containment_root else {
        return Ok(());
    };
    validate_local_qualified_path(root, event_id)?;
    let root_handle = open_path_handle(root, true, event_id)?;
    let root_metadata = root_handle
        .metadata()
        .map_err(|cause| map_io_error(event_id, cause))?;
    let root_identity = file_identity_with_metadata(&root_handle, &root_metadata, event_id)?;
    let named_root = open_path_handle(root, true, event_id)?;
    let named_metadata = named_root
        .metadata()
        .map_err(|cause| map_io_error(event_id, cause))?;
    let named_identity = file_identity_with_metadata(&named_root, &named_metadata, event_id)?;
    if root_identity != named_identity
        || root_identity.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || !path_is_within_case_insensitive(final_path, &root_identity.final_path)
    {
        return Err(content_error(
            event_id,
            CompleteContentErrorKind::SourceUnreadable,
        ));
    }
    Ok(())
}

fn path_is_within_case_insensitive(path: &Path, root: &Path) -> bool {
    let path_components = comparable_path_components(path);
    let root_components = comparable_path_components(root);
    root_components.len() <= path_components.len()
        && root_components
            .iter()
            .zip(&path_components)
            .all(|(root, path)| {
                root.iter()
                    .copied()
                    .map(ascii_lower_u16)
                    .eq(path.iter().copied().map(ascii_lower_u16))
            })
}

fn path_equal_case_insensitive(left: &Path, right: &Path) -> bool {
    let left = comparable_path_components(left);
    let right = comparable_path_components(right);
    left.len() == right.len()
        && left.iter().zip(&right).all(|(left, right)| {
            left.iter()
                .copied()
                .map(ascii_lower_u16)
                .eq(right.iter().copied().map(ascii_lower_u16))
        })
}

fn comparable_path_components(path: &Path) -> Vec<Vec<u16>> {
    path.components()
        .map(|component| match component {
            Component::Prefix(prefix) => match prefix.kind() {
                Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
                    vec![ascii_lower_u16(letter as u16), b':' as u16]
                }
                _ => component.as_os_str().encode_wide().collect(),
            },
            _ => component.as_os_str().encode_wide().collect(),
        })
        .collect()
}

fn reject_remote_drive(path: &Path, event_id: Uuid) -> Result<(), CompleteContentError> {
    reject_remote_drive_io(path)
        .map_err(|_| content_error(event_id, CompleteContentErrorKind::SourceUnreadable))
}

fn reject_remote_drive_io(path: &Path) -> io::Result<()> {
    let drive = match path.components().next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => letter,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "non-local source path",
                ))
            }
        },
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unqualified source path",
            ))
        }
    };
    let root = [drive as u16, b':' as u16, b'\\' as u16, 0];
    let drive_type = unsafe { GetDriveTypeW(root.as_ptr()) };
    if matches!(drive_type, DRIVE_UNKNOWN | DRIVE_NO_ROOT_DIR | DRIVE_REMOTE) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "network or unavailable source volume",
        ));
    }
    Ok(())
}

fn validate_local_qualified_path_io(path: &Path) -> io::Result<()> {
    let mut components = path.components();
    if !matches!(components.next(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
        || !matches!(components.next(), Some(Component::RootDir))
        || components.any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        })
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "source path is not a qualified local DOS path",
        ));
    }
    Ok(())
}

fn starts_with_ascii_case_insensitive(value: &[u16], prefix: &[u16]) -> bool {
    value.len() >= prefix.len()
        && value
            .iter()
            .copied()
            .zip(prefix.iter().copied())
            .take(prefix.len())
            .all(|(left, right)| ascii_lower_u16(left) == ascii_lower_u16(right))
}

fn ascii_lower_u16(value: u16) -> u16 {
    if (b'A' as u16..=b'Z' as u16).contains(&value) {
        value + u16::from(b'a' - b'A')
    } else {
        value
    }
}

fn content_error(event_id: Uuid, kind: CompleteContentErrorKind) -> CompleteContentError {
    CompleteContentError::new(kind, event_id)
}
#[cfg(test)]
#[path = "windows/enumeration_tests.rs"]
mod enumeration_tests;
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unc_device_and_drive_relative_routes() {
        let event_id = Uuid::new_v4();
        for path in [
            r"\\server\share\session.jsonl",
            r"\\?\UNC\server\share\session.jsonl",
            r"\\.\C:\sessions\session.jsonl",
            r"C:sessions\session.jsonl",
            r"sessions\session.jsonl",
        ] {
            assert!(validate_local_qualified_path(Path::new(path), event_id).is_err());
        }
    }

    #[test]
    fn path_containment_is_component_based_and_case_insensitive() {
        assert!(path_is_within_case_insensitive(
            Path::new(r"C:\Users\Dev\ctx\session.jsonl"),
            Path::new(r"c:\users\dev\CTX"),
        ));
        assert!(path_is_within_case_insensitive(
            Path::new(r"\\?\C:\Users\Dev\ctx\session.jsonl"),
            Path::new(r"c:\users\dev\CTX"),
        ));
        assert!(!path_is_within_case_insensitive(
            Path::new(r"C:\Users\Dev\ctx-other\session.jsonl"),
            Path::new(r"C:\Users\Dev\ctx"),
        ));
    }

    #[test]
    fn admitted_file_uses_held_handle_after_named_path_is_replaced() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("session.jsonl");
        fs::write(&path, b"original").unwrap();
        let admitted = admit_regular_file(&path, Some(temp.path()), Uuid::new_v4()).unwrap();
        let replacement = temp.path().join("replacement.jsonl");
        let moved = temp.path().join("moved-original.jsonl");
        fs::write(&replacement, b"replacement").unwrap();
        fs::rename(&path, &moved).unwrap();
        fs::rename(&replacement, &path).unwrap();

        let mut bytes = [0_u8; 8];
        read_exact_at(&admitted.file, &mut bytes, 0).unwrap();
        assert_eq!(&bytes, b"original");
        let error = verify_named_file_still_matches(
            &path,
            Some(temp.path()),
            &admitted.identity,
            Uuid::new_v4(),
        )
        .unwrap_err();
        assert_eq!(error.kind, CompleteContentErrorKind::SourceChanged);
    }

    #[test]
    fn named_file_delete_after_admission_is_source_changed() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("session.jsonl");
        fs::write(&path, b"original").unwrap();
        let admitted = admit_regular_file(&path, Some(temp.path()), Uuid::new_v4()).unwrap();
        fs::remove_file(&path).unwrap();

        let error = verify_named_file_still_matches(
            &path,
            Some(temp.path()),
            &admitted.identity,
            Uuid::new_v4(),
        )
        .unwrap_err();
        assert_eq!(error.kind, CompleteContentErrorKind::SourceChanged);
    }

    #[test]
    fn named_file_reparse_replacement_after_admission_is_source_changed() {
        use std::os::windows::fs::symlink_file;

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("session.jsonl");
        let moved = temp.path().join("moved-session.jsonl");
        fs::write(&path, b"original").unwrap();
        let admitted = admit_regular_file(&path, Some(temp.path()), Uuid::new_v4()).unwrap();
        fs::rename(&path, &moved).unwrap();
        if let Err(error) = symlink_file(&moved, &path) {
            if error.kind() == io::ErrorKind::PermissionDenied {
                return;
            }
            panic!("failed to create Windows file reparse point: {error}");
        }

        let error = verify_named_file_still_matches(
            &path,
            Some(temp.path()),
            &admitted.identity,
            Uuid::new_v4(),
        )
        .unwrap_err();
        assert_eq!(error.kind, CompleteContentErrorKind::SourceChanged);
    }

    #[test]
    fn final_handle_path_must_stay_within_the_authorized_root() {
        let temp = tempfile::tempdir().unwrap();
        let allowed = temp.path().join("allowed");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&allowed).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let outside_file = outside.join("session.jsonl");
        fs::write(&outside_file, b"outside").unwrap();
        assert!(admit_regular_file(&outside_file, Some(&allowed), Uuid::new_v4()).is_err());
    }

    #[test]
    fn retained_directory_identity_detects_named_root_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let moved = temp.path().join("moved-root");
        let replacement = temp.path().join("replacement");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&replacement).unwrap();
        let admitted = admit_path(&root, Some(temp.path()), Uuid::new_v4()).unwrap();
        let AdmittedWindowsPath::Directory(admitted) = admitted else {
            panic!("root must be admitted as a directory");
        };
        fs::rename(&root, &moved).unwrap();
        fs::rename(&replacement, &root).unwrap();
        assert!(
            verify_named_directory_still_matches(&root, &admitted.identity, Uuid::new_v4(),)
                .is_err()
        );
    }

    #[test]
    fn retained_parent_and_root_bind_child_final_handle() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        fs::create_dir_all(&root).unwrap();
        let child = root.join("session.json");
        fs::write(&child, b"content").unwrap();
        let admitted = admit_path(&root, Some(temp.path()), Uuid::new_v4()).unwrap();
        let AdmittedWindowsPath::Directory(root_directory) = admitted else {
            panic!("root must be admitted as a directory");
        };
        let entries = directory_entries(
            &root_directory,
            8,
            std::time::Instant::now() + std::time::Duration::from_secs(1),
            Uuid::new_v4(),
        )
        .unwrap();
        let observed = entries
            .iter()
            .find(|entry| entry.name == "session.json")
            .unwrap();
        let child = admit_path_under_retained_directory(
            &child,
            &root_directory.identity,
            &root_directory.identity,
            observed.file_id,
            observed.attributes,
            Uuid::new_v4(),
        )
        .unwrap();
        let AdmittedWindowsPath::File(child) = child else {
            panic!("child must be admitted as a file");
        };
        assert_eq!(
            read_bounded_admitted_regular_file(&child, 16, Uuid::new_v4()).unwrap(),
            b"content"
        );
    }
}
