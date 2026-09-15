//! User-owned executable directories may retain their configured group access.
//! Private state and replacement files have separate, stricter checks.

use std::{
    fs::{File, OpenOptions},
    io,
    os::unix::{fs::MetadataExt as _, fs::OpenOptionsExt as _},
    path::Path,
};

pub fn verify_install_directory(path: &Path) -> io::Result<()> {
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_DIRECTORY)
        .open(path)?;
    verify_install_directory_handle(&directory)
}

pub fn verify_install_directory_handle(directory: &File) -> io::Result<()> {
    let metadata = directory.metadata()?;
    if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(denied(
            "installation directory must be owned by the current user",
        ));
    }
    if metadata.mode() & 0o002 != 0 {
        return Err(denied(
            "installation directory allows other accounts to write; choose an owner-controlled install directory",
        ));
    }
    Ok(())
}

fn denied(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

#[cfg(test)]
mod tests;
