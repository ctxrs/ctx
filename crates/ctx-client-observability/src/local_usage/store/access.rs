use std::{io, path::Path};

use super::{establish_private_data_root, verify_private_directory_and_owner, UsageStoreError};

#[cfg(unix)]
use std::{
    collections::HashSet,
    os::unix::fs::MetadataExt as _,
    sync::{LazyLock, Mutex},
};

#[cfg(unix)]
static ACTIVE_ROOTS: LazyLock<Mutex<HashSet<(u64, u64)>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

// POSIX locks belong to the process, not the descriptor. Even a read-only
// usage report must not close a family descriptor while this process writes
// that root. SQLite still owns coordination with other processes.
pub(super) struct RootAccess {
    #[cfg(unix)]
    identity: (u64, u64),
}

impl RootAccess {
    pub(super) fn acquire(path: &Path, create: bool) -> Result<Option<Self>, UsageStoreError> {
        let parent = path.parent().ok_or(UsageStoreError::SchemaIdentity)?;
        match parent.symlink_metadata() {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound && create => {
                establish_private_data_root(parent)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        }
        verify_private_directory_and_owner(parent)?;
        #[cfg(unix)]
        {
            let metadata = parent.symlink_metadata()?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(UsageStoreError::SchemaIdentity);
            }
            let identity = (metadata.dev(), metadata.ino());
            let busy = || {
                UsageStoreError::Io(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "local usage store is busy",
                ))
            };
            let mut active = ACTIVE_ROOTS.lock().map_err(|_| busy())?;
            if !active.insert(identity) {
                return Err(busy());
            }
            Ok(Some(Self { identity }))
        }
        #[cfg(not(unix))]
        Ok(Some(Self {}))
    }
}

#[cfg(unix)]
impl Drop for RootAccess {
    fn drop(&mut self) {
        ACTIVE_ROOTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.identity);
    }
}
