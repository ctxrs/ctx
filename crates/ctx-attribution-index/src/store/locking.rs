use std::fs::{self, File};
use std::path::{Path, PathBuf};

use super::{SegmentStoreError, io_error};
use crate::filesystem;

const PUBLICATION_LOCK_FILE: &str = "attribution-manifest.publication-lock";

pub(super) struct PublicationLock {
    _file: File,
    path: PathBuf,
}
impl PublicationLock {
    pub(super) fn acquire(root: &Path) -> Result<Self, SegmentStoreError> {
        let path = root.join(PUBLICATION_LOCK_FILE);
        let file = match filesystem::create_private_file_new(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                filesystem::open(&path)
                    .map_err(|source| io_error("open publication lock", &path, source))?
            }
            Err(source) => return Err(io_error("create publication lock", &path, source)),
        };
        fs2::FileExt::try_lock_exclusive(&file).map_err(|source| {
            if source.kind() == std::io::ErrorKind::WouldBlock
                || source.raw_os_error() == fs2::lock_contended_error().raw_os_error()
            {
                SegmentStoreError::PublicationBusy
            } else {
                io_error("lock publication", &path, source)
            }
        })?;
        let lock = Self { _file: file, path };
        lock.verify_identity()?;
        Ok(lock)
    }

    pub(super) fn verify_identity(&self) -> Result<(), SegmentStoreError> {
        let named = fs::symlink_metadata(&self.path)
            .map_err(|source| io_error("stat publication lock", &self.path, source))?;
        if !named.is_file() {
            return Err(SegmentStoreError::Corrupt("publication lock replaced"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let held = self
                ._file
                .metadata()
                .map_err(|source| io_error("stat held lock", &self.path, source))?;
            if held.dev() != named.dev() || held.ino() != named.ino() || held.nlink() != 1 {
                return Err(SegmentStoreError::Corrupt("publication lock replaced"));
            }
        }
        // The Windows verified open denies pathname deletion while held.
        Ok(())
    }
}
