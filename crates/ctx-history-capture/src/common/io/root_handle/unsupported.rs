use std::{
    ffi::{OsStr, OsString},
    fs::{File, Metadata},
    io,
    path::{Path, PathBuf},
};

use super::AuthorityOpenError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ObjectStamp;

#[derive(Debug)]
pub(super) struct FilesystemIdentity;

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

pub(super) fn open_absolute(_path: &Path) -> Result<OpenedPath, AuthorityOpenError> {
    Err(AuthorityOpenError::Rejected(
        "provider source authority handles are unsupported on this platform",
    ))
}

pub(super) fn open_child(
    _parent: &File,
    _name: &OsStr,
    _filesystem: &FilesystemIdentity,
) -> Result<OpenedPath, AuthorityOpenError> {
    Err(AuthorityOpenError::Rejected(
        "provider source authority handles are unsupported on this platform",
    ))
}

pub(super) fn directory_entries(
    _directory: &File,
    _maximum_entries: usize,
) -> Result<Vec<OsString>, AuthorityOpenError> {
    Err(AuthorityOpenError::Rejected(
        "provider source authority handles are unsupported on this platform",
    ))
}

pub(super) fn object_stamp(_file: &File, _metadata: &Metadata) -> io::Result<ObjectStamp> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "provider source authority handles are unsupported on this platform",
    ))
}

pub(super) fn read_exact_at(_file: &File, _bytes: &mut [u8], _offset: u64) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "provider source authority handles are unsupported on this platform",
    ))
}
