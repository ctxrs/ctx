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
            fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _},
        },
    },
    path::{Component, Path},
};

#[cfg(target_os = "macos")]
use std::{ffi::c_void, ptr::null_mut};

const PRIVATE_DIRECTORY_MODE: libc::mode_t = 0o700;

#[cfg(target_os = "macos")]
type Acl = *mut c_void;
#[cfg(target_os = "macos")]
const ACL_TYPE_EXTENDED: libc::c_int = 0x0000_0100;

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn acl_init(count: libc::c_int) -> Acl;
    fn acl_get_fd_np(fd: libc::c_int, acl_type: libc::c_int) -> Acl;
    fn acl_get_entry(acl: Acl, entry_id: libc::c_int, entry: *mut *mut c_void) -> libc::c_int;
    fn acl_set_fd_np(fd: libc::c_int, acl: Acl, acl_type: libc::c_int) -> libc::c_int;
    fn acl_free(object: *mut c_void) -> libc::c_int;
}

pub(super) fn create_private_directory_all(path: &Path) -> io::Result<()> {
    walk_private_directory(path, ExistingFinalPolicy::Verify)
}

pub(super) fn establish_private_data_root(path: &Path) -> io::Result<()> {
    walk_private_directory(path, ExistingFinalPolicy::EstablishExact)
}

pub(super) fn ensure_private_directory(path: &Path) -> io::Result<()> {
    walk_private_directory(path, ExistingFinalPolicy::EnsurePrivate)
}

#[derive(Clone, Copy)]
enum ExistingFinalPolicy {
    Verify,
    EstablishExact,
    EnsurePrivate,
}

fn walk_private_directory(path: &Path, existing_final: ExistingFinalPolicy) -> io::Result<()> {
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

    let normalized_path = super::path_overlap::normalize_platform_namespace_alias(path);
    walk_private_directory_nofollow(&normalized_path, existing_final)
}

fn walk_private_directory_nofollow(
    path: &Path,
    existing_final: ExistingFinalPolicy,
) -> io::Result<()> {
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
            clear_extended_acl(&next)?;
            verify_exact_private_directory(&next.metadata()?)?;
            created_private_ancestor = true;
        } else if is_final {
            match existing_final {
                ExistingFinalPolicy::Verify => verify_owner_only_directory(&next.metadata()?)?,
                ExistingFinalPolicy::EstablishExact => establish_exact_private_directory(&next)?,
                ExistingFinalPolicy::EnsurePrivate => ensure_owner_private_directory(&next)?,
            }
        } else if created_private_ancestor || raced_existing {
            verify_owner_only_directory(&next.metadata()?)?;
        }
        current = next;
    }

    if !saw_component {
        if matches!(existing_final, ExistingFinalPolicy::EstablishExact) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "ctx data root must name a directory below the filesystem root",
            ));
        }
        verify_owner_only_directory(&current.metadata()?)?;
    }
    Ok(())
}

fn ensure_owner_private_directory(directory: &File) -> io::Result<()> {
    let metadata = directory.metadata()?;
    verify_directory_type(&metadata)?;
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(private_directory_error());
    }
    if metadata.mode() & 0o077 != 0 {
        directory.set_permissions(std::fs::Permissions::from_mode(0o700))?;
    }
    clear_extended_acl(directory)?;
    verify_owner_only_directory(&directory.metadata()?)
}

fn establish_exact_private_directory(directory: &File) -> io::Result<()> {
    let metadata = directory.metadata()?;
    verify_directory_type(&metadata)?;
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "ctx data root is not owned by the current user",
        ));
    }
    directory.set_permissions(std::fs::Permissions::from_mode(0o700))?;
    clear_extended_acl(directory)?;
    verify_exact_private_directory(&directory.metadata()?)
}

#[cfg(target_os = "macos")]
pub(super) fn clear_extended_acl(directory: &File) -> io::Result<()> {
    let acl = unsafe { acl_init(0) };
    if acl.is_null() {
        return Err(io::Error::last_os_error());
    }
    let result = unsafe { acl_set_fd_np(directory.as_raw_fd(), acl, ACL_TYPE_EXTENDED) };
    let set_error = (result != 0).then(io::Error::last_os_error);
    let free_result = unsafe { acl_free(acl) };
    if let Some(error) = set_error {
        return Err(error);
    }
    if free_result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub(super) fn clear_extended_acl(_directory: &File) -> io::Result<()> {
    Ok(())
}

#[cfg(target_os = "macos")]
pub(super) fn verify_no_extended_acl(file: &File) -> io::Result<()> {
    const ACL_FIRST_ENTRY: libc::c_int = 0;

    let acl = unsafe { acl_get_fd_np(file.as_raw_fd(), ACL_TYPE_EXTENDED) };
    if acl.is_null() {
        let error = io::Error::last_os_error();
        // Darwin reports ENOENT when a regular file has no extended ACL.
        // Absence is the exact state this verifier requires.
        return if error.raw_os_error() == Some(libc::ENOENT) {
            Ok(())
        } else {
            Err(error)
        };
    }
    let mut entry = null_mut();
    let result = unsafe { acl_get_entry(acl, ACL_FIRST_ENTRY, &raw mut entry) };
    let entry_error = (result != 0).then(io::Error::last_os_error);
    let free_result = unsafe { acl_free(acl) };
    if free_result != 0 {
        return Err(io::Error::last_os_error());
    }
    // Darwin returns zero when ACL_FIRST_ENTRY finds an entry and EINVAL when
    // the valid retrieved ACL has no first entry.
    match (result, entry_error) {
        (0, _) => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private state path has an extended ACL",
        )),
        (-1, Some(error)) if error.raw_os_error() == Some(libc::EINVAL) => Ok(()),
        (-1, Some(error)) => Err(error),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected result while inspecting an extended ACL",
        )),
    }
}

