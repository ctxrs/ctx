//! Stable object identity captured from an already-opened source handle.

use std::{fs, fs::File, io, time::SystemTime};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FrozenFile {
    pub(super) length: u64,
    modified: SystemTime,
    readonly: bool,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    ctime: i64,
    #[cfg(unix)]
    ctime_nsec: i64,
    #[cfg(target_os = "windows")]
    volume_serial_number: u64,
    #[cfg(target_os = "windows")]
    file_id: [u8; 16],
    #[cfg(target_os = "windows")]
    change_time: i64,
    #[cfg(target_os = "windows")]
    last_write_time: i64,
    #[cfg(target_os = "windows")]
    attributes: u32,
}

impl FrozenFile {
    pub(super) fn same_object(&self, other: &Self) -> bool {
        #[cfg(unix)]
        {
            self.device == other.device && self.inode == other.inode
        }
        #[cfg(target_os = "windows")]
        {
            self.volume_serial_number == other.volume_serial_number && self.file_id == other.file_id
        }
        #[cfg(not(any(unix, target_os = "windows")))]
        {
            self == other
        }
    }

    #[cfg(unix)]
    pub(super) fn from_metadata(metadata: &fs::Metadata) -> io::Result<Self> {
        use std::os::unix::fs::MetadataExt;
        Ok(Self {
            length: metadata.len(),
            modified: metadata.modified()?,
            readonly: metadata.permissions().readonly(),
            device: metadata.dev(),
            inode: metadata.ino(),
            ctime: metadata.ctime(),
            ctime_nsec: metadata.ctime_nsec(),
        })
    }

    pub(super) fn from_file(file: &File, metadata: &fs::Metadata) -> io::Result<Self> {
        #[cfg(unix)]
        {
            let _ = file;
            Self::from_metadata(metadata)
        }
        #[cfg(target_os = "windows")]
        {
            let identity = super::windows::file_identity(file)?;
            Ok(Self {
                length: metadata.len(),
                modified: metadata.modified()?,
                readonly: metadata.permissions().readonly(),
                volume_serial_number: identity.volume_serial_number,
                file_id: identity.file_id,
                change_time: identity.change_time,
                last_write_time: identity.last_write_time,
                attributes: identity.attributes,
            })
        }
        #[cfg(not(any(unix, target_os = "windows")))]
        {
            let _ = file;
            Ok(Self {
                length: metadata.len(),
                modified: metadata.modified()?,
                readonly: metadata.permissions().readonly(),
            })
        }
    }
}
