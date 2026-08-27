use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io,
    path::{Component, Path, PathBuf},
};

#[cfg(windows)]
use std::ffi::OsStr;

use anyhow::{anyhow, bail, Context, Result};

#[cfg(windows)]
use super::path_identity::windows_disk_path_identity;

/// A process-owned lock for mutations of one installed executable.
///
/// The lock file is intentionally persistent. Ownership is represented only by
/// the live OS lock on `file`; no PID, timestamp, stale-owner deletion, or
/// pathname unlink participates in coordination.
#[cfg_attr(windows, allow(dead_code))]
pub(in crate::upgrade) struct InstallationLock {
    _owner: OwnerFileLock,
}

#[cfg_attr(windows, allow(dead_code))]
impl InstallationLock {
    #[cfg(windows)]
    pub(in crate::upgrade) fn acquire_for_recovery(executable: &Path) -> Result<Self> {
        Self::acquire_inner(executable, true)
    }

    #[cfg(windows)]
    fn acquire_inner(executable: &Path, allow_recovery_hardlink: bool) -> Result<Self> {
        let executable = executable_lock_identity(executable, allow_recovery_hardlink)?;
        validate_lock_executable(&executable, allow_recovery_hardlink)?;
        let path = installation_lock_path(&executable)?;
        let owner = OwnerFileLock::acquire(&path)?;
        revalidate_locked_executable(&executable, allow_recovery_hardlink)?;
        Ok(Self { _owner: owner })
    }

    pub(in crate::upgrade) fn try_acquire(executable: &Path) -> Result<Option<Self>> {
        Self::try_acquire_inner(executable, false)
    }

    pub(in crate::upgrade) fn try_acquire_for_recovery(executable: &Path) -> Result<Option<Self>> {
        Self::try_acquire_inner(executable, true)
    }

    fn try_acquire_inner(executable: &Path, allow_recovery_hardlink: bool) -> Result<Option<Self>> {
        let executable = executable_lock_identity(executable, allow_recovery_hardlink)?;
        validate_lock_executable(&executable, allow_recovery_hardlink)?;
        let path = installation_lock_path(&executable)?;
        let Some(owner) = OwnerFileLock::try_acquire(&path)? else {
            return Ok(None);
        };
        revalidate_locked_executable(&executable, allow_recovery_hardlink)?;
        Ok(Some(Self { _owner: owner }))
    }
}

fn executable_lock_identity(path: &Path, _recovery: bool) -> Result<PathBuf> {
    #[cfg(windows)]
    if _recovery {
        return canonical_recovery_executable(path);
    }
    canonical_executable(path)
}

fn validate_lock_executable(path: &Path, recovery: bool) -> Result<()> {
    #[cfg(windows)]
    if recovery && !path.try_exists()? {
        return Ok(());
    }
    validate_mutable_executable(path, recovery)
}

pub(in crate::upgrade) struct OwnerFileLock {
    file: File,
}

impl OwnerFileLock {
    #[cfg(windows)]
    pub(in crate::upgrade) fn acquire(path: &Path) -> Result<Self> {
        let file = open_lock_file(path)?;
        lock_file(&file, false)
            .with_context(|| format!("acquire ctx owner-file lock {}", path.display()))?;
        Ok(Self { file })
    }

    pub(in crate::upgrade) fn try_acquire(path: &Path) -> Result<Option<Self>> {
        let file = open_lock_file(path)?;
        if !lock_file(&file, true)
            .with_context(|| format!("try ctx owner-file lock {}", path.display()))?
        {
            return Ok(None);
        }
        Ok(Some(Self { file }))
    }
}

impl Drop for OwnerFileLock {
    fn drop(&mut self) {
        let _ = unlock_file(&self.file);
    }
}

