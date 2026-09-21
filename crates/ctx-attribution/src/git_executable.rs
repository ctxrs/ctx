//! Exact executable binding for local-only work-graph Git reads.

#[cfg(target_os = "windows")]
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::repository_path::{BoundRepositoryRoot, identity_digest};

pub const HANDOFF_ENV: &str = "CTX_GIT_EXECUTABLE";
const MAX_EXECUTABLE_PATH_BYTES: usize = 4 * 1024;
const MAX_EXECUTABLE_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum GitExecutableError {
    #[error("the Git executable handoff is unavailable")]
    Missing,
    #[error("the Git executable handoff is invalid")]
    Invalid,
    #[error("the Git executable changed after authorization")]
    Changed,
    #[error("Git is not available on the developer PATH")]
    NotFound,
}

/// One exact, platform-native Git executable authorized by the public host.
///
/// The private helper never searches `PATH` in production. When file blame
/// requires Git, it binds the canonical executable, its containing directory,
/// filesystem identity, and bounded content digest, then revalidates those
/// bindings around every Git operation.
#[derive(Clone, Debug)]
pub struct GitExecutable {
    path: PathBuf,
    parent: BoundRepositoryRoot,
    identity: Arc<same_file::Handle>,
    length: u64,
    modified: Option<SystemTime>,
    content_digest: [u8; 32],
    identity_digest: [u8; 32],
}

impl GitExecutable {
    /// Validates the dedicated executable locator supplied by the public host.
    ///
    /// # Errors
    ///
    /// Fails when the locator is missing, non-absolute, non-native, indirect,
    /// non-executable, oversized, or not a stable regular file.
    pub fn from_handoff_environment() -> Result<Self, GitExecutableError> {
        let value = std::env::var_os(HANDOFF_ENV).ok_or(GitExecutableError::Missing)?;
        if value.is_empty() {
            return Err(GitExecutableError::Invalid);
        }
        Self::authorize(Path::new(&value))
    }

    /// Resolves and validates Git from the optional explicit locator or PATH.
    /// The same file identity checks apply in development and release builds.
    ///
    /// # Errors
    ///
    /// Fails if PATH has no valid native Git executable.
    pub fn discover() -> Result<Self, GitExecutableError> {
        match Self::from_handoff_environment() {
            Ok(executable) => return Ok(executable),
            Err(GitExecutableError::Missing) => {}
            Err(error) => return Err(error),
        }
        let search_path = std::env::var_os("PATH").ok_or(GitExecutableError::NotFound)?;
        let current = std::env::current_dir().map_err(|_| GitExecutableError::NotFound)?;
        for directory in std::env::split_paths(&search_path) {
            let directory = if directory.is_absolute() {
                directory
            } else {
                current.join(directory)
            };
            for name in executable_names() {
                let candidate = directory.join(name);
                let Ok(canonical) = fs::canonicalize(candidate) else {
                    continue;
                };
                if let Ok(executable) = Self::authorize(&canonical) {
                    return Ok(executable);
                }
            }
        }
        Err(GitExecutableError::NotFound)
    }

