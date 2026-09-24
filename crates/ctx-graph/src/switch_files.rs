//! Permissions and publication of migration files containing configuration secrets.
use std::{fs::File, path::Path};

use anyhow::Result;
use tempfile::NamedTempFile;

/// Restrict a newly created, empty file (or directory) before writing secrets.
/// Callers must own the path and must not use this to repair an exposed backup.
pub fn protect(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let file = File::options()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        let metadata = file.metadata()?;
        anyhow::ensure!(
            metadata.is_file() || metadata.is_dir(),
            "expected a file or directory"
        );
        let mode = if metadata.is_dir() { 0o700 } else { 0o600 };
        file.set_permissions(std::fs::Permissions::from_mode(mode))?;
        unix::clear_acl(&file, metadata.is_dir())?;
    }
    #[cfg(windows)]
    windows::protect(path)?;
    #[cfg(not(any(unix, windows)))]
    anyhow::bail!("private migration files are unsupported on this platform");
    Ok(())
}

/// Copy access permissions onto an empty, protected replacement before writing.
/// Windows preserves the destination DACL in `replace` instead.
pub fn preserve_permissions(temp: &File, destination: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use anyhow::{Context, ensure};
        use std::os::{
            fd::AsRawFd,
            unix::fs::{MetadataExt, OpenOptionsExt},
        };
        let source = File::options()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(destination)?;
        let original = source.metadata()?;
        let current = temp.metadata()?;
        ensure!(
            original.is_file() && current.is_file() && current.len() == 0,
            "permission preservation requires a regular source and empty temporary file"
        );
        if (current.uid(), current.gid()) != (original.uid(), original.gid()) {
            // Do not silently change the identity to which owner/group access applies.
            unix::check(unsafe { libc::fchown(temp.as_raw_fd(), original.uid(), original.gid()) })
                .context("cannot preserve configuration owner/group")?;
        }
        // The replacement is still empty. Set mode before the ACL:
        // a macOS deny-write-security entry may forbid later chmod operations.
        temp.set_permissions(original.permissions())?;
        unix::copy_acl(&source, temp).context("cannot preserve configuration access ACL")?;
        let copied = temp.metadata()?;
        ensure!(
            (copied.uid(), copied.gid(), copied.mode() & 0o7777)
                == (original.uid(), original.gid(), original.mode() & 0o7777),
            "configuration permissions were not preserved"
        );
    }
    #[cfg(not(unix))]
    let _ = (temp, destination);
    Ok(())
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::{io, os::fd::AsRawFd};

    pub(super) fn check(result: libc::c_int) -> io::Result<()> {
        if result == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    #[cfg(target_os = "linux")]
    fn no_acl(error: &io::Error) -> bool {
        matches!(error.raw_os_error(), Some(libc::ENODATA | libc::EOPNOTSUPP))
    }

    #[cfg(target_os = "linux")]
    fn remove(file: &File, name: &std::ffi::CStr) -> io::Result<()> {
        match check(unsafe { libc::fremovexattr(file.as_raw_fd(), name.as_ptr()) }) {
            Err(error) if no_acl(&error) => Ok(()),
            result => result,
        }
    }

    pub(super) fn clear_acl(file: &File, directory: bool) -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            remove(file, c"system.posix_acl_access")?;
            if directory {
                remove(file, c"system.posix_acl_default")?;
            }
        }
        #[cfg(target_os = "macos")]
        {
            let _ = directory;
            macos::Acl::empty()?.set(file)?;
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        anyhow::bail!("private file ACLs are unsupported on this Unix platform");
        Ok(())
    }

    pub(super) fn copy_acl(source: &File, target: &File) -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            // Linux limits xattr values to 64 KiB. One read avoids a size/read race.
            let mut acl = vec![0u8; 65536];
            let name = c"system.posix_acl_access";
            let length = unsafe {
                libc::fgetxattr(
                    source.as_raw_fd(),
                    name.as_ptr(),
                    acl.as_mut_ptr().cast(),
                    acl.len(),
                )
            };
            if length == -1 {
                let error = io::Error::last_os_error();
                if !no_acl(&error) {
                    return Err(error.into());
                }
                remove(target, name)?;
            } else {
                check(unsafe {
                    libc::fsetxattr(
                        target.as_raw_fd(),
                        name.as_ptr(),
                        acl.as_ptr().cast(),
                        length as usize,
                        0,
                    )
                })?;
            }
        }
        #[cfg(target_os = "macos")]
        macos::Acl::read(source)?.set(target)?;
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        anyhow::bail!("preserving access ACLs is unsupported on this Unix platform");
        Ok(())
    }

    #[cfg(target_os = "macos")]
    mod macos {
        use super::*;
        use std::ffi::c_void;

        // Darwin's libc exports these but the Rust libc crate has no bindings.
        // https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man3/acl_set_fd.3.html
        unsafe extern "C" {
            fn acl_get_fd(fd: libc::c_int) -> *mut c_void;
            fn acl_init(count: libc::c_int) -> *mut c_void;
            fn acl_set_fd(fd: libc::c_int, acl: *mut c_void) -> libc::c_int;
            fn acl_free(acl: *mut c_void) -> libc::c_int;
        }

        pub(super) struct Acl(*mut c_void);
        impl Acl {
            pub(super) fn empty() -> io::Result<Self> {
                let acl = unsafe { acl_init(0) };
                if acl.is_null() {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(Self(acl))
                }
            }

            pub(super) fn read(file: &File) -> io::Result<Self> {
                let acl = unsafe { acl_get_fd(file.as_raw_fd()) };
                if !acl.is_null() {
                    return Ok(Self(acl));
                }
                let error = io::Error::last_os_error();
                // Darwin reports an absent ACL as ENOENT even for an open fd.
                if error.raw_os_error() == Some(libc::ENOENT) {
                    Self::empty()
                } else {
                    Err(error)
                }
            }

            pub(super) fn set(&self, file: &File) -> io::Result<()> {
                check(unsafe { acl_set_fd(file.as_raw_fd(), self.0) })
            }
        }
        impl Drop for Acl {
            fn drop(&mut self) {
                unsafe {
                    acl_free(self.0);
                }
            }
        }
    }
}