pub(super) fn canonical_executable(path: &Path) -> Result<PathBuf> {
    validate_absolute_path(path, "ctx executable")?;
    let canonical = fs::canonicalize(path)
        .with_context(|| format!("canonicalize ctx executable {}", path.display()))?;
    validate_absolute_path(&canonical, "canonical ctx executable")?;
    let metadata = fs::symlink_metadata(&canonical)
        .with_context(|| format!("inspect ctx executable {}", canonical.display()))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        bail!(
            "ctx executable is not a safe regular file: {}",
            canonical.display()
        );
    }
    let parent = canonical
        .parent()
        .ok_or_else(|| anyhow!("ctx executable has no parent: {}", canonical.display()))?;
    let canonical_parent = fs::canonicalize(parent)
        .with_context(|| format!("canonicalize ctx executable directory {}", parent.display()))?;
    if canonical_parent != parent {
        bail!(
            "ctx executable directory is not canonical: {}",
            parent.display()
        );
    }
    Ok(canonical)
}

/// Resolves the same executable-scoped lock identity when ReplaceFileW left
/// the final pathname absent. Only the already-canonical parent and original
/// executable file name are accepted; ordinary locking still requires the
/// executable itself to exist and pass `canonical_executable`.
#[cfg(windows)]
pub(super) fn canonical_recovery_executable(path: &Path) -> Result<PathBuf> {
    validate_absolute_path(path, "ctx recovery executable")?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("ctx recovery executable has no parent: {}", path.display()))?;
    let file_name = path
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            anyhow!(
                "ctx recovery executable has no file name: {}",
                path.display()
            )
        })?;
    validate_windows_path_leaf(file_name, "ctx recovery executable")?;
    let parent_identity = windows_disk_path_identity(parent).ok_or_else(|| {
        anyhow!(
            "ctx recovery executable uses an unsupported Windows path form: {}",
            path.display()
        )
    })?;
    let canonical_parent = fs::canonicalize(parent).with_context(|| {
        format!(
            "canonicalize ctx recovery executable directory {}",
            parent.display()
        )
    })?;
    validate_absolute_path(
        &canonical_parent,
        "canonical ctx recovery executable directory",
    )?;
    let canonical_parent_identity =
        windows_disk_path_identity(&canonical_parent).ok_or_else(|| {
            anyhow!(
                "canonical ctx recovery executable uses an unsupported Windows path form: {}",
                canonical_parent.display()
            )
        })?;
    if canonical_parent_identity != parent_identity {
        bail!(
            "ctx recovery executable directory is not canonical: {}",
            parent.display()
        );
    }
    let candidate = canonical_parent.join(file_name);
    match fs::symlink_metadata(&candidate) {
        Ok(_) => {
            let canonical = canonical_executable(&candidate)?;
            if canonical != candidate {
                bail!(
                    "ctx recovery executable changed identity: expected {}, found {}",
                    candidate.display(),
                    canonical.display()
                );
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!("inspect ctx recovery executable {}", candidate.display())
            })
        }
    }
    Ok(candidate)
}

/// Rejects Windows leaf spellings that Win32 can normalize to another name or
/// interpret as a DOS device rather than the requested regular file.
#[cfg(windows)]
pub(super) fn validate_windows_path_leaf(leaf: &OsStr, label: &str) -> Result<()> {
    use std::os::windows::ffi::OsStrExt as _;

    let units: Vec<_> = leaf.encode_wide().collect();
    let has_forbidden_unit = units.iter().any(|unit| {
        *unit <= 0x1f
            || matches!(
                *unit,
                0x22 | 0x2a | 0x2f | 0x3a | 0x3c | 0x3e | 0x3f | 0x5c | 0x7c
            )
    });
    if units.is_empty()
        || matches!(units.last(), Some(0x20 | 0x2e))
        || has_forbidden_unit
        || windows_leaf_has_reserved_device_stem(&units)
    {
        bail!(
            "{label} has an unsafe Windows file name: {}",
            Path::new(leaf).display()
        );
    }
    Ok(())
}

