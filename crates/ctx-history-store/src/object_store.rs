#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::SystemTime,
};

use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{Result, StoreError};

pub(crate) const OBJECTS_DIR: &str = "objects";
pub(crate) const SPOOL_DIR: &str = "spool";
const LEGACY_HISTORY_DIR_NAME: &str = "work-record";
pub(crate) const LEGACY_BLOBS_DIR: &str = "blobs";
const LEGACY_INBOX_DIR: &str = "inbox";

pub(crate) fn migrate_legacy_history_layout(data_root: &Path) -> Result<bool> {
    let legacy_dir = data_root.join(LEGACY_HISTORY_DIR_NAME);
    if !legacy_dir.is_dir() {
        return Ok(false);
    }

    let mut moves = Vec::new();
    push_legacy_move(
        &mut moves,
        legacy_dir.join("work.sqlite"),
        data_root.join("work.sqlite"),
    );
    push_legacy_move(
        &mut moves,
        legacy_dir.join("config.toml"),
        data_root.join("config.toml"),
    );
    push_legacy_move(&mut moves, legacy_dir.join("logs"), data_root.join("logs"));
    push_legacy_move(
        &mut moves,
        legacy_dir.join("device.json"),
        data_root.join("device.json"),
    );

    let object_candidates = [
        legacy_dir.join(OBJECTS_DIR),
        legacy_dir.join(LEGACY_BLOBS_DIR),
    ];
    let spool_candidates = [
        legacy_dir.join(SPOOL_DIR),
        legacy_dir.join(LEGACY_INBOX_DIR),
    ];
    if multiple_existing_paths(&object_candidates) || multiple_existing_paths(&spool_candidates) {
        return Ok(false);
    }

    if let Some(object_source) = unique_existing_path(&object_candidates) {
        push_legacy_move(&mut moves, object_source, data_root.join(OBJECTS_DIR));
    }

    if let Some(spool_source) = unique_existing_path(&spool_candidates) {
        push_legacy_move(&mut moves, spool_source, data_root.join(SPOOL_DIR));
    }

    if moves.is_empty() || moves.iter().any(|(_, dest)| dest.exists()) {
        return Ok(false);
    }

    for (source, dest) in moves {
        fs::rename(source, dest)?;
    }
    let _ = fs::remove_dir(&legacy_dir);
    Ok(true)
}

fn push_legacy_move(moves: &mut Vec<(PathBuf, PathBuf)>, source: PathBuf, dest: PathBuf) {
    if source.exists() {
        moves.push((source, dest));
    }
}

fn unique_existing_path(paths: &[PathBuf]) -> Option<PathBuf> {
    let mut existing = paths.iter().filter(|path| path.exists());
    let first = existing.next()?.clone();
    if existing.next().is_some() {
        return None;
    }
    Some(first)
}

fn multiple_existing_paths(paths: &[PathBuf]) -> bool {
    paths.iter().filter(|path| path.exists()).take(2).count() > 1
}

pub(crate) fn object_relative_path(hash: &str) -> String {
    let shard = &hash[..2];
    format!("{OBJECTS_DIR}/{shard}/{hash}")
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut value = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut value, "{byte:02x}");
    }
    value
}

pub(crate) fn sha256_reader_hex(reader: &mut impl Read) -> std::io::Result<(String, u64)> {
    let mut digest = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
        total = total.saturating_add(read as u64);
    }
    let digest = digest.finalize();
    let mut value = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut value, "{byte:02x}");
    }
    Ok((value, total))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileSnapshot {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(windows)]
    volume_and_index: Option<(u32, u64)>,
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WindowsFileIdentity {
    pub(crate) volume_serial: u32,
    pub(crate) file_index: u64,
    pub(crate) links: u32,
}

#[cfg(windows)]
pub(crate) fn windows_file_identity(file: &fs::File) -> std::io::Result<WindowsFileIdentity> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };

    let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
    // SAFETY: the handle remains owned by `file`; Windows initializes `info`
    // completely on a nonzero return and does not retain either pointer.
    let success =
        unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, info.as_mut_ptr()) };
    if success == 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: a nonzero API result guarantees initialization.
    let info = unsafe { info.assume_init() };
    Ok(WindowsFileIdentity {
        volume_serial: info.dwVolumeSerialNumber,
        file_index: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        links: info.nNumberOfLinks,
    })
}

pub(crate) fn file_snapshot(file: &fs::File) -> std::io::Result<FileSnapshot> {
    let snapshot = file_snapshot_from_metadata(&file.metadata()?)?;
    #[cfg(windows)]
    {
        let identity = windows_file_identity(file)?;
        let mut snapshot = snapshot;
        snapshot.volume_and_index = Some((identity.volume_serial, identity.file_index));
        return Ok(snapshot);
    }
    #[cfg(not(windows))]
    Ok(snapshot)
}

fn file_snapshot_from_metadata(metadata: &fs::Metadata) -> std::io::Result<FileSnapshot> {
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    Ok(FileSnapshot {
        len: metadata.len(),
        modified: metadata.modified().ok(),
        #[cfg(unix)]
        device: metadata.dev(),
        #[cfg(unix)]
        inode: metadata.ino(),
        #[cfg(windows)]
        volume_and_index: None,
    })
}

pub(crate) fn path_matches_file_snapshot(
    path: &Path,
    expected: &FileSnapshot,
) -> std::io::Result<bool> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Ok(false);
    }
    let current = fs::File::open(path)?;
    Ok(file_snapshot(&current)? == *expected)
}

pub(crate) fn ensure_regular_blob_file(id: Uuid, path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_file() {
        Ok(())
    } else {
        Err(StoreError::ArchiveArtifactNonRegularFile {
            id,
            path: path.to_path_buf(),
        })
    }
}

#[cfg(unix)]
pub(crate) fn restrict_private_dir(path: &Path) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn restrict_private_dir(_path: &Path) -> Result<()> {
    // Windows privacy follows the parent directory's inherited ACL. Public
    // documentation does not claim that this helper installs an owner-only ACL.
    Ok(())
}

#[cfg(unix)]
pub(crate) fn restrict_private_file(path: &Path) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn restrict_private_file(_path: &Path) -> Result<()> {
    // Windows privacy follows the parent directory's inherited ACL. Public
    // documentation does not claim that this helper installs an owner-only ACL.
    Ok(())
}
