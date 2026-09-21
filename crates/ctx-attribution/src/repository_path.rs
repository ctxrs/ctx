//! Filesystem-identity-bound repository paths for local Git authority.

use std::fs;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use sha2::{Digest, Sha256};

const MAX_REPOSITORY_PATH_BYTES: usize = 4 * 1024;
const MAX_REPOSITORY_PATH_COMPONENTS: usize = 256;

/// An explicitly authorized directory resolved to one stable filesystem object.
///
/// Descendants are resolved relative to `canonical` and may not traverse
/// symlinks below it.
#[derive(Clone, Debug)]
pub(crate) struct BoundRepositoryRoot {
    canonical: PathBuf,
    identity: Arc<same_file::Handle>,
    identity_digest: [u8; 32],
}

#[derive(Debug)]
pub(crate) struct BoundRepositoryFile {
    path: PathBuf,
    identity: same_file::Handle,
}

impl BoundRepositoryFile {
    pub(crate) fn verify(&self) -> io::Result<()> {
        let metadata = fs::symlink_metadata(&self.path)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || same_file::Handle::from_path(&self.path)? != self.identity
        {
            return Err(invalid_path());
        }
        Ok(())
    }
}

impl BoundRepositoryRoot {
    pub(crate) fn authorize(path: &Path) -> io::Result<Self> {
        if !path.is_absolute() || path_bytes(path) > MAX_REPOSITORY_PATH_BYTES {
            return Err(invalid_path());
        }
        let canonical = fs::canonicalize(path)?;
        if path_bytes(&canonical) > MAX_REPOSITORY_PATH_BYTES {
            return Err(invalid_path());
        }
        root_metadata(&canonical)?;
        let identity = same_file::Handle::from_path(&canonical)?;
        let identity_digest = identity_digest(&identity);
        Ok(Self {
            canonical,
            identity: Arc::new(identity),
            identity_digest,
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.canonical
    }

    pub(crate) fn identity_bytes(&self) -> [u8; 32] {
        self.identity_digest
    }

    pub(crate) fn verify(&self) -> io::Result<()> {
        root_metadata(&self.canonical)?;
        let current = same_file::Handle::from_path(&self.canonical)?;
        if current != *self.identity || fs::canonicalize(&self.canonical)? != self.canonical {
            return Err(invalid_path());
        }
        Ok(())
    }

    /// Resolves one existing repository-relative file without following a
    /// symlink below the authorized root.
    pub(crate) fn existing_file(&self, relative: &Path) -> io::Result<BoundRepositoryFile> {
        self.verify()?;
        let path = self.walk_existing_descendant(relative, false)?;
        let identity = same_file::Handle::from_path(&path)?;
        self.verify()?;
        Ok(BoundRepositoryFile { path, identity })
    }

    fn walk_existing_descendant(&self, relative: &Path, directory: bool) -> io::Result<PathBuf> {
        let relative = normalized_relative(relative)?;
        let mut current = self.canonical.clone();
        for component in relative.components() {
            let Component::Normal(component) = component else {
                continue;
            };
            current.push(component);
            let metadata = fs::symlink_metadata(&current)?;
            if metadata.file_type().is_symlink() {
                return Err(invalid_path());
            }
        }
        let metadata = fs::metadata(&current)?;
        if (directory && !metadata.is_dir()) || (!directory && !metadata.is_file()) {
            return Err(invalid_path());
        }
        if fs::canonicalize(&current)? != current || !current.starts_with(&self.canonical) {
            return Err(invalid_path());
        }
        Ok(current)
    }
}

fn normalized_relative(path: &Path) -> io::Result<PathBuf> {
    if path_bytes(path) > MAX_REPOSITORY_PATH_BYTES {
        return Err(invalid_path());
    }
    let mut normalized = PathBuf::new();
    let mut component_count = 0_usize;
    for component in path.components() {
        match component {
            Component::Normal(value) => {
                component_count = component_count.saturating_add(1);
                if component_count > MAX_REPOSITORY_PATH_COMPONENTS {
                    return Err(invalid_path());
                }
                normalized.push(value);
            }
            Component::CurDir => {}
            Component::Prefix(_) | Component::RootDir | Component::ParentDir => {
                return Err(invalid_path());
            }
        }
    }
    Ok(normalized)
}

fn path_bytes(path: &Path) -> usize {
    path.as_os_str().as_encoded_bytes().len()
}

fn root_metadata(path: &Path) -> io::Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(invalid_path());
    }
    Ok(metadata)
}

pub(crate) fn identity_digest(identity: &same_file::Handle) -> [u8; 32] {
    let mut hasher = IdentityHasher(Sha256::new());
    identity.hash(&mut hasher);
    hasher.0.finalize().into()
}

struct IdentityHasher(Sha256);

impl Hasher for IdentityHasher {
    fn finish(&self) -> u64 {
        0
    }

    fn write(&mut self, bytes: &[u8]) {
        self.0.update([0]);
        self.0
            .update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
        self.0.update(bytes);
    }

    fn write_u8(&mut self, value: u8) {
        self.0.update([1, value]);
    }

    fn write_u16(&mut self, value: u16) {
        self.0.update([2]);
        self.0.update(value.to_be_bytes());
    }

    fn write_u32(&mut self, value: u32) {
        self.0.update([3]);
        self.0.update(value.to_be_bytes());
    }

    fn write_u64(&mut self, value: u64) {
        self.0.update([4]);
        self.0.update(value.to_be_bytes());
    }

    fn write_u128(&mut self, value: u128) {
        self.0.update([5]);
        self.0.update(value.to_be_bytes());
    }

    fn write_usize(&mut self, value: usize) {
        self.0.update([6]);
        self.0
            .update(u64::try_from(value).unwrap_or(u64::MAX).to_be_bytes());
    }

    fn write_i8(&mut self, value: i8) {
        self.0.update([7, value.cast_unsigned()]);
    }

    fn write_i16(&mut self, value: i16) {
        self.0.update([8]);
        self.0.update(value.to_be_bytes());
    }

    fn write_i32(&mut self, value: i32) {
        self.0.update([9]);
        self.0.update(value.to_be_bytes());
    }

    fn write_i64(&mut self, value: i64) {
        self.0.update([10]);
        self.0.update(value.to_be_bytes());
    }

    fn write_i128(&mut self, value: i128) {
        self.0.update([11]);
        self.0.update(value.to_be_bytes());
    }

    fn write_isize(&mut self, value: isize) {
        self.0.update([12]);
        self.0
            .update(i64::try_from(value).unwrap_or(i64::MAX).to_be_bytes());
    }
}

fn invalid_path() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "unsafe repository path")
}

#[cfg(test)]
#[path = "repository_path_tests.rs"]
mod tests;
