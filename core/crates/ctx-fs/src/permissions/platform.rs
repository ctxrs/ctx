use std::fs::{self, File};
use std::path::Path;

use anyhow::{Context, Result};
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::LocalFree,
    Security::{
        Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1},
        SetFileSecurityW, DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR,
    },
};

use super::{PRIVATE_DIR_MODE, PRIVATE_FILE_MODE};

#[cfg(windows)]
const WINDOWS_PRIVATE_SDDL: &str = "D:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FA;;;OW)";

pub(super) fn apply_private_dir_permissions_sync(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(path, fs::Permissions::from_mode(PRIVATE_DIR_MODE))
            .with_context(|| format!("chmod 0700 {}", path.display()))?;
    }
    #[cfg(windows)]
    apply_windows_private_acl(path)?;
    Ok(())
}

pub(super) fn apply_private_file_permissions_sync(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(path, fs::Permissions::from_mode(PRIVATE_FILE_MODE))
            .with_context(|| format!("chmod 0600 {}", path.display()))?;
    }
    #[cfg(windows)]
    apply_windows_private_acl(path)?;
    Ok(())
}

pub(super) fn harden_private_open_file_sync(file: &File, path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        file.set_permissions(fs::Permissions::from_mode(PRIVATE_FILE_MODE))
            .with_context(|| format!("chmod 0600 open file {}", path.display()))?;
    }
    #[cfg(windows)]
    apply_windows_private_acl(path)?;
    Ok(())
}

#[cfg(unix)]
pub(super) fn is_private_dir_boundary(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;

    metadata.is_dir()
        && !metadata_is_link_or_reparse_point(metadata)
        && metadata.permissions().mode() & 0o777 == PRIVATE_DIR_MODE
}

#[cfg(not(unix))]
pub(super) fn is_private_dir_boundary(_metadata: &fs::Metadata) -> bool {
    false
}

#[cfg(windows)]
pub(super) fn metadata_is_link_or_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
pub(super) fn metadata_is_link_or_reparse_point(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(windows)]
fn encode_windows_path(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(windows)]
fn encode_windows_sddl(sddl: &str) -> Vec<u16> {
    std::ffi::OsStr::new(sddl)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(windows)]
fn apply_windows_private_acl(path: &Path) -> Result<()> {
    let path_wide = encode_windows_path(path);
    let sddl_wide = encode_windows_sddl(WINDOWS_PRIVATE_SDDL);
    let mut security_descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl_wide.as_ptr(),
            SDDL_REVISION_1 as u32,
            &mut security_descriptor,
            std::ptr::null_mut(),
        )
    };
    if converted == 0 {
        anyhow::bail!("failed to build Windows private ACL for {}", path.display());
    }
    let result = unsafe {
        SetFileSecurityW(
            path_wide.as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            security_descriptor,
        )
    };
    unsafe {
        let _ = LocalFree(security_descriptor as isize);
    }
    if result == 0 {
        anyhow::bail!("failed to apply Windows private ACL to {}", path.display());
    }
    Ok(())
}