#[cfg(not(target_os = "macos"))]
pub(super) fn verify_no_extended_acl(_file: &File) -> io::Result<()> {
    Ok(())
}

fn open_or_create_directory(parent: &File, name: &OsStr) -> io::Result<(File, bool, bool)> {
    open_or_create_directory_after_missing(parent, name, || Ok(()))
}

fn open_or_create_directory_after_missing(
    parent: &File,
    name: &OsStr,
    after_missing: impl FnOnce() -> io::Result<()>,
) -> io::Result<(File, bool, bool)> {
    match open_directory(parent.as_raw_fd(), name) {
        Ok(directory) => Ok((directory, false, false)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            after_missing()?;
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
    if metadata.uid() == unsafe { libc::geteuid() } && metadata.permissions().mode() & 0o077 == 0 {
        Ok(())
    } else {
        Err(private_directory_error())
    }
}

fn verify_exact_private_directory(metadata: &Metadata) -> io::Result<()> {
    verify_directory_type(metadata)?;
    if metadata.uid() == unsafe { libc::geteuid() }
        && metadata.permissions().mode() & 0o777 == 0o700
    {
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
    use std::{
        fs,
        os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    };

    use super::*;

    #[test]
    fn private_data_root_creation_is_exact_and_usable_under_umask_0777() {
        const CHILD_ENV: &str = "CTX_TEST_PRIVATE_DIRECTORY_UMASK_CHILD";
        if let Some(target) = std::env::var_os(CHILD_ENV) {
            // SAFETY: this is a single-test child process, so changing its
            // process-wide umask cannot race other tests.
            unsafe {
                libc::umask(0o777);
            }
            let first = Path::new(&target).join("private");
            let nested = first.join("state");
            ensure_private_directory(&nested).unwrap();
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
            .arg(
                "platform_security::unix_private_directory::tests::private_data_root_creation_is_exact_and_usable_under_umask_0777",
            )
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
    fn user_owned_0700_symlink_prefix_is_rejected() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let target = temp.path().join("target");
        let link = temp.path().join("link");
        let temp_metadata = fs::metadata(temp.path()).unwrap();
        assert_eq!(temp_metadata.uid(), unsafe { libc::geteuid() });
        assert_eq!(temp_metadata.permissions().mode() & 0o777, 0o700);
        fs::create_dir(&target).unwrap();
        symlink(&target, &link).unwrap();

        assert!(establish_private_data_root(&link.join("nested")).is_err());
        assert!(!target.join("nested").exists());
    }

    #[test]
    fn create_race_refuses_a_symlink_winner_without_repair() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let target = temp.path().join("target");
        let raced = temp.path().join("raced");
        fs::create_dir(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        let parent = File::open(temp.path()).unwrap();

        let result = open_or_create_directory_after_missing(&parent, OsStr::new("raced"), || {
            symlink(&target, &raced)
        });

        assert!(result.is_err());
        assert!(fs::symlink_metadata(&raced)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[test]
    fn establishing_data_root_repairs_existing_mode_before_use() {
        let temp = tempfile::tempdir().unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o755)).unwrap();
        let target = temp.path().join("data");
        fs::create_dir(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();

        establish_private_data_root(&target).unwrap();

        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(temp.path()).unwrap().permissions().mode() & 0o777,
            0o755,
            "establishing a legacy final root must not chmod existing ancestors"
        );
        fs::write(target.join("first-write"), b"private").unwrap();
    }

    #[test]
    fn establishing_data_root_rejects_symlink_without_touching_target() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        let link = temp.path().join("data");
        fs::create_dir(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        symlink(&target, &link).unwrap();

        assert!(establish_private_data_root(&link).is_err());
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn establishing_data_root_removes_extended_acl() {
        use std::process::Command;

        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("data");
        fs::create_dir(&target).unwrap();
        let user = std::env::var("USER").unwrap();
        let status = Command::new("/bin/chmod")
            .args([
                "+a",
                &format!("{user} allow read"),
                target.to_str().unwrap(),
            ])
            .status()
            .unwrap();
        assert!(status.success());

        establish_private_data_root(&target).unwrap();

        let output = Command::new("/bin/ls")
            .args(["-lde", target.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(!String::from_utf8_lossy(&output.stdout).contains(" 0: "));
    }
}
