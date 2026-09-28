use std::{
    fs::{File, Metadata},
    io,
    os::unix::fs::MetadataExt,
};

use sha2::{Digest, Sha256};

use super::RetainedFileIdentityVersion;

pub(super) fn retained_file_identity(
    file: &File,
    metadata: &Metadata,
    version: RetainedFileIdentityVersion,
) -> io::Result<([u8; 32], [u8; 32])> {
    #[cfg(target_os = "macos")]
    let volume_uuid = boot_stable_volume_uuid(file)?;
    #[cfg(not(target_os = "macos"))]
    let volume_uuid = {
        let _ = file;
        None
    };
    Ok(unix_tokens(metadata, version, metadata.dev(), volume_uuid))
}

// Keep the mount-assigned device number separate so tests can simulate a
// reboot without changing the inode, timestamps, or persistent volume UUID.
fn unix_tokens(
    metadata: &Metadata,
    version: RetainedFileIdentityVersion,
    device: u64,
    volume_uuid: Option<[u8; 16]>,
) -> ([u8; 32], [u8; 32]) {
    let mut stable = Sha256::new();
    let mut change = Sha256::new();
    match version {
        RetainedFileIdentityVersion::SharedJsonlV1 => {
            stable.update(b"ctx-jsonl-retained-file-identity-v1\0unix-stable\0");
            change.update(b"ctx-jsonl-retained-file-identity-v1\0unix-change\0");
        }
        RetainedFileIdentityVersion::OrdinaryFileV2 => {
            stable.update(b"ctx-ordinary-file-observation-v2\0unix-stable\0");
            change.update(b"ctx-ordinary-file-observation-v2\0unix-change\0");
        }
    }
    let device = device.to_le_bytes();
    let volume: &[u8] = volume_uuid.as_ref().map_or(&device[..], |uuid| &uuid[..]);
    stable.update(volume);
    stable.update(metadata.ino().to_le_bytes());
    if version == RetainedFileIdentityVersion::OrdinaryFileV2 {
        stable.update(metadata.mode().to_le_bytes());
        change.update(volume);
        change.update(metadata.ino().to_le_bytes());
    }
    change.update(metadata.ctime().to_le_bytes());
    change.update(metadata.ctime_nsec().to_le_bytes());
    (stable.finalize().into(), change.finalize().into())
}

#[cfg(any(target_os = "macos", test))]
#[repr(C)]
struct VolumeUuidReply {
    length: u32,
    uuid: [u8; 16],
}

/// macOS assigns `st_dev` at mount time. Ask the opened file's own volume for
/// its persistent identity; no mount pathname or cached device mapping is needed.
#[cfg(target_os = "macos")]
pub(crate) fn boot_stable_volume_uuid(file: &File) -> io::Result<Option<[u8; 16]>> {
    use std::os::fd::AsRawFd;

    let mut request = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: 0,
        volattr: libc::ATTR_VOL_INFO | libc::ATTR_VOL_UUID,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    let mut reply = VolumeUuidReply {
        length: 0,
        uuid: [0; 16],
    };
    // SAFETY: `file` owns the descriptor. The C-layout buffer has room for the
    // returned length and the sole requested fixed-width UUID attribute.
    let status = unsafe {
        libc::fgetattrlist(
            file.as_raw_fd(),
            (&raw mut request).cast(),
            (&raw mut reply).cast(),
            std::mem::size_of::<VolumeUuidReply>(),
            0,
        )
    };
    let result = if status == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    };
    decode_volume_uuid(result, reply)
}

#[cfg(any(target_os = "macos", test))]
fn decode_volume_uuid(
    result: io::Result<()>,
    reply: VolumeUuidReply,
) -> io::Result<Option<[u8; 16]>> {
    match result {
        // These filesystems do not provide the requested volume attribute.
        Err(error) if matches!(error.raw_os_error(), Some(libc::EINVAL | libc::ENOTSUP)) => {
            Ok(None)
        }
        Err(error) => Err(error),
        Ok(()) if reply.length as usize == std::mem::size_of::<VolumeUuidReply>() => {
            Ok(Some(reply.uuid))
        }
        Ok(()) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid volume UUID reply length",
        )),
    }
}

#[cfg(test)]
#[path = "unix_identity_tests.rs"]
mod tests;
