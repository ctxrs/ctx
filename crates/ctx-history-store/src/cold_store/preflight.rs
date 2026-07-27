use std::{
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
};

use same_file::Handle;
use uuid::Uuid;

use super::{append_suffix, link_count};
use crate::{Result, StoreError};

pub(super) const COLD_PROBE_MARKER: &str = ".ctx-native-cold-probe-";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ColdTargetState {
    Absent,
    ExistingRegular,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HardLinkOutcome {
    Linked,
    Unsupported,
}

pub(super) fn cold_target_state(path: &Path) -> Result<ColdTargetState> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata_is_plain_regular(&metadata) => {
            Ok(ColdTargetState::ExistingRegular)
        }
        Ok(_) => Err(StoreError::ColdStoreTargetIneligible(path.to_path_buf())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(ColdTargetState::Absent),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn prove_adjacent_hard_link_with<HardLink>(
    target: &Path,
    hard_link: HardLink,
) -> Result<bool>
where
    HardLink: FnOnce(&Path, &Path) -> std::io::Result<()>,
{
    let (source_path, probe_target_path, source_file) = create_probe_source(target)?;
    let source_identity = Handle::from_file(source_file)?;
    let mut cleanup = ProbeCleanup::new(
        source_path.clone(),
        probe_target_path.clone(),
        &source_identity,
    );
    revalidate_probe_identity(&source_path, &source_identity, 1)?;

    match create_absent_hard_link_with(&source_path, &probe_target_path, hard_link)? {
        HardLinkOutcome::Unsupported => {
            cleanup.finish()?;
            Ok(false)
        }
        HardLinkOutcome::Linked => {
            revalidate_probe_identity(&source_path, &source_identity, 2)?;
            revalidate_probe_identity(&probe_target_path, &source_identity, 2)?;
            cleanup.finish()?;
            Ok(true)
        }
    }
}

pub(super) fn create_absent_hard_link_with<HardLink>(
    source: &Path,
    target: &Path,
    hard_link: HardLink,
) -> Result<HardLinkOutcome>
where
    HardLink: FnOnce(&Path, &Path) -> std::io::Result<()>,
{
    match hard_link(source, target) {
        Ok(()) => Ok(HardLinkOutcome::Linked),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            Err(StoreError::ColdStoreTargetChanged(target.to_path_buf()))
        }
        Err(error) if hard_link_is_unsupported(&error) => Ok(HardLinkOutcome::Unsupported),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn hard_link_is_unsupported(error: &std::io::Error) -> bool {
    if matches!(
        error.kind(),
        std::io::ErrorKind::Unsupported | std::io::ErrorKind::CrossesDevices
    ) {
        return true;
    }
    #[cfg(target_os = "windows")]
    {
        // ERROR_INVALID_FUNCTION and ERROR_NOT_SUPPORTED. Access-denied
        // errors intentionally remain fail-closed.
        matches!(error.raw_os_error(), Some(1 | 50))
    }
    #[cfg(not(target_os = "windows"))]
    {
        false
    }
}

fn create_probe_source(target: &Path) -> Result<(PathBuf, PathBuf, std::fs::File)> {
    loop {
        let nonce = Uuid::new_v4();
        let source_path = append_suffix(target, &format!("{COLD_PROBE_MARKER}{nonce}.source"));
        let target_path = append_suffix(target, &format!("{COLD_PROBE_MARKER}{nonce}.target"));
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&source_path)
        {
            Ok(file) => return Ok((source_path, target_path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
}

fn revalidate_probe_identity(path: &Path, identity: &Handle, expected_links: u64) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(|_| StoreError::ColdStoreInvalidState)?;
    if !metadata_is_plain_regular(&metadata)
        || Handle::from_path(path)
            .map(|current| current != *identity)
            .unwrap_or(true)
        || link_count(path)?.is_some_and(|actual| actual != expected_links)
    {
        return Err(StoreError::ColdStoreInvalidState);
    }
    let after = fs::symlink_metadata(path).map_err(|_| StoreError::ColdStoreInvalidState)?;
    if !metadata_is_plain_regular(&after)
        || Handle::from_path(path)
            .map(|current| current != *identity)
            .unwrap_or(true)
    {
        return Err(StoreError::ColdStoreInvalidState);
    }
    Ok(())
}

fn metadata_is_plain_regular(metadata: &std::fs::Metadata) -> bool {
    if !metadata.file_type().is_file() {
        return false;
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
    }
    #[cfg(not(target_os = "windows"))]
    {
        true
    }
}

struct ProbeCleanup<'a> {
    source_path: PathBuf,
    target_path: PathBuf,
    identity: &'a Handle,
    active: bool,
}

impl<'a> ProbeCleanup<'a> {
    fn new(source_path: PathBuf, target_path: PathBuf, identity: &'a Handle) -> Self {
        Self {
            source_path,
            target_path,
            identity,
            active: true,
        }
    }

    fn finish(&mut self) -> Result<()> {
        remove_probe_path_if_same(&self.target_path, self.identity)?;
        revalidate_probe_identity(&self.source_path, self.identity, 1)?;
        remove_probe_path_if_same(&self.source_path, self.identity)?;
        self.active = false;
        Ok(())
    }
}

impl Drop for ProbeCleanup<'_> {
    fn drop(&mut self) {
        if self.active {
            remove_probe_path_if_same_best_effort(&self.target_path, self.identity);
            remove_probe_path_if_same_best_effort(&self.source_path, self.identity);
        }
    }
}

fn remove_probe_path_if_same(path: &Path, identity: &Handle) -> Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
        Ok(metadata) if metadata_is_plain_regular(&metadata) => {}
        Ok(_) => return Err(StoreError::ColdStoreInvalidState),
    }
    if Handle::from_path(path)
        .map(|current| current != *identity)
        .unwrap_or(true)
    {
        return Err(StoreError::ColdStoreInvalidState);
    }
    fs::remove_file(path)?;
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
        Ok(_) => Err(StoreError::ColdStoreInvalidState),
    }
}

fn remove_probe_path_if_same_best_effort(path: &Path, identity: &Handle) {
    let matches = fs::symlink_metadata(path)
        .map(|metadata| metadata_is_plain_regular(&metadata))
        .unwrap_or(false)
        && Handle::from_path(path)
            .map(|current| current == *identity)
            .unwrap_or(false);
    if matches {
        let _ = fs::remove_file(path);
    }
}
