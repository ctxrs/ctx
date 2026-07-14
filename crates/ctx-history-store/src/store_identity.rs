use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::{Result, StoreError};

pub(crate) struct CanonicalStoreIdentity {
    canonical_path: PathBuf,
    digest: String,
}

impl CanonicalStoreIdentity {
    pub(crate) fn open_target(path: &Path, create: bool) -> Result<Self> {
        if create {
            match OpenOptions::new().write(true).create_new(true).open(path) {
                Ok(file) => drop(file),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        let canonical_path = fs::canonicalize(path)?;
        let metadata = fs::metadata(&canonical_path)?;
        if !metadata.is_file() {
            return Err(StoreError::UnsafeStoreIdentity);
        }

        let mut digest = Sha256::new();
        digest.update(b"ctx-canonical-store-v2");
        hash_stable_file_identity(&mut digest, &canonical_path, &metadata)?;
        Ok(Self {
            canonical_path,
            digest: hex_digest(digest.finalize().as_slice()),
        })
    }

    pub(crate) fn canonical_path(&self) -> &Path {
        &self.canonical_path
    }

    pub(crate) fn digest(&self) -> &str {
        &self.digest
    }

    pub(crate) fn private_root(&self) -> PathBuf {
        std::env::temp_dir().join(format!(".ctx-store-{}", self.digest))
    }
}

#[cfg(unix)]
fn hash_stable_file_identity(
    digest: &mut Sha256,
    _path: &Path,
    metadata: &fs::Metadata,
) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    digest.update(metadata.dev().to_be_bytes());
    digest.update(metadata.ino().to_be_bytes());
    Ok(())
}

#[cfg(windows)]
fn hash_stable_file_identity(
    digest: &mut Sha256,
    path: &Path,
    _metadata: &fs::Metadata,
) -> Result<()> {
    use std::{fs::File, os::windows::io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };

    let file = File::open(path)?;
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut info) };
    if ok == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    digest.update(info.dwVolumeSerialNumber.to_be_bytes());
    digest.update(info.nFileIndexHigh.to_be_bytes());
    digest.update(info.nFileIndexLow.to_be_bytes());
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn hash_stable_file_identity(
    digest: &mut Sha256,
    path: &Path,
    _metadata: &fs::Metadata,
) -> Result<()> {
    let value = path.to_string_lossy();
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value.as_bytes());
    Ok(())
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut value = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(&mut value, "{byte:02x}");
    }
    value
}
