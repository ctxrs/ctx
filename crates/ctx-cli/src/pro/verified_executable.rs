use std::{
    fs,
    path::{Path, PathBuf},
};

#[cfg(windows)]
use std::io::Read as _;

use anyhow::{bail, Context, Result};
#[cfg(not(windows))]
use ctx_history_core::platform_security::{
    verify_private_directory, verify_private_executable, verify_private_file,
};
#[cfg(windows)]
use ctx_history_core::platform_security::{
    verify_private_directory_handle, verify_private_file_handle,
};
use ctx_pro_host_protocol::ProFilesystemLayout;
#[cfg(any(test, ctx_pro_test_helper))]
use sha2::{Digest as _, Sha256};

pub(super) struct PreparedHelperExecution {
    program: PathBuf,
}

impl PreparedHelperExecution {
    pub(super) fn program(&self) -> &Path {
        &self.program
    }

    pub(super) fn configure_command(&self, command: &mut std::process::Command) {
        let _ = command;
    }
}

/// Holds the exact installed helper and its controlled directory chain for the
/// complete helper process lifetime.
pub(super) struct VerifiedHelperExecutable {
    path: PathBuf,
    artifact_sha256: String,
    _installation_guard: Option<Box<dyn std::any::Any>>,
    #[cfg(windows)]
    _directories: Vec<fs::File>,
    #[cfg(windows)]
    helper: fs::File,
    #[cfg(windows)]
    marker: fs::File,
}

impl VerifiedHelperExecutable {
    #[cfg(any(test, ctx_pro_test_helper))]
    pub(super) fn open_developer(path: &Path) -> Result<Self> {
        let metadata = fs::symlink_metadata(path)
            .context("pro_not_installed: inspect developer Pro helper")?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            bail!("pro_not_installed: developer Pro helper is not a regular file");
        }
        let artifact_sha256 = format!(
            "{:x}",
            Sha256::digest(fs::read(path).context("pro_not_installed: read developer Pro helper")?)
        );
        #[cfg(windows)]
        {
            let helper = open_locked_file(path)?;
            let marker = helper
                .try_clone()
                .context("pro_not_installed: clone developer Pro helper handle")?;
            Ok(Self {
                path: path.to_path_buf(),
                artifact_sha256,
                _installation_guard: None,
                _directories: Vec::new(),
                helper,
                marker,
            })
        }
        #[cfg(not(windows))]
        Ok(Self {
            path: path.to_path_buf(),
            artifact_sha256,
            _installation_guard: None,
        })
    }

    pub(super) fn open(
        data_root: &Path,
        path: &Path,
        marker: &Path,
        artifact_sha256: String,
    ) -> Result<Self> {
        let layout = ProFilesystemLayout::new(data_root);
        let expected_parent = layout.bin_dir();
        if path.parent() != Some(expected_parent.as_path())
            || marker.parent() != Some(expected_parent.as_path())
        {
            bail!("invalid_response: installed Pro helper path escaped its private root");
        }

        #[cfg(windows)]
        {
            let directories = [data_root.to_path_buf(), layout.pro_root(), expected_parent];
            let mut locked = Vec::with_capacity(directories.len());
            for directory in directories {
                let handle = open_locked_directory(&directory)?;
                verify_private_directory_handle(&handle)
                    .context("invalid_response: Pro install directory ACL is unsafe")?;
                locked.push(handle);
            }
            let helper = open_locked_file(path)?;
            let marker_file = open_locked_file(marker)?;
            verify_private_file_handle(&helper)
                .context("invalid_response: installed Pro helper ACL is unsafe")?;
            verify_private_file_handle(&marker_file)
                .context("invalid_response: installed Pro marker ACL is unsafe")?;
            Ok(Self {
                path: path.to_path_buf(),
                artifact_sha256,
                _installation_guard: None,
                _directories: locked,
                helper,
                marker: marker_file,
            })
        }
        #[cfg(not(windows))]
        {
            verify_private_directory(data_root)
                .context("invalid_response: ctx data directory permissions are unsafe")?;
            verify_private_directory(&layout.pro_root())
                .context("invalid_response: Pro directory permissions are unsafe")?;
            verify_private_directory(&expected_parent)
                .context("invalid_response: Pro install directory permissions are unsafe")?;
            verify_private_executable(path)
                .context("invalid_response: installed Pro helper permissions are unsafe")?;
            verify_private_file(marker)
                .context("invalid_response: installed Pro marker permissions are unsafe")?;
            Ok(Self {
                path: path.to_path_buf(),
                artifact_sha256,
                _installation_guard: None,
            })
        }
    }

    pub(super) fn retain_installation_guard(&mut self, guard: impl std::any::Any) {
        self._installation_guard = Some(Box::new(guard));
    }

    pub(super) fn artifact_sha256(&self) -> &str {
        &self.artifact_sha256
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn prepare_execution(&self) -> Result<PreparedHelperExecution> {
        self.verify_execution_identity()?;
        Ok(PreparedHelperExecution {
            program: self.path.clone(),
        })
    }

    /// Reopens the execution pathname immediately before spawn and confirms it
    /// still resolves to the exact retained installed handle.
    pub(super) fn verify_execution_identity(&self) -> Result<()> {
        #[cfg(windows)]
        {
            let named = open_file_handle(&self.path)?;
            verify_open_identity(&self.helper, &named, false)?;
        }
        Ok(())
    }

    pub(super) fn read_helper(&self, maximum: u64) -> Result<Vec<u8>> {
        #[cfg(windows)]
        {
            read_bounded_handle(&self.helper, maximum, "installed Pro helper")
        }
        #[cfg(not(windows))]
        read_bounded_path(&self.path, maximum, "installed Pro helper")
    }

    pub(super) fn read_marker(&self, marker_path: &Path, maximum: u64) -> Result<Vec<u8>> {
        #[cfg(windows)]
        {
            let _ = marker_path;
            read_bounded_handle(&self.marker, maximum, "installed Pro marker")
        }
        #[cfg(not(windows))]
        read_bounded_path(marker_path, maximum, "installed Pro marker")
    }
}

