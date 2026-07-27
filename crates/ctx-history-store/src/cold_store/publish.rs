//! Adjacent-name publication, retirement and recovery.
//!
//! Every mutation of the destination name goes through this module: the
//! absent-target-only link that publishes a generation, the identity-checked
//! retirement that makes room for it, and the recovery the next lock owner runs
//! over names an interrupted publication left behind.

use std::{
    ffi::OsString,
    fs::{self, File},
    path::{Path, PathBuf},
};

use same_file::Handle;
use uuid::Uuid;

use super::preflight::{create_absent_hard_link_with, HardLinkOutcome};
use super::{COLD_RETIRED_TAIL, COLD_STAGE_MARKER, DATABASE_SIDECARS};
use crate::Result;

pub(super) fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value: OsString = path.as_os_str().to_owned();
    value.push(suffix);
    PathBuf::from(value)
}

pub(super) fn adjacent_stage_path(target: &Path) -> PathBuf {
    loop {
        let candidate = append_suffix(
            target,
            &format!("{COLD_STAGE_MARKER}{}.sqlite", Uuid::new_v4()),
        );
        if !candidate.exists() {
            return candidate;
        }
    }
}

/// Adjacent name that holds a retired generation across publication.
///
/// It uses the ordinary stage marker so an interrupted install leaves a name
/// this builder already recognizes, recovers, and cleans up.
pub(super) fn adjacent_retired_path(target: &Path) -> PathBuf {
    loop {
        let candidate = append_suffix(
            target,
            &format!("{COLD_STAGE_MARKER}{}{COLD_RETIRED_TAIL}", Uuid::new_v4()),
        );
        if !candidate.exists() {
            return candidate;
        }
    }
}

pub(super) fn stage_sidecars(path: &Path) -> Vec<PathBuf> {
    let mut paths = DATABASE_SIDECARS
        .into_iter()
        .map(|suffix| append_suffix(path, suffix))
        .collect::<Vec<_>>();
    for suffix in [
        ".event-search-bulk.lock.sqlite",
        ".source-inventory.lock.sqlite",
        ".migration.lock.sqlite",
    ] {
        let lock = append_suffix(path, suffix);
        paths.push(lock.clone());
        for sidecar in DATABASE_SIDECARS {
            paths.push(append_suffix(&lock, sidecar));
        }
    }
    paths
}

pub(super) fn remove_stage_sidecars(path: &Path) {
    for sidecar in stage_sidecars(path) {
        let _ = fs::remove_file(sidecar);
    }
}

/// Drops the rollback and write-ahead sidecars of a database name that no
/// longer exists. Leaving them adjacent would attach the retired generation's
/// journal state to the newly published database of the same name.
pub(super) fn remove_database_sidecars(path: &Path) {
    for suffix in DATABASE_SIDECARS {
        let _ = fs::remove_file(append_suffix(path, suffix));
    }
}

pub(super) fn remove_path_if_same(path: &Path, identity: &Handle) {
    let matches = fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_file())
        .unwrap_or(false)
        && Handle::from_path(path)
            .map(|current| current == *identity)
            .unwrap_or(false);
    if matches {
        let _ = fs::remove_file(path);
    }
}

#[cfg(unix)]
pub(super) fn fsync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(target_os = "windows")]
pub(super) fn fsync_directory(path: &Path) -> Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?
        .sync_all()?;
    Ok(())
}

pub(super) fn install_same_filesystem(stage: &Path, target: &Path) -> Result<()> {
    link_absent(stage, target)
}

/// Links `source` onto an absent `target`, failing closed when any other object
/// already owns the destination name.
pub(super) fn link_absent(source: &Path, target: &Path) -> Result<()> {
    match create_absent_hard_link_with(source, target, |source, target| {
        fs::hard_link(source, target)
    })? {
        HardLinkOutcome::Linked => Ok(()),
        HardLinkOutcome::Unsupported => Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "adjacent absent-target hard-link publication is unsupported",
        )
        .into()),
    }
}

