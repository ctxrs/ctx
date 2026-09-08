use std::{fs, path::Path};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use anyhow::{Context, Result};
use ctx_history_platform::platform_security::{
    establish_private_data_root, restrict_private_directory, restrict_private_file,
};

pub fn create_private_dir_all(path: &Path) -> Result<()> {
    // The existing platform owner creates missing components privately and
    // repairs only the final directory, never external pre-existing ancestors.
    establish_private_data_root(path)
        .with_context(|| format!("create private directory {}", path.display()))?;
    Ok(())
}

/// Establishes one journal directory and confirms its final containing link.
/// Call at journal initialization, including retries: existence alone does not
/// confirm a prior creator's last link. Ordinary mutable status uses the
/// existing-directory path above without these extra flushes.
pub fn create_private_dir_all_before_ack(path: &Path) -> Result<()> {
    create_private_dir_all(path)?;
    #[cfg(not(windows))]
    fs::File::open(path)
        .with_context(|| format!("open private directory {}", path.display()))?
        .sync_all()
        .with_context(|| format!("sync private directory {}", path.display()))?;
    crate::sync_private_file_parent(path)
}

pub fn private_create_new_file(path: &Path) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    options.open(path)
}

#[cfg(unix)]
pub(crate) fn private_open_existing_file_nofollow(path: &Path) -> std::io::Result<fs::File> {
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(windows)]
pub(crate) fn private_open_existing_file_nofollow(path: &Path) -> std::io::Result<fs::File> {
    ctx_history_platform::platform_security::open_verified_private_file(path)
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn private_open_existing_file_nofollow(_path: &Path) -> std::io::Result<fs::File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "private no-follow file access is unavailable on this platform",
    ))
}

#[cfg(unix)]
pub fn private_create_new_lock_file(path: &Path) -> std::io::Result<fs::File> {
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
pub fn private_create_new_lock_file(path: &Path) -> std::io::Result<fs::File> {
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
}

#[cfg(unix)]
pub fn private_open_existing_lock_file(path: &Path) -> std::io::Result<fs::File> {
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(windows)]
pub fn private_open_existing_lock_file(path: &Path) -> std::io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "ctx process lock is not a regular file",
        ));
    }
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
pub fn private_open_existing_lock_file(path: &Path) -> std::io::Result<fs::File> {
    fs::OpenOptions::new().read(true).write(true).open(path)
}

pub fn secure_private_dir_permissions(path: &Path) -> Result<()> {
    restrict_private_directory(path)
        .with_context(|| format!("secure private directory {}", path.display()))?;
    Ok(())
}

pub fn secure_private_file_permissions(path: &Path) -> Result<()> {
    restrict_private_file(path)
        .with_context(|| format!("secure private file {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests;