/// Publish a protected temporary file. Existing Windows configs retain their DACL.
pub fn replace(temp: NamedTempFile, path: &Path, exists: bool) -> Result<()> {
    if !exists {
        temp.persist_noclobber(path)?;
    } else {
        #[cfg(windows)]
        windows::replace(temp, path)?;
        #[cfg(not(windows))]
        temp.persist(path)?;
    }
    Ok(())
}

#[cfg(windows)]
mod windows {
    use super::*;
    use anyhow::{Context, ensure};
    use std::{
        io,
        os::windows::{ffi::OsStrExt, io::FromRawHandle, io::OwnedHandle},
        ptr::{null, null_mut},
    };
    use windows_sys::Win32::{
        Foundation::HANDLE,
        Security::{
            ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, AddAccessAllowedAceEx,
            Authorization::{SE_FILE_OBJECT, SetNamedSecurityInfoW},
            CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, GetTokenInformation, InitializeAcl,
            OBJECT_INHERIT_ACE, PROTECTED_DACL_SECURITY_INFORMATION, SECURITY_MAX_SID_SIZE,
            TOKEN_QUERY, TOKEN_USER, TokenUser,
        },
        Storage::FileSystem::{FILE_ALL_ACCESS, ReplaceFileW},
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };

    fn wide(path: &Path) -> Result<Vec<u16>> {
        let mut wide: Vec<_> = path.as_os_str().encode_wide().collect();
        ensure!(!wide.contains(&0), "file path contains a NUL");
        wide.push(0);
        Ok(wide)
    }

    pub(super) fn protect(path: &Path) -> Result<()> {
        let inheritance = if path.metadata()?.is_dir() {
            OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE
        } else {
            0
        };
        let path = wide(path)?;
        // All buffers are aligned for their native structures and remain alive
        // until SetNamedSecurityInfoW copies the ACL. OwnedHandle closes the token.
        unsafe {
            let mut token: HANDLE = null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return Err(io::Error::last_os_error()).context("cannot open current user token");
            }
            let _token = OwnedHandle::from_raw_handle(token);
            let mut user = [0usize;
                (size_of::<TOKEN_USER>() + SECURITY_MAX_SID_SIZE as usize)
                    .div_ceil(size_of::<usize>())];
            let mut returned = 0;
            if GetTokenInformation(
                token,
                TokenUser,
                user.as_mut_ptr().cast(),
                size_of_val(&user) as u32,
                &mut returned,
            ) == 0
            {
                return Err(io::Error::last_os_error()).context("cannot read current user SID");
            }
            let sid = (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid;
            let mut acl = [0u32;
                (size_of::<ACL>()
                    + size_of::<ACCESS_ALLOWED_ACE>()
                    + SECURITY_MAX_SID_SIZE as usize)
                    .div_ceil(size_of::<u32>())];
            let acl_size = size_of_val(&acl) as u32;
            let acl = acl.as_mut_ptr().cast::<ACL>();
            if InitializeAcl(acl, acl_size, ACL_REVISION) == 0
                || AddAccessAllowedAceEx(acl, ACL_REVISION, inheritance, FILE_ALL_ACCESS, sid) == 0
            {
                return Err(io::Error::last_os_error()).context("cannot build private file ACL");
            }
            // Replace the entire DACL and disable inheritance: shared project
            // readers must never gain access to the configuration backup.
            let error = SetNamedSecurityInfoW(
                path.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                acl,
                null(),
            );
            if error != 0 {
                return Err(io::Error::from_raw_os_error(error as i32))
                    .context("cannot protect migration file");
            }
        }
        Ok(())
    }

    pub(super) fn replace(temp: NamedTempFile, path: &Path) -> Result<()> {
        let destination = wide(path)?;
        // Close the file handle before replacement; keep TempPath for cleanup.
        let temp = temp.into_temp_path();
        let source = wide(&temp)?;
        // ReplaceFileW preserves the original DACL. Flags must remain zero:
        // IGNORE_ACL_ERRORS / IGNORE_MERGE_ERRORS can silently lose protection.
        // https://learn.microsoft.com/windows/win32/api/winbase/nf-winbase-replacefilew
        if unsafe {
            ReplaceFileW(
                destination.as_ptr(),
                source.as_ptr(),
                null(),
                0,
                null(),
                null(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error()).context("cannot replace configuration file");
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "switch_files_tests.rs"]
mod tests;
