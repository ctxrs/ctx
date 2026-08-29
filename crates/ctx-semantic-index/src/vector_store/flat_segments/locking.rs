use std::{
    fs::{File, OpenOptions},
    path::Path,
};

use super::{io_error, transaction_lock_path, FlatResult};

/// Coordinates one semantic control/flat snapshot with source-backed writers.
/// The lock file is part of every initialized flat store; passive callers open
/// it read-only and therefore cannot create storage artifacts.
pub(crate) struct FlatStoreCoordinationGuard {
    _lock: FileLock,
}

impl FlatStoreCoordinationGuard {
    pub(crate) fn lock_passive_snapshot(root: &Path) -> FlatResult<Self> {
        Ok(Self {
            _lock: FileLock::shared(&transaction_lock_path(root))?,
        })
    }

    pub(crate) fn lock_control_writer(root: &Path) -> FlatResult<Self> {
        Ok(Self {
            _lock: FileLock::exclusive(&transaction_lock_path(root))?,
        })
    }
}

pub(super) struct FileLock {
    file: File,
}

impl FileLock {
    pub(super) fn shared(path: &Path) -> FlatResult<Self> {
        let file = open_lock(path, false)?;
        fs2::FileExt::lock_shared(&file).map_err(|source| io_error("lock shared", path, source))?;
        Ok(Self { file })
    }

    pub(super) fn exclusive(path: &Path) -> FlatResult<Self> {
        let file = open_lock(path, true)?;
        fs2::FileExt::lock_exclusive(&file)
            .map_err(|source| io_error("lock exclusive", path, source))?;
        Ok(Self { file })
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

pub(super) fn open_lock(path: &Path, create: bool) -> FlatResult<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    if create {
        options.write(true).create(true);
    }
    options
        .open(path)
        .map_err(|source| io_error("open flat writer lock", path, source))
}
