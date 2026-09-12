//! No-follow file metadata operations that preserve live POSIX record locks.
//!
//! Opening and closing a separate descriptor would release every POSIX lock
//! this process holds on the inode, including SQLite's WAL coordination locks.

use std::{
    ffi::{CStr, CString},
    fs::{self, Metadata},
    io,
    os::unix::{ffi::OsStrExt as _, fs::MetadataExt as _},
    path::Path,
};

use super::private_policy_error;

pub(super) fn restrict(path: &Path) -> io::Result<()> {
    let before = owned_regular_file(path)?;
    let name = native_path(path)?;
    let result = unsafe {
        libc::fchmodat(
            libc::AT_FDCWD,
            name.as_ptr(),
            0o600,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    clear_extended_acl(&name)?;
    verify(path)?;
    verify_same_file(path, &before)
}

pub(super) fn verify(path: &Path) -> io::Result<()> {
    let before = owned_regular_file(path)?;
    if before.mode() & 0o177 != 0 {
        return Err(private_policy_error());
    }
    verify_no_extended_acl(&native_path(path)?)?;
    verify_same_file(path, &before)
}

fn owned_regular_file(path: &Path) -> io::Result<Metadata> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(private_policy_error());
    }
    Ok(metadata)
}

fn verify_same_file(path: &Path, before: &Metadata) -> io::Result<()> {
    let after = owned_regular_file(path)?;
    if (before.dev(), before.ino()) != (after.dev(), after.ino()) || after.mode() & 0o177 != 0 {
        return Err(private_policy_error());
    }
    Ok(())
}

fn native_path(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "private file path contains a NUL byte",
        )
    })
}

#[cfg(target_os = "macos")]
fn clear_extended_acl(path: &CStr) -> io::Result<()> {
    let mut attributes: libc::attrlist = unsafe { std::mem::zeroed() };
    attributes.bitmapcount = libc::ATTR_BIT_MAP_COUNT as _;
    attributes.commonattr = libc::ATTR_CMN_EXTENDED_SECURITY;
    // Darwin's native attrreference_t followed by an empty kauth_filesec:
    // reference offset/length, magic, unchanged owner/group GUIDs, NOACL, flags.
    // Unlike acl_set_link_np's lstat-then-chmod implementation, setattrlist
    // applies FSOPT_NOFOLLOW inside the kernel for the actual ACL mutation.
    let mut payload: [u32; 13] = [8, 44, 0x012c_c16d, 0, 0, 0, 0, 0, 0, 0, 0, u32::MAX, 0];
    let result = unsafe {
        libc::setattrlist(
            path.as_ptr(),
            std::ptr::from_mut(&mut attributes).cast(),
            payload.as_mut_ptr().cast(),
            std::mem::size_of_val(&payload),
            libc::FSOPT_NOFOLLOW as _,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "macos")]
fn verify_no_extended_acl(path: &CStr) -> io::Result<()> {
    unsafe extern "C" {
        fn acl_get_link_np(path: *const libc::c_char, acl_type: libc::c_int) -> *mut libc::c_void;
    }
    const ACL_TYPE_EXTENDED: libc::c_int = 0x0000_0100;
    let acl = unsafe { acl_get_link_np(path.as_ptr(), ACL_TYPE_EXTENDED) };
    super::unix_private_directory::verify_empty_acl(acl)
}

#[cfg(not(target_os = "macos"))]
fn clear_extended_acl(_path: &CStr) -> io::Result<()> {
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn verify_no_extended_acl(_path: &CStr) -> io::Result<()> {
    Ok(())
}
