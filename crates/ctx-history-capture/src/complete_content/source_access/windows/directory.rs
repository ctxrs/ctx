//! Handle-bound directory admission and enumeration for Windows sources.

use std::{
    ffi::OsString,
    fs::File,
    io,
    mem::size_of,
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        io::AsRawHandle,
    },
    path::Path,
    time::Instant,
};

use uuid::Uuid;
use windows_sys::Win32::Storage::FileSystem::{
    FileIdBothDirectoryInfo, FileIdBothDirectoryRestartInfo, GetFileInformationByHandleEx,
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ID_BOTH_DIR_INFO,
};

use super::{
    admit_path, admit_path_without_containment, ascii_lower_u16, content_error, file_identity,
    map_io_error, path_equal_case_insensitive, path_is_within_case_insensitive,
    AdmittedWindowsPath, CompleteContentError, CompleteContentErrorKind, WindowsFileIdentity,
};

const DIRECTORY_QUERY_BUFFER_BYTES: usize = 64 * 1024;
const ERROR_NO_MORE_FILES: i32 = 18;

pub(crate) struct AdmittedWindowsDirectory {
    pub(crate) file: File,
    pub(crate) identity: WindowsFileIdentity,
}

pub(crate) struct WindowsDirectoryEntry {
    pub(crate) name: OsString,
    pub(crate) file_id: i64,
    pub(crate) attributes: u32,
}

pub(crate) fn admit_path_under_retained_directory(
    path: &Path,
    parent: &WindowsFileIdentity,
    retained_root: &WindowsFileIdentity,
    expected_file_id: i64,
    expected_attributes: u32,
    event_id: Uuid,
) -> Result<AdmittedWindowsPath, CompleteContentError> {
    let parent_path = path
        .parent()
        .ok_or_else(|| content_error(event_id, CompleteContentErrorKind::SourceUnreadable))?;
    verify_named_directory_still_matches(parent_path, parent, event_id)?;
    let admitted = admit_path_without_containment(path, event_id)?;
    let identity = match &admitted {
        AdmittedWindowsPath::File(file) => &file.identity,
        AdmittedWindowsPath::Directory(directory) => &directory.identity,
    };
    let expected_name = path
        .file_name()
        .ok_or_else(|| content_error(event_id, CompleteContentErrorKind::SourceUnreadable))?;
    let expected_final = parent.final_path.join(expected_name);
    if !path_equal_case_insensitive(&identity.final_path, &expected_final)
        || !path_is_within_case_insensitive(&identity.final_path, &retained_root.final_path)
        || identity.volume_serial_number != retained_root.volume_serial_number
        || file_id_64(identity) != expected_file_id
        || (identity.attributes & FILE_ATTRIBUTE_DIRECTORY)
            != (expected_attributes & FILE_ATTRIBUTE_DIRECTORY)
    {
        return Err(content_error(
            event_id,
            CompleteContentErrorKind::SourceChanged,
        ));
    }
    verify_named_directory_still_matches(parent_path, parent, event_id)?;
    Ok(admitted)
}

pub(crate) fn admit_directory(
    path: &Path,
    containment_root: Option<&Path>,
    event_id: Uuid,
) -> Result<AdmittedWindowsDirectory, CompleteContentError> {
    match admit_path(path, containment_root, event_id)? {
        AdmittedWindowsPath::Directory(directory) => Ok(directory),
        AdmittedWindowsPath::File(_) => Err(content_error(
            event_id,
            CompleteContentErrorKind::SourceUnreadable,
        )),
    }
}