#[cfg(windows)]
fn windows_leaf_has_reserved_device_stem(units: &[u16]) -> bool {
    const RESERVED: &[&[u8]] = &[
        b"CON", b"PRN", b"AUX", b"NUL", b"CONIN$", b"CONOUT$", b"CLOCK$",
    ];

    let stem_end = units
        .iter()
        .position(|unit| *unit == u16::from(b'.'))
        .unwrap_or(units.len());
    let mut stem = &units[..stem_end];
    while matches!(stem.last(), Some(0x20 | 0x2e)) {
        stem = &stem[..stem.len() - 1];
    }
    if RESERVED
        .iter()
        .any(|reserved| windows_wide_eq_ascii(stem, reserved))
    {
        return true;
    }
    if stem.len() != 4
        || !(windows_wide_eq_ascii(&stem[..3], b"COM") || windows_wide_eq_ascii(&stem[..3], b"LPT"))
    {
        return false;
    }
    matches!(stem[3], 0x31..=0x39 | 0x00b2 | 0x00b3 | 0x00b9)
}

#[cfg(windows)]
fn windows_wide_eq_ascii(units: &[u16], expected: &[u8]) -> bool {
    units.len() == expected.len()
        && units.iter().zip(expected).all(|(unit, expected)| {
            *unit <= 0x7f && (*unit as u8).to_ascii_uppercase() == *expected
        })
}

fn revalidate_locked_executable(expected: &Path, allow_recovery_hardlink: bool) -> Result<()> {
    let current = executable_lock_identity(expected, allow_recovery_hardlink)?;
    if current != expected {
        bail!(
            "ctx executable changed while acquiring its installation lock: expected {}, found {}",
            expected.display(),
            current.display()
        );
    }
    validate_lock_executable(&current, allow_recovery_hardlink)
}

#[cfg(unix)]
fn validate_mutable_executable(path: &Path, allow_recovery_hardlink: bool) -> Result<()> {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect ctx executable {}", path.display()))?;
    if metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() == 0
        || (allow_recovery_hardlink && metadata.nlink() > 2)
        || (!allow_recovery_hardlink && metadata.nlink() != 1)
    {
        bail!(
            "ctx executable is not an owner-safe regular file: {}",
            path.display()
        );
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("ctx executable has no parent: {}", path.display()))?;
    let parent_metadata = fs::symlink_metadata(parent)
        .with_context(|| format!("inspect ctx executable directory {}", parent.display()))?;
    use std::os::unix::fs::PermissionsExt as _;
    if !parent_metadata.is_dir()
        || parent_metadata.file_type().is_symlink()
        || parent_metadata.uid() != unsafe { libc::geteuid() }
        || parent_metadata.permissions().mode() & 0o022 != 0
    {
        bail!(
            "ctx executable directory is not owner-safe: {}",
            parent.display()
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_mutable_executable(_path: &Path, _allow_recovery_hardlink: bool) -> Result<()> {
    Ok(())
}

fn validate_absolute_path(path: &Path, label: &str) -> Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        bail!("{label} must be a safe absolute path: {}", path.display());
    }
    Ok(())
}

pub(super) fn installation_lock_path(executable: &Path) -> Result<PathBuf> {
    let file_name = executable
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| anyhow!("ctx executable has no file name: {}", executable.display()))?;
    let mut lock_name = OsString::from(".");
    lock_name.push(file_name);
    lock_name.push(".install.lock");
    Ok(executable.with_file_name(lock_name))
}

fn open_lock_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;

        options
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};

        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options
        .open(path)
        .with_context(|| format!("open ctx installation lock {}", path.display()))?;
    validate_open_lock(path, &file)?;
    protect_open_lock(path, &file)?;
    validate_open_lock(path, &file)?;
    Ok(file)
}

#[cfg(unix)]
fn validate_open_lock(path: &Path, file: &File) -> Result<()> {
    use std::os::unix::fs::MetadataExt as _;

    let opened = file.metadata()?;
    let named = fs::symlink_metadata(path)
        .with_context(|| format!("inspect ctx installation lock {}", path.display()))?;
    if !opened.is_file()
        || named.file_type().is_symlink()
        || opened.dev() != named.dev()
        || opened.ino() != named.ino()
        || opened.nlink() != 1
        || opened.uid() != unsafe { libc::geteuid() }
    {
        bail!(
            "ctx installation lock is not an owner-safe regular file: {}",
            path.display()
        );
    }
    Ok(())
}

