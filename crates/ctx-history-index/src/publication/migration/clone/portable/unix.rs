use std::{
    ffi::{CStr, CString, OsString},
    fs::{File, Metadata, OpenOptions},
    io,
    os::{
        fd::{AsRawFd, FromRawFd, RawFd},
        unix::{
            ffi::{OsStrExt, OsStringExt},
            fs::{MetadataExt, OpenOptionsExt},
        },
    },
    path::Path,
};

use super::{entry_kind, require_regular, ObjectIdentity};
use crate::{IndexError, Result};

pub(super) fn open_directory_path(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)
}

pub(super) fn open_directory_at(
    parent: &File,
    _parent_path: &Path,
    name: &Path,
) -> io::Result<File> {
    open_at(
        parent.as_raw_fd(),
        name,
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW,
        0,
    )
}

pub(super) fn create_directory_at(
    parent: &File,
    parent_path: &Path,
    name: &Path,
) -> io::Result<File> {
    let name_c = path_cstring(name)?;
    // SAFETY: the parent descriptor and NUL-terminated name remain valid.
    if unsafe { libc::mkdirat(parent.as_raw_fd(), name_c.as_ptr(), 0o700) } != 0 {
        return Err(io::Error::last_os_error());
    }
    open_directory_at(parent, parent_path, name)
}

pub(super) fn open_regular_file_at(
    parent: &File,
    _parent_path: &Path,
    name: &Path,
) -> io::Result<File> {
    open_at(
        parent.as_raw_fd(),
        name,
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK,
        0,
    )
}

pub(super) fn create_regular_file_at(
    parent: &File,
    _parent_path: &Path,
    name: &Path,
) -> io::Result<File> {
    open_at(
        parent.as_raw_fd(),
        name,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW,
        0o600,
    )
}

