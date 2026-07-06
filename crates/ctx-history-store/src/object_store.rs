#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use std::{
    fs,
    path::{Path, PathBuf},
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

#[derive(Debug, Default)]
pub(crate) struct BlobWriteGuard {
    created_paths: Vec<PathBuf>,
    committed: bool,
}

impl BlobWriteGuard {
    pub(crate) fn commit(&mut self) {
        self.committed = true;
        self.created_paths.clear();
    }
}

impl Drop for BlobWriteGuard {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        for path in self.created_paths.iter().rev() {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(unix)]
pub(crate) fn restrict_private_dir(path: &Path) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn restrict_private_dir(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
pub(crate) fn restrict_private_file(path: &Path) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn restrict_private_file(_path: &Path) -> Result<()> {
    Ok(())
}