    /// Authorizes one already-resolved native executable.
    ///
    /// # Errors
    ///
    /// Fails if the path or filesystem object does not satisfy the handoff
    /// contract.
    pub fn authorize(path: &Path) -> Result<Self, GitExecutableError> {
        validate_path(path)?;
        let canonical = fs::canonicalize(path).map_err(|_| GitExecutableError::Invalid)?;
        if canonical != path || path_bytes(&canonical) > MAX_EXECUTABLE_PATH_BYTES {
            return Err(GitExecutableError::Invalid);
        }
        let parent_path = canonical.parent().ok_or(GitExecutableError::Invalid)?;
        let parent =
            BoundRepositoryRoot::authorize(parent_path).map_err(|_| GitExecutableError::Invalid)?;
        let metadata = executable_metadata(&canonical)?;
        let identity =
            same_file::Handle::from_path(&canonical).map_err(|_| GitExecutableError::Invalid)?;
        let content_digest = digest_executable(&canonical, metadata.len())?;
        let identity_digest = executable_identity_digest(
            &canonical,
            &parent,
            &identity,
            metadata.len(),
            &content_digest,
        );
        let executable = Self {
            path: canonical,
            parent,
            identity: Arc::new(identity),
            length: metadata.len(),
            modified: metadata.modified().ok(),
            content_digest,
            identity_digest,
        };
        executable.verify()?;
        Ok(executable)
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn identity_digest(&self) -> [u8; 32] {
        self.identity_digest
    }

    /// Revalidates directory, file identity, type, permissions, and contents.
    ///
    /// # Errors
    ///
    /// Fails closed if any authorized property changed or disappeared.
    pub fn verify(&self) -> Result<(), GitExecutableError> {
        self.verify_identity()?;
        if digest_executable(&self.path, self.length).map_err(|_| GitExecutableError::Changed)?
            != self.content_digest
        {
            return Err(GitExecutableError::Changed);
        }
        self.verify_identity()
    }

    pub(crate) fn verify_identity(&self) -> Result<(), GitExecutableError> {
        self.parent
            .verify()
            .map_err(|_| GitExecutableError::Changed)?;
        if self.path.parent() != Some(self.parent.path())
            || fs::canonicalize(&self.path).map_err(|_| GitExecutableError::Changed)? != self.path
        {
            return Err(GitExecutableError::Changed);
        }
        let metadata = executable_metadata(&self.path).map_err(|_| GitExecutableError::Changed)?;
        let identity =
            same_file::Handle::from_path(&self.path).map_err(|_| GitExecutableError::Changed)?;
        if identity != *self.identity
            || metadata.len() != self.length
            || metadata.modified().ok() != self.modified
        {
            return Err(GitExecutableError::Changed);
        }
        self.parent
            .verify()
            .map_err(|_| GitExecutableError::Changed)
    }
}

fn executable_metadata(path: &Path) -> Result<fs::Metadata, GitExecutableError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| GitExecutableError::Invalid)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() == 0
        || metadata.len() > MAX_EXECUTABLE_BYTES
    {
        return Err(GitExecutableError::Invalid);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(GitExecutableError::Invalid);
        }
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::fs::MetadataExt as _;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(GitExecutableError::Invalid);
        }
    }
    Ok(metadata)
}

fn digest_executable(path: &Path, expected_bytes: u64) -> Result<[u8; 32], GitExecutableError> {
    if expected_bytes == 0 || expected_bytes > MAX_EXECUTABLE_BYTES {
        return Err(GitExecutableError::Invalid);
    }
    let file = File::open(path).map_err(|_| GitExecutableError::Invalid)?;
    let mut bounded = file.take(MAX_EXECUTABLE_BYTES.saturating_add(1));
    let mut hash = Sha256::new();
    let copied = io::copy(&mut bounded, &mut DigestWriter(&mut hash))
        .map_err(|_| GitExecutableError::Invalid)?;
    if copied != expected_bytes || copied > MAX_EXECUTABLE_BYTES {
        return Err(GitExecutableError::Invalid);
    }
    Ok(hash.finalize().into())
}

struct DigestWriter<'a>(&'a mut Sha256);

impl io::Write for DigestWriter<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0.update(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn executable_identity_digest(
    path: &Path,
    parent: &BoundRepositoryRoot,
    identity: &same_file::Handle,
    length: u64,
    content_digest: &[u8; 32],
) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"ctx-pro-git-executable-v1\0");
    hash.update((path_bytes(path) as u64).to_be_bytes());
    hash.update(path.as_os_str().as_encoded_bytes());
    hash.update(parent.identity_bytes());
    hash.update(identity_digest(identity));
    hash.update(length.to_be_bytes());
    hash.update(content_digest);
    hash.finalize().into()
}

fn validate_path(path: &Path) -> Result<(), GitExecutableError> {
    if !path.is_absolute() || path_bytes(path) > MAX_EXECUTABLE_PATH_BYTES {
        return Err(GitExecutableError::Invalid);
    }
    #[cfg(target_os = "windows")]
    {
        use std::path::Prefix;
        let mut components = path.components();
        let valid_prefix = matches!(
            components.next(),
            Some(Component::Prefix(prefix))
                if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_))
        );
        if !valid_prefix
            || !matches!(components.next(), Some(Component::RootDir))
            || path
                .extension()
                .and_then(OsStr::to_str)
                .is_none_or(|extension| !extension.eq_ignore_ascii_case("exe"))
            || components.any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(GitExecutableError::Invalid);
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let mut components = path.components();
        if !matches!(components.next(), Some(Component::RootDir))
            || components.any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(GitExecutableError::Invalid);
        }
    }
    Ok(())
}

fn path_bytes(path: &Path) -> usize {
    path.as_os_str().as_encoded_bytes().len()
}

#[cfg(target_os = "windows")]
fn executable_names() -> &'static [&'static str] {
    &["git.exe"]
}

#[cfg(not(target_os = "windows"))]
fn executable_names() -> &'static [&'static str] {
    &["git"]
}

#[cfg(test)]
#[path = "git_executable_tests.rs"]
mod tests;