fn open_at(parent: RawFd, name: &Path, flags: libc::c_int, mode: libc::mode_t) -> io::Result<File> {
    let name = path_cstring(name)?;
    // SAFETY: the parent descriptor and NUL-terminated name remain valid;
    // a successful descriptor is transferred into `File` exactly once.
    let fd = unsafe {
        libc::openat(
            parent,
            name.as_ptr(),
            flags | libc::O_CLOEXEC,
            mode as libc::c_uint,
        )
    };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: `openat` returned a new owned descriptor.
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

fn path_cstring(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains an interior NUL"))
}

pub(super) fn directory_entries(
    file: &File,
    _path: &Path,
    maximum: usize,
) -> Result<Vec<OsString>> {
    // SAFETY: `dup` creates an independently owned descriptor.
    let duplicate = unsafe { libc::dup(file.as_raw_fd()) };
    if duplicate < 0 {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: `fdopendir` consumes `duplicate` on success.
    let stream = unsafe { libc::fdopendir(duplicate) };
    if stream.is_null() {
        // SAFETY: `fdopendir` did not consume the descriptor on failure.
        unsafe { libc::close(duplicate) };
        return Err(io::Error::last_os_error().into());
    }
    struct Stream(*mut libc::DIR);
    impl Drop for Stream {
        fn drop(&mut self) {
            // SAFETY: the stream is uniquely owned and closed once.
            unsafe { libc::closedir(self.0) };
        }
    }
    let stream = Stream(stream);
    let mut entries = Vec::new();
    loop {
        set_errno(0);
        // SAFETY: the stream remains live through this call.
        let entry = unsafe { libc::readdir(stream.0) };
        if entry.is_null() {
            let error = io::Error::last_os_error();
            if error.raw_os_error().unwrap_or(0) != 0 {
                return Err(error.into());
            }
            break;
        }
        // SAFETY: POSIX guarantees NUL termination of `d_name`.
        let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if bytes != b"." && bytes != b".." {
            let actual = entries
                .len()
                .checked_add(1)
                .ok_or(IndexError::CountOverflow)?;
            if actual > maximum {
                return Err(IndexError::PredecessorMigrationFileLimit { actual, maximum });
            }
            entries.push(OsString::from_vec(bytes.to_vec()));
        }
    }
    entries.sort();
    Ok(entries)
}

pub(super) fn object_identity(file: &File) -> io::Result<ObjectIdentity> {
    let metadata = file.metadata()?;
    Ok(ObjectIdentity {
        first: metadata.dev(),
        second: metadata.ino(),
    })
}

pub(super) fn permission_identity(metadata: &Metadata) -> u32 {
    metadata.mode() & 0o7777
}

pub(super) fn is_unsafe_link_or_provider(_metadata: &Metadata) -> bool {
    false
}

pub(super) fn is_nofollow_rejection(error: &io::Error) -> bool {
    error
        .raw_os_error()
        .is_some_and(|code| [libc::ELOOP, libc::ENOTDIR].contains(&code))
}

pub(super) fn available_bytes(file: &File, _path: &Path) -> io::Result<u64> {
    // SAFETY: zeroed `statvfs` is initialized by successful `fstatvfs`.
    let mut stat = unsafe { std::mem::zeroed::<libc::statvfs>() };
    if unsafe { libc::fstatvfs(file.as_raw_fd(), &mut stat) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64))
}

pub(super) fn restrict_destination_directory(file: &File) -> io::Result<()> {
    // SAFETY: `file` owns a live directory descriptor.
    if unsafe { libc::fchmod(file.as_raw_fd(), 0o700) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub(super) fn sync_directory(file: &File) -> io::Result<()> {
    file.sync_all()
}

pub(super) fn discard_destination(
    parent: &File,
    _parent_path: &Path,
    name: &Path,
    destination: &File,
    destination_path: &Path,
) -> io::Result<()> {
    for entry in directory_entries(
        destination,
        destination_path,
        super::MAX_MIGRATION_DIRECTORY_ENTRIES,
    )
    .map_err(|error| io::Error::other(error.to_string()))?
    {
        let relative = Path::new(&entry);
        let file = open_regular_file_at(destination, destination_path, relative)?;
        let kind =
            entry_kind(&file.metadata()?).map_err(|error| io::Error::other(error.to_string()))?;
        require_regular(kind).map_err(|error| io::Error::other(error.to_string()))?;
        unlink_open_file(destination, relative, &file, 0)?;
    }
    unlink_open_file(parent, name, destination, libc::AT_REMOVEDIR)
}

#[cfg(target_os = "freebsd")]
fn unlink_open_file(parent: &File, name: &Path, file: &File, flags: libc::c_int) -> io::Result<()> {
    let name = path_cstring(name)?;
    unsafe extern "C" {
        fn funlinkat(
            dfd: libc::c_int,
            path: *const libc::c_char,
            fd: libc::c_int,
            flag: libc::c_int,
        ) -> libc::c_int;
    }
    // SAFETY: all descriptors and the NUL-terminated name remain valid;
    // FreeBSD only unlinks when `name` still identifies `file`.
    if unsafe { funlinkat(parent.as_raw_fd(), name.as_ptr(), file.as_raw_fd(), flags) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "freebsd"))]
fn unlink_open_file(parent: &File, name: &Path, file: &File, flags: libc::c_int) -> io::Result<()> {
    let expected = object_identity(file)?;
    let named = if flags & libc::AT_REMOVEDIR != 0 {
        open_directory_at(parent, Path::new(""), name)?
    } else {
        open_regular_file_at(parent, Path::new(""), name)?
    };
    if object_identity(&named)? != expected {
        return Err(io::Error::other(
            "cleanup target changed after authentication",
        ));
    }
    let name = path_cstring(name)?;
    // SAFETY: the parent descriptor and NUL-terminated name remain valid.
    if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), flags) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
fn set_errno(value: libc::c_int) {
    // SAFETY: the pointer addresses this thread's errno.
    unsafe { *libc::__errno_location() = value };
}

#[cfg(any(target_os = "macos", target_os = "freebsd"))]
fn set_errno(value: libc::c_int) {
    // SAFETY: the pointer addresses this thread's errno.
    unsafe { *libc::__error() = value };
}