/// Resolves a generation retired by an interrupted or failed publication.
///
/// The lock owner runs this before admission. It never deletes a generation it
/// cannot prove redundant, and returns whether the destination is admissible:
///
/// * target absent, one retired name — the build died between unlinking the old
///   generation and linking the new one. Restore it and drop the backup name.
/// * target present and the same object as a retired name — the build died
///   between creating the backup link and unlinking the original. The backup is
///   a redundant second name for a live object, so drop it.
/// * target present and a *different* object from a retired name — another
///   owner took the destination and the retired generation is real data nothing
///   else will claim. Keep it and decline the rebuild.
pub(super) fn restore_retired_target(
    parent: &Path,
    target_name: &std::ffi::OsStr,
    target_path: &Path,
) -> Result<bool> {
    let mut retired = Vec::new();
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        if !is_retired_generation_name(target_name, &entry.file_name()) {
            continue;
        }
        if fs::symlink_metadata(entry.path())?.file_type().is_file() {
            retired.push(entry.path());
        }
    }
    if retired.is_empty() {
        return Ok(true);
    }
    let Some(target) = Handle::from_path(target_path).ok() else {
        // More than one retired name cannot come from this serialized builder,
        // so an ambiguous set is retained rather than guessed at.
        let [retired_path] = retired.as_slice() else {
            return Ok(false);
        };
        link_absent(retired_path, target_path)?;
        fsync_directory(parent)?;
        let _ = fs::remove_file(retired_path);
        let _ = fsync_directory(parent);
        return Ok(true);
    };
    let mut admissible = true;
    for retired_path in &retired {
        if Handle::from_path(retired_path).is_ok_and(|current| current == target) {
            let _ = fs::remove_file(retired_path);
        } else {
            admissible = false;
        }
    }
    let _ = fsync_directory(parent);
    Ok(admissible)
}

pub(super) fn is_retired_generation_name(
    target_name: &std::ffi::OsStr,
    entry_name: &std::ffi::OsStr,
) -> bool {
    stage_marker_uuid_suffix(target_name, entry_name)
        .is_some_and(|suffix| suffix == COLD_RETIRED_TAIL.as_bytes())
}

pub(super) fn cleanup_orphaned_stage_files(
    parent: &Path,
    target_name: &std::ffi::OsStr,
) -> Result<()> {
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        if !is_exact_orphaned_stage_name(target_name, &entry.file_name()) {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_file() {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

pub(super) fn is_exact_orphaned_stage_name(
    target_name: &std::ffi::OsStr,
    entry_name: &std::ffi::OsStr,
) -> bool {
    const SUFFIXES: [&[u8]; 16] = [
        b".sqlite",
        b".sqlite-wal",
        b".sqlite-shm",
        b".sqlite-journal",
        b".sqlite.event-search-bulk.lock.sqlite",
        b".sqlite.event-search-bulk.lock.sqlite-wal",
        b".sqlite.event-search-bulk.lock.sqlite-shm",
        b".sqlite.event-search-bulk.lock.sqlite-journal",
        b".sqlite.source-inventory.lock.sqlite",
        b".sqlite.source-inventory.lock.sqlite-wal",
        b".sqlite.source-inventory.lock.sqlite-shm",
        b".sqlite.source-inventory.lock.sqlite-journal",
        b".sqlite.migration.lock.sqlite",
        b".sqlite.migration.lock.sqlite-wal",
        b".sqlite.migration.lock.sqlite-shm",
        b".sqlite.migration.lock.sqlite-journal",
    ];
    // A retired generation is a real database, never an orphan. Only
    // `restore_retired_target` removes one, and only once it is proven
    // redundant.
    stage_marker_uuid_suffix(target_name, entry_name)
        .is_some_and(|suffix| SUFFIXES.contains(&suffix))
}

/// Splits an adjacent stage-marked name into its trailing suffix, proving the
/// nonce between the marker and the suffix is a UUID this builder minted.
pub(super) fn stage_marker_uuid_suffix<'a>(
    target_name: &std::ffi::OsStr,
    entry_name: &'a std::ffi::OsStr,
) -> Option<&'a [u8]> {
    let rest = entry_name
        .as_encoded_bytes()
        .strip_prefix(target_name.as_encoded_bytes())?
        .strip_prefix(COLD_STAGE_MARKER.as_bytes())?;
    let (uuid, suffix) = rest.split_at_checked(36)?;
    std::str::from_utf8(uuid)
        .ok()
        .and_then(|value| Uuid::parse_str(value).ok())?;
    Some(suffix)
}

#[cfg(unix)]
pub(super) fn link_count(path: &Path) -> Result<Option<u64>> {
    use std::os::unix::fs::MetadataExt;
    Ok(Some(fs::metadata(path)?.nlink()))
}

#[cfg(target_os = "windows")]
pub(super) fn link_count(_path: &Path) -> Result<Option<u64>> {
    // Stable file-ID equality on both names proves that CreateHardLinkW
    // published the exact staged object. Rust 1.88 does not expose the link
    // count needed for the additional Unix invariant.
    Ok(None)
}
