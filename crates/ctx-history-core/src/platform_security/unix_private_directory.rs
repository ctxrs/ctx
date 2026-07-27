//! Descriptor-relative private-directory creation for Unix.

#![allow(unsafe_code)]

use std::{
    ffi::{CString, OsStr},
    fs::{File, Metadata},
    io,
    os::{
        fd::{AsRawFd as _, FromRawFd as _},
        unix::{
            ffi::OsStrExt as _,
            fs::{FileTypeExt as _, PermissionsExt as _},
        },
    },
    path::{Component, Path},
};

const PRIVATE_DIRECTORY_MODE: libc::mode_t = 0o700;

pub(super) fn create_private_directory_all(path: &Path) -> io::Result<()> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "private directory path must be non-empty and traversal-free",
        ));
    }

    let mut components = path.components().peekable();
    let mut current = match components.peek() {
        Some(Component::RootDir) => {
            components.next();
            open_directory(libc::AT_FDCWD, OsStr::new("/"))?
        }
        Some(Component::Prefix(_)) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Unix private directory paths cannot contain a platform prefix",
            ));
        }
        _ => open_directory(libc::AT_FDCWD, OsStr::new("."))?,
    };
    let mut created_private_ancestor = false;
    let mut saw_component = false;

    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            continue;
        };
        saw_component = true;
        let is_final = components.peek().is_none();
        let (next, created, raced_existing) = open_or_create_directory(&current, name)?;
        if created {
            verify_exact_private_directory(&next.metadata()?)?;
            created_private_ancestor = true;
        } else if is_final || created_private_ancestor || raced_existing {
            verify_owner_only_directory(&next.metadata()?)?;
        }
        current = next;
    }

    if !saw_component {
        verify_owner_only_directory(&current.metadata()?)?;
    }
    Ok(())
}

fn open_or_create_directory(parent: &File, name: &OsStr) -> io::Result<(File, bool, bool)> {
    match open_directory(parent.as_raw_fd(), name) {
        Ok(directory) => Ok((directory, false, false)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let name = path_component(name)?;
            // mkdirat applies umask only by removing bits from 0700, so a new
            // directory is never exposed to group or other while it is made
            // usable. fchmodat is descriptor-relative and refuses symlinks.
            let created =
                unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), PRIVATE_DIRECTORY_MODE) };
            if created == 0 {
                set_exact_private_mode(parent, &name)?;
                let directory = open_directory_cstr(parent.as_raw_fd(), &name)?;
                return Ok((directory, true, false));
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::AlreadyExists {
                return Err(error);
            }

            // Another creator won the race. Treat that object as pre-existing:
            // open without following and verify it, but never chmod it.
            let directory = open_directory_cstr(parent.as_raw_fd(), &name)?;
            Ok((directory, false, true))
        }
        Err(error) => Err(error),
    }
}

fn set_exact_private_mode(parent: &File, name: &CString) -> io::Result<()> {
    let result = unsafe {
        libc::fchmodat(
            parent.as_raw_fd(),
            name.as_ptr(),
            PRIVATE_DIRECTORY_MODE,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn open_directory(parent: libc::c_int, name: &OsStr) -> io::Result<File> {
    let name = path_component(name)?;
    open_directory_cstr(parent, &name)
}

fn open_directory_cstr(parent: libc::c_int, name: &CString) -> io::Result<File> {
    let descriptor = unsafe {
        libc::openat(
            parent,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_DIRECTORY,
        )
    };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    let file = unsafe { File::from_raw_fd(descriptor) };
    verify_directory_type(&file.metadata()?)?;
    Ok(file)
}

fn path_component(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "private directory path component contains a NUL byte",
        )
    })
}

fn verify_directory_type(metadata: &Metadata) -> io::Result<()> {
    if metadata.file_type().is_dir()
        && !metadata.file_type().is_symlink()
        && !metadata.file_type().is_block_device()
        && !metadata.file_type().is_char_device()
        && !metadata.file_type().is_fifo()
        && !metadata.file_type().is_socket()
    {
        Ok(())
    } else {
        Err(private_directory_error())
    }
}

fn verify_owner_only_directory(metadata: &Metadata) -> io::Result<()> {
    verify_directory_type(metadata)?;
    if metadata.permissions().mode() & 0o077 == 0 {
        Ok(())
    } else {
        Err(private_directory_error())
    }
}

fn verify_exact_private_directory(metadata: &Metadata) -> io::Result<()> {
    verify_directory_type(metadata)?;
    if metadata.permissions().mode() & 0o777 == 0o700 {
        Ok(())
    } else {
        Err(private_directory_error())
    }
}

fn private_directory_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "private state path is not owner-only",
    )
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt as _};

    use super::*;

    #[test]
    fn nested_creation_is_exact_and_usable_under_umask_0777() {
        const CHILD_ENV: &str = "CTX_TEST_PRIVATE_DIRECTORY_UMASK_CHILD";
        if let Some(target) = std::env::var_os(CHILD_ENV) {
            // SAFETY: this is a single-test child process, so changing its
            // process-wide umask cannot race other tests.
            unsafe {
                libc::umask(0o777);
            }
            let first = Path::new(&target).join("private");
            let nested = first.join("state");
            create_private_directory_all(&nested).unwrap();
            assert_eq!(
                fs::metadata(&first).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(&nested).unwrap().permissions().mode() & 0o777,
                0o700
            );
            fs::write(nested.join("usable"), b"ok").unwrap();
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("nested_creation_is_exact_and_usable_under_umask_0777")
            .arg("--nocapture")
            .env(CHILD_ENV, temp.path())
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn insecure_existing_target_is_rejected_without_repair() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("insecure");
        fs::create_dir(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();

        assert!(create_private_directory_all(&target).is_err());
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[test]
    fn symlink_ancestor_is_rejected() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        let link = temp.path().join("link");
        fs::create_dir(&target).unwrap();
        symlink(&target, &link).unwrap();

        assert!(create_private_directory_all(&link.join("nested")).is_err());
        assert!(!target.join("nested").exists());
    }
}
