use std::{
    fs::{self, File},
    io::Read as _,
    path::Path,
};

use anyhow::{bail, Context, Result};

#[derive(Clone, Copy)]
pub(super) enum StableFileKind {
    Data,
    Executable,
}

/// Reads one stable, bounded regular file without following a final symlink.
///
/// `None` means the named file did not exist. Every other unsafe or unreadable
/// state is an error so callers can distinguish absence from corruption.
pub(super) fn read_stable_file(
    path: &Path,
    label: &str,
    max_bytes: u64,
    kind: StableFileKind,
) -> Result<Option<Vec<u8>>> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;

        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;

        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("open {label} {}", path.display()))
        }
    };
    validate_stable_file(path, label, &file, kind)?;
    let length = file.metadata()?.len();
    if length > max_bytes {
        bail!("{label} exceeds {max_bytes} bytes: {}", path.display());
    }
    let mut bytes = Vec::with_capacity(usize::try_from(length).unwrap_or(0));
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .with_context(|| format!("read {label} {}", path.display()))?;
    if bytes.len() as u64 > max_bytes {
        bail!("{label} exceeds {max_bytes} bytes: {}", path.display());
    }
    Ok(Some(bytes))
}

#[cfg(unix)]
fn validate_stable_file(path: &Path, label: &str, file: &File, kind: StableFileKind) -> Result<()> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    let opened = file.metadata()?;
    let named = fs::symlink_metadata(path)
        .with_context(|| format!("inspect {label} {}", path.display()))?;
    let mode = opened.permissions().mode();
    if !opened.is_file()
        || named.file_type().is_symlink()
        || opened.dev() != named.dev()
        || opened.ino() != named.ino()
        || opened.nlink() != 1
        || opened.uid() != unsafe { libc::geteuid() }
        || mode & 0o022 != 0
        || (matches!(kind, StableFileKind::Executable) && mode & 0o100 == 0)
    {
        bail!(
            "{label} is not an owner-safe regular file: {}",
            path.display()
        );
    }
    Ok(())
}

#[cfg(windows)]
fn validate_stable_file(
    path: &Path,
    label: &str,
    file: &File,
    _kind: StableFileKind,
) -> Result<()> {
    use std::os::windows::fs::MetadataExt as _;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    let opened = file.metadata()?;
    let named = fs::symlink_metadata(path)
        .with_context(|| format!("inspect {label} {}", path.display()))?;
    if !opened.is_file()
        || !named.is_file()
        || opened.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || named.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        bail!("{label} is not a safe regular file: {}", path.display());
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn validate_stable_file(
    path: &Path,
    label: &str,
    file: &File,
    _kind: StableFileKind,
) -> Result<()> {
    if !file.metadata()?.is_file() {
        bail!("{label} is not a safe regular file: {}", path.display());
    }
    Ok(())
}
