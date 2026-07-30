use std::{
    fs::{File, Metadata},
    path::Path,
};

use sha2::{Digest, Sha256};

use super::{JsonlFileObservation, JsonlObservedTime};
#[cfg(target_os = "windows")]
use crate::CaptureError;
use crate::Result;

pub(super) fn observe_metadata(
    path: &Path,
    file: &File,
    metadata: &Metadata,
) -> Result<JsonlFileObservation> {
    let identity = retained_file_identity(path, file, metadata)?;
    Ok(JsonlFileObservation {
        length: metadata.len(),
        modified: JsonlObservedTime::from_system_time(metadata.modified()?),
        readonly: metadata.permissions().readonly(),
        stable_identity: identity.as_ref().map(|identity| identity.0),
        change_identity: identity.map(|identity| identity.1),
    })
}

#[cfg(unix)]
fn retained_file_identity(
    _path: &Path,
    _file: &File,
    metadata: &Metadata,
) -> Result<Option<([u8; 32], [u8; 32])>> {
    use std::os::unix::fs::MetadataExt;

    let mut stable = Sha256::new();
    stable.update(b"ctx-jsonl-retained-file-identity-v1\0unix-stable\0");
    stable.update(metadata.dev().to_le_bytes());
    stable.update(metadata.ino().to_le_bytes());
    let mut change = Sha256::new();
    change.update(b"ctx-jsonl-retained-file-identity-v1\0unix-change\0");
    change.update(metadata.ctime().to_le_bytes());
    change.update(metadata.ctime_nsec().to_le_bytes());
    Ok(Some((stable.finalize().into(), change.finalize().into())))
}

#[cfg(target_os = "windows")]
fn retained_file_identity(
    path: &Path,
    file: &File,
    _metadata: &Metadata,
) -> Result<Option<([u8; 32], [u8; 32])>> {
    use std::{mem::size_of, os::windows::io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        FileBasicInfo, FileIdInfo, GetFileInformationByHandleEx, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_BASIC_INFO, FILE_ID_INFO,
    };

    let handle = file.as_raw_handle();
    let mut basic_info = FILE_BASIC_INFO::default();
    let basic_result = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileBasicInfo,
            &mut basic_info as *mut FILE_BASIC_INFO as *mut std::ffi::c_void,
            size_of::<FILE_BASIC_INFO>() as u32,
        )
    };
    if basic_result == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if basic_info.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "reparse-point provider transcript files are rejected",
        });
    }
    let mut id_info = FILE_ID_INFO::default();
    let id_result = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            &mut id_info as *mut FILE_ID_INFO as *mut std::ffi::c_void,
            size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if id_result == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut stable = Sha256::new();
    stable.update(b"ctx-jsonl-retained-file-identity-v1\0windows-stable\0");
    stable.update(id_info.VolumeSerialNumber.to_le_bytes());
    stable.update(id_info.FileId.Identifier);
    let mut change = Sha256::new();
    change.update(b"ctx-jsonl-retained-file-identity-v1\0windows-change\0");
    change.update(basic_info.ChangeTime.to_le_bytes());
    change.update(basic_info.LastWriteTime.to_le_bytes());
    change.update(basic_info.FileAttributes.to_le_bytes());
    Ok(Some((stable.finalize().into(), change.finalize().into())))
}

#[cfg(not(any(unix, target_os = "windows")))]
fn retained_file_identity(
    _path: &Path,
    _file: &File,
    _metadata: &Metadata,
) -> Result<Option<([u8; 32], [u8; 32])>> {
    Ok(None)
}