#[cfg(windows)]
fn validate_open_lock(path: &Path, file: &File) -> Result<()> {
    use std::os::windows::fs::MetadataExt as _;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    let opened = file.metadata()?;
    let named = fs::symlink_metadata(path)
        .with_context(|| format!("inspect ctx installation lock {}", path.display()))?;
    if !opened.is_file()
        || !named.is_file()
        || opened.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || named.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        bail!(
            "ctx installation lock is not an owner-safe regular file: {}",
            path.display()
        );
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn validate_open_lock(path: &Path, _file: &File) -> Result<()> {
    bail!(
        "ctx installation locking is unsupported on this platform: {}",
        path.display()
    )
}

#[cfg(unix)]
fn protect_open_lock(path: &Path, file: &File) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    file.set_permissions(fs::Permissions::from_mode(0o600))
        .with_context(|| format!("protect ctx installation lock {}", path.display()))
}

#[cfg(windows)]
fn protect_open_lock(path: &Path, file: &File) -> Result<()> {
    use ctx_history_platform::platform_security::verify_private_file_handle;

    ctx_history_platform::platform_security::restrict_private_file(path)
        .with_context(|| format!("protect ctx installation lock {}", path.display()))?;
    verify_private_file_handle(file)
        .with_context(|| format!("verify ctx installation lock {}", path.display()))
}

#[cfg(not(any(unix, windows)))]
fn protect_open_lock(path: &Path, _file: &File) -> Result<()> {
    bail!(
        "ctx installation locking is unsupported on this platform: {}",
        path.display()
    )
}

#[cfg(unix)]
fn lock_file(file: &File, nonblocking: bool) -> io::Result<bool> {
    use std::os::fd::AsRawFd as _;

    let operation = libc::LOCK_EX | if nonblocking { libc::LOCK_NB } else { 0 };
    loop {
        if unsafe { libc::flock(file.as_raw_fd(), operation) } == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        if nonblocking && error.kind() == io::ErrorKind::WouldBlock {
            return Ok(false);
        }
        return Err(error);
    }
}

#[cfg(windows)]
fn lock_file(file: &File, nonblocking: bool) -> io::Result<bool> {
    use std::{mem, os::windows::io::AsRawHandle as _};
    use windows_sys::Win32::{
        Foundation::{ERROR_IO_PENDING, ERROR_LOCK_VIOLATION},
        Storage::FileSystem::{LockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY},
        System::IO::OVERLAPPED,
    };

    let mut overlapped: OVERLAPPED = unsafe { mem::zeroed() };
    let flags = LOCKFILE_EXCLUSIVE_LOCK
        | if nonblocking {
            LOCKFILE_FAIL_IMMEDIATELY
        } else {
            0
        };
    let result = unsafe {
        LockFileEx(
            file.as_raw_handle(),
            flags,
            0,
            u32::MAX,
            u32::MAX,
            &mut overlapped,
        )
    };
    if result != 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if nonblocking
        && matches!(
            error.raw_os_error(),
            Some(code) if code == ERROR_IO_PENDING as i32 || code == ERROR_LOCK_VIOLATION as i32
        )
    {
        Ok(false)
    } else {
        Err(error)
    }
}

#[cfg(not(any(unix, windows)))]
fn lock_file(_file: &File, _nonblocking: bool) -> io::Result<bool> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "ctx installation locking is unsupported on this platform",
    ))
}

#[cfg(unix)]
fn unlock_file(file: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd as _;

    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn unlock_file(file: &File) -> io::Result<()> {
    use std::{mem, os::windows::io::AsRawHandle as _};
    use windows_sys::Win32::{Storage::FileSystem::UnlockFileEx, System::IO::OVERLAPPED};

    let mut overlapped: OVERLAPPED = unsafe { mem::zeroed() };
    if unsafe { UnlockFileEx(file.as_raw_handle(), 0, u32::MAX, u32::MAX, &mut overlapped) } != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(any(unix, windows)))]
fn unlock_file(_file: &File) -> io::Result<()> {
    Ok(())
}