#[cfg(not(windows))]
fn read_bounded_path(path: &Path, maximum: u64, label: &str) -> Result<Vec<u8>> {
    let metadata =
        fs::symlink_metadata(path).with_context(|| format!("invalid_request: inspect {label}"))?;
    if !metadata.file_type().is_file() || metadata.len() > maximum {
        bail!("invalid_response: {label} is invalid");
    }
    fs::read(path).with_context(|| format!("invalid_request: read {label}"))
}

#[cfg(windows)]
fn read_bounded_handle(file: &fs::File, maximum: u64, label: &str) -> Result<Vec<u8>> {
    let metadata = file
        .metadata()
        .with_context(|| format!("invalid_request: inspect {label} handle"))?;
    if metadata.len() > maximum {
        bail!("invalid_response: {label} exceeds maximum size");
    }
    let reader = file
        .try_clone()
        .with_context(|| format!("invalid_request: clone {label} handle"))?;
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    reader
        .take(maximum.saturating_add(1))
        .read_to_end(&mut bytes)
        .with_context(|| format!("invalid_request: read {label} handle"))?;
    if bytes.len() as u64 > maximum {
        bail!("invalid_response: {label} exceeds maximum size");
    }
    Ok(bytes)
}

#[cfg(windows)]
fn open_locked_directory(path: &Path) -> Result<fs::File> {
    let file = open_directory_handle(path)?;
    let named = open_directory_handle(path)?;
    verify_open_identity(&file, &named, true)?;
    Ok(file)
}

#[cfg(windows)]
fn open_directory_handle(path: &Path) -> Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
    };

    fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .context("invalid_response: lock Pro install directory")
}

#[cfg(windows)]
fn open_locked_file(path: &Path) -> Result<fs::File> {
    let file = open_file_handle(path)?;
    let named = open_file_handle(path)?;
    verify_open_identity(&file, &named, false)?;
    Ok(file)
}

#[cfg(windows)]
fn open_file_handle(path: &Path) -> Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ};

    fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .context("invalid_response: lock installed Pro file")
}

#[cfg(windows)]
fn verify_open_identity(file: &fs::File, named_handle: &fs::File, directory: bool) -> Result<()> {
    use std::os::windows::fs::MetadataExt as _;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    let opened = file
        .metadata()
        .context("invalid_response: inspect locked Pro path")?;
    let named = named_handle
        .metadata()
        .context("invalid_response: inspect second locked Pro handle")?;
    let expected_type = if directory {
        opened.is_dir() && named.is_dir()
    } else {
        opened.is_file() && named.is_file()
    };
    if !expected_type
        || opened.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || named.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || windows_file_identity(file)? != windows_file_identity(named_handle)?
    {
        bail!("invalid_response: locked Pro path identity changed");
    }
    Ok(())
}

#[cfg(windows)]
fn windows_file_identity(file: &fs::File) -> Result<(u32, u64)> {
    use std::mem::MaybeUninit;
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };

    let mut information = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
    // SAFETY: the file owns a live Windows handle and the out pointer is valid.
    if unsafe {
        GetFileInformationByHandle(file.as_raw_handle() as HANDLE, information.as_mut_ptr())
    } == 0
    {
        return Err(std::io::Error::last_os_error())
            .context("invalid_response: read locked Pro path identity");
    }
    // SAFETY: the successful API call initialized the complete structure.
    let information = unsafe { information.assume_init() };
    let index =
        (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow);
    Ok((information.dwVolumeSerialNumber, index))
}

#[cfg(all(test, windows))]
mod windows_tests {
    use ctx_history_core::platform_security::{
        restrict_private_directory, restrict_private_executable, restrict_private_file,
    };

    use super::*;

    #[test]
    fn locked_verified_helper_cannot_be_replaced_before_spawn(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let parent = tempfile::tempdir()?;
        let data_root = parent.path().join("ctx-data");
        let layout = ProFilesystemLayout::new(&data_root);
        let pro = layout.pro_root();
        let bin = layout.bin_dir();
        fs::create_dir_all(&bin)?;
        for directory in [&data_root, &pro, &bin] {
            restrict_private_directory(directory)?;
        }
        let helper = bin.join("ctx-pro.exe");
        let marker = bin.join("ctx-pro.exe.install.json");
        fs::write(&helper, b"signed helper bytes")?;
        fs::write(&marker, b"signed marker bytes")?;
        restrict_private_executable(&helper)?;
        restrict_private_file(&marker)?;

        let locked = VerifiedHelperExecutable::open(
            &data_root,
            &helper,
            &marker,
            format!("{:x}", Sha256::digest(b"signed helper bytes")),
        )?;
        assert!(fs::remove_file(&helper).is_err());
        assert!(fs::rename(&helper, bin.join("replaced.exe")).is_err());
        assert!(fs::write(&helper, b"attacker replacement").is_err());
        assert_eq!(locked.read_helper(1024)?, b"signed helper bytes");
        locked.verify_execution_identity()?;
        drop(locked);
        assert!(fs::write(&helper, b"owner update after spawn").is_ok());
        Ok(())
    }
}