pub(crate) fn directory_entries(
    directory: &AdmittedWindowsDirectory,
    maximum_entries: usize,
    deadline: Instant,
    event_id: Uuid,
) -> Result<Vec<WindowsDirectoryEntry>, CompleteContentError> {
    let mut entries = Vec::new();
    let mut restart = true;
    loop {
        check_directory_deadline(deadline, event_id)?;
        let mut buffer = vec![0_u64; DIRECTORY_QUERY_BUFFER_BYTES / size_of::<u64>()];
        let class = if restart {
            FileIdBothDirectoryRestartInfo
        } else {
            FileIdBothDirectoryInfo
        };
        let result = unsafe {
            GetFileInformationByHandleEx(
                directory.file.as_raw_handle(),
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
            return Err(map_io_error(event_id, cause));
        }
        restart = false;
        parse_directory_query_buffer(&buffer, &mut entries, maximum_entries, deadline, event_id)?;
    }
    entries.sort_by(|left, right| {
        comparable_os_string(&left.name).cmp(&comparable_os_string(&right.name))
    });
    Ok(entries)
}

pub(crate) fn verify_named_directory_still_matches(
    path: &Path,
    expected: &WindowsFileIdentity,
    event_id: Uuid,
) -> Result<(), CompleteContentError> {
    let current = admit_path_without_containment(path, event_id)?;
    let AdmittedWindowsPath::Directory(current) = current else {
        return Err(content_error(
            event_id,
            CompleteContentErrorKind::SourceChanged,
        ));
    };
    let held = file_identity(&current.file).map_err(|cause| map_io_error(event_id, cause))?;
    if &current.identity != expected || &held != expected {
        return Err(content_error(
            event_id,
            CompleteContentErrorKind::SourceChanged,
        ));
    }
    Ok(())
}

pub(crate) fn verify_named_directory_still_matches_within(
    path: &Path,
    containment_root: Option<&Path>,
    expected: &WindowsFileIdentity,
    event_id: Uuid,
) -> Result<(), CompleteContentError> {
    let current = admit_directory(path, containment_root, event_id)?;
    let held = file_identity(&current.file).map_err(|cause| map_io_error(event_id, cause))?;
    if &current.identity != expected || &held != expected {
        return Err(content_error(
            event_id,
            CompleteContentErrorKind::SourceChanged,
        ));
    }
    Ok(())
}

fn parse_directory_query_buffer(
    buffer: &[u64],
    output: &mut Vec<WindowsDirectoryEntry>,
    maximum_entries: usize,
    deadline: Instant,
    event_id: Uuid,
) -> Result<(), CompleteContentError> {
    let fixed = std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, FileName);
    let buffer_bytes = std::mem::size_of_val(buffer);
    let buffer_pointer = buffer.as_ptr().cast::<u8>();
    let mut offset = 0_usize;
    loop {
        check_directory_deadline(deadline, event_id)?;
        let header_end = offset
            .checked_add(fixed)
            .filter(|end| *end <= buffer_bytes)
            .ok_or_else(|| content_error(event_id, CompleteContentErrorKind::SourceUnreadable))?;
        let entry = unsafe { &*buffer_pointer.add(offset).cast::<FILE_ID_BOTH_DIR_INFO>() };
        let name_bytes = usize::try_from(entry.FileNameLength)
            .ok()
            .filter(|length| length % size_of::<u16>() == 0)
            .ok_or_else(|| content_error(event_id, CompleteContentErrorKind::SourceUnreadable))?;
        let _name_end = header_end
            .checked_add(name_bytes)
            .filter(|end| *end <= buffer_bytes)
            .ok_or_else(|| content_error(event_id, CompleteContentErrorKind::SourceUnreadable))?;
        let name_units = unsafe {
            std::slice::from_raw_parts(
                buffer_pointer.add(header_end).cast::<u16>(),
                name_bytes / size_of::<u16>(),
            )
        };
        let name = OsString::from_wide(name_units);
        if name != "." && name != ".." {
            if output.len() >= maximum_entries {
                return Err(content_error(
                    event_id,
                    CompleteContentErrorKind::ContentTooLarge,
                ));
            }
            if name.is_empty()
                || name
                    .encode_wide()
                    .any(|unit| matches!(unit, value if value == b'\\' as u16 || value == b'/' as u16 || value == 0))
                || entry.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
            {
                return Err(content_error(
                    event_id,
                    CompleteContentErrorKind::SourceChanged,
                ));
            }
            output.push(WindowsDirectoryEntry {
                name,
                file_id: entry.FileId,
                attributes: entry.FileAttributes,
            });
        }
        if entry.NextEntryOffset == 0 {
            break;
        }
        let next = usize::try_from(entry.NextEntryOffset)
            .ok()
            .and_then(|next| offset.checked_add(next))
            .filter(|next| *next > offset && *next < buffer_bytes)
            .ok_or_else(|| content_error(event_id, CompleteContentErrorKind::SourceUnreadable))?;
        offset = next;
    }
    Ok(())
}

fn check_directory_deadline(deadline: Instant, event_id: Uuid) -> Result<(), CompleteContentError> {
    if Instant::now() >= deadline {
        Err(content_error(
            event_id,
            CompleteContentErrorKind::SourceChanged,
        ))
    } else {
        Ok(())
    }
}

fn file_id_64(identity: &WindowsFileIdentity) -> i64 {
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&identity.file_id[..8]);
    i64::from_le_bytes(bytes)
}

fn comparable_os_string(value: &OsString) -> Vec<u16> {
    value.encode_wide().map(ascii_lower_u16).collect::<Vec<_>>()
}
