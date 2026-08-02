use super::*;

#[derive(Debug, Clone, Copy)]
pub(in super::super) enum ExpectedObjectKind {
    Directory,
    RegularFile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in super::super) struct NativeFileState {
    pub(in super::super) identity: NativeFileIdentity,
    pub(in super::super) length: u64,
    platform: PlatformFileState,
}

impl NativeFileState {
    pub(in super::super) fn read(
        file: &File,
        path: &Path,
        expected_kind: ExpectedObjectKind,
    ) -> SqliteSourceAccessResult<Self> {
        let metadata = file
            .metadata()
            .map_err(|source| SqliteSourceAccessError::Io {
                operation: "reading retained SQLite source metadata",
                path: path.to_path_buf(),
                source,
            })?;
        validate_opened_metadata(path, &metadata, expected_kind)?;
        let (identity, platform) =
            platform_file_state(file, &metadata).map_err(|source| SqliteSourceAccessError::Io {
                operation: "reading native SQLite source identity",
                path: path.to_path_buf(),
                source,
            })?;
        Ok(Self {
            identity,
            length: metadata.len(),
            platform,
        })
    }

    pub(super) fn hash_into(&self, digest: &mut Sha256) {
        self.identity.hash_into(digest);
        digest.update(self.length.to_le_bytes());
        self.platform.hash_into(digest);
    }
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in super::super) struct NativeFileIdentity {
    device: u64,
    inode: u64,
}

#[cfg(windows)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in super::super) struct NativeFileIdentity {
    volume_serial_number: u64,
    file_id: [u8; 16],
}

#[cfg(not(any(unix, windows)))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in super::super) struct NativeFileIdentity;

impl NativeFileIdentity {
    pub(super) fn digest(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(EVIDENCE_DOMAIN);
        digest.update(b"identity\0");
        self.hash_into(&mut digest);
        digest.finalize().into()
    }

    pub(super) fn hash_into(&self, digest: &mut Sha256) {
        #[cfg(unix)]
        {
            digest.update(self.device.to_le_bytes());
            digest.update(self.inode.to_le_bytes());
        }
        #[cfg(windows)]
        {
            digest.update(self.volume_serial_number.to_le_bytes());
            digest.update(self.file_id);
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = digest;
        }
    }
}

#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct PlatformFileState {
    mode: u32,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

#[cfg(windows)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct PlatformFileState {
    creation_time: i64,
    last_write_time: i64,
    change_time: i64,
    attributes: u32,
}

#[cfg(not(any(unix, windows)))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct PlatformFileState;

impl PlatformFileState {
    fn hash_into(&self, digest: &mut Sha256) {
        #[cfg(unix)]
        {
            digest.update(self.mode.to_le_bytes());
            digest.update(self.modified_seconds.to_le_bytes());
            digest.update(self.modified_nanoseconds.to_le_bytes());
            digest.update(self.changed_seconds.to_le_bytes());
            digest.update(self.changed_nanoseconds.to_le_bytes());
        }
        #[cfg(windows)]
        {
            digest.update(self.creation_time.to_le_bytes());
            digest.update(self.last_write_time.to_le_bytes());
            digest.update(self.change_time.to_le_bytes());
            digest.update(self.attributes.to_le_bytes());
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = digest;
        }
    }
}

#[cfg(unix)]
fn platform_file_state(
    _file: &File,
    metadata: &Metadata,
) -> std::io::Result<(NativeFileIdentity, PlatformFileState)> {
    use std::os::unix::fs::MetadataExt;

    Ok((
        NativeFileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        },
        PlatformFileState {
            mode: metadata.mode(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        },
    ))
}

#[cfg(windows)]
fn platform_file_state(
    file: &File,
    _metadata: &Metadata,
) -> std::io::Result<(NativeFileIdentity, PlatformFileState)> {
    use std::{mem::size_of, os::windows::io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        FileBasicInfo, FileIdInfo, GetFileInformationByHandleEx, FILE_BASIC_INFO, FILE_ID_INFO,
    };

    let handle = file.as_raw_handle();
    let mut basic = FILE_BASIC_INFO::default();
    if unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileBasicInfo,
            (&mut basic as *mut FILE_BASIC_INFO).cast(),
            size_of::<FILE_BASIC_INFO>() as u32,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    let mut id = FILE_ID_INFO::default();
    if unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileIdInfo,
            (&mut id as *mut FILE_ID_INFO).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok((
        NativeFileIdentity {
            volume_serial_number: id.VolumeSerialNumber,
            file_id: id.FileId.Identifier,
        },
        PlatformFileState {
            creation_time: basic.CreationTime,
            last_write_time: basic.LastWriteTime,
            change_time: basic.ChangeTime,
            attributes: basic.FileAttributes,
        },
    ))
}

#[cfg(not(any(unix, windows)))]
fn platform_file_state(
    _file: &File,
    _metadata: &Metadata,
) -> std::io::Result<(NativeFileIdentity, PlatformFileState)> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "native SQLite source identity is unsupported on this platform",
    ))
}

pub(in super::super) fn validate_approved_parent_path(path: &Path) -> SqliteSourceAccessResult<()> {
    if !path.is_absolute()
        || path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(SqliteSourceAccessError::UnsafeFile {
            path: path.to_path_buf(),
            reason: "the approved SQLite parent path must be absolute and traversal-free",
        });
    }
    Ok(())
}

pub(super) fn validate_database_leaf(name: &OsStr) -> SqliteSourceAccessResult<()> {
    let path = Path::new(name);
    if name.is_empty()
        || path.components().count() != 1
        || !matches!(path.components().next(), Some(Component::Normal(_)))
    {
        return Err(SqliteSourceAccessError::UnsafeFile {
            path: path.to_path_buf(),
            reason: "the SQLite database name must be one normal leaf component",
        });
    }
    Ok(())
}

pub(super) fn with_suffix(name: &OsStr, suffix: &str) -> OsString {
    let mut value = name.to_os_string();
    value.push(suffix);
    value
}

fn validate_opened_metadata(
    path: &Path,
    metadata: &Metadata,
    expected_kind: ExpectedObjectKind,
) -> SqliteSourceAccessResult<()> {
    if metadata.file_type().is_symlink() || metadata_is_reparse_point(metadata) {
        return Err(SqliteSourceAccessError::UnsafeFile {
            path: path.to_path_buf(),
            reason: "symlink and reparse-point SQLite source objects are not allowed",
        });
    }
    let valid = match expected_kind {
        ExpectedObjectKind::Directory => metadata.is_dir(),
        ExpectedObjectKind::RegularFile => metadata.file_type().is_file(),
    };
    if valid {
        Ok(())
    } else {
        Err(SqliteSourceAccessError::UnsafeFile {
            path: path.to_path_buf(),
            reason: match expected_kind {
                ExpectedObjectKind::Directory => "the approved SQLite parent must be a directory",
                ExpectedObjectKind::RegularFile => {
                    "SQLite source family members must be regular files"
                }
            },
        })
    }
}

#[cfg(windows)]
fn metadata_is_reparse_point(metadata: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn metadata_is_reparse_point(_metadata: &Metadata) -> bool {
    false
}
