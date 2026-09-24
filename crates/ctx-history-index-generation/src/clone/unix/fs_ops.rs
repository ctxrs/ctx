use super::*;

pub(super) fn write_authenticated_plan_bytes(
    destination: &File,
    path: &Path,
    bytes: &[u8],
) -> Result<u64> {
    let mut destination_file = create_regular_file_at(destination, path)?;
    destination_file.write_all(bytes)?;
    destination_file.flush()?;
    let copied = u64::try_from(bytes.len()).map_err(|_| IndexError::CountOverflow)?;
    if FileIdentity::from_metadata(&destination_file.metadata()?).bytes != copied {
        return Err(IndexError::CurrentRepublishSourceTopology(
            "plan byte count does not match copied control file",
        ));
    }
    Ok(copied)
}

pub(super) fn discard_bound_directory(
    generations: &BoundDirectory,
    destination_name: &Path,
    destination: &BoundDirectory,
) -> Result<()> {
    for name in directory_entries(&destination.file, MAX_REPUBLISH_DIRECTORY_ENTRIES)? {
        let relative = Path::new(&name);
        validate_single_component(relative)?;
        let file = open_regular_file_at(&destination.file, relative)?;
        let identity = FileIdentity::from_metadata(&file.metadata()?);
        validate_file_binding(&destination.file, relative, identity)?;
        unlink_at(&destination.file, relative, 0)?;
    }
    validate_child_binding(&generations.file, destination_name, destination.identity)?;
    unlink_at(&generations.file, destination_name, libc::AT_REMOVEDIR)
}

pub(super) fn unlink_at(parent: &File, path: &Path, flags: libc::c_int) -> Result<()> {
    let path = path_cstring(path)?;
    // SAFETY: the parent descriptor and NUL-terminated relative path stay
    // live for the call. Callers retain and revalidate the opened target.
    if unsafe { libc::unlinkat(parent.as_raw_fd(), path.as_ptr(), flags) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error().into())
    }
}

pub(super) fn open_regular_file_at(directory: &File, path: &Path) -> Result<File> {
    let file = open_at_nofollow(directory.as_raw_fd(), path, libc::O_RDONLY)
        .map_err(source_topology_open_error)?;
    let identity = FileIdentity::from_metadata(&file.metadata()?);
    if !identity.is_regular() {
        return Err(IndexError::CurrentRepublishSourceTopology(
            "non-regular directory entry",
        ));
    }
    Ok(file)
}

pub(super) fn create_regular_file_at(directory: &File, path: &Path) -> io::Result<File> {
    let path = path_cstring(path)?;
    // SAFETY: `path` is NUL-terminated, the directory descriptor remains
    // open, and successful ownership is transferred into `File` exactly once.
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            path.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600,
        )
    };
    file_from_fd(fd)
}

pub(super) fn open_path_nofollow(path: &Path, flags: libc::c_int) -> io::Result<File> {
    let path = path_cstring(path)?;
    // SAFETY: `path` is NUL-terminated and successful descriptor ownership
    // is transferred into `File` exactly once.
    let fd = unsafe { libc::open(path.as_ptr(), flags | libc::O_CLOEXEC | libc::O_NOFOLLOW) };
    file_from_fd(fd)
}

pub(super) fn open_at_nofollow(
    directory: RawFd,
    path: &Path,
    flags: libc::c_int,
) -> io::Result<File> {
    let path = path_cstring(path)?;
    // SAFETY: `path` is NUL-terminated, `directory` is borrowed for the
    // call, and successful descriptor ownership transfers exactly once.
    let fd = unsafe {
        libc::openat(
            directory,
            path.as_ptr(),
            flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    file_from_fd(fd)
}

pub(super) fn file_from_fd(fd: libc::c_int) -> io::Result<File> {
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: a nonnegative `open`/`openat` result is a newly owned fd.
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

pub(super) fn path_cstring(path: &Path) -> io::Result<CString> {
    use std::os::unix::ffi::OsStrExt;
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains an interior NUL"))
}

pub(super) fn create_directory_at(parent: &File, path: &Path) -> Result<()> {
    let path = path_cstring(path)?;
    // SAFETY: `path` is NUL-terminated and `parent` remains open.
    if unsafe { libc::mkdirat(parent.as_raw_fd(), path.as_ptr(), 0o700) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error().into())
    }
}

#[cfg(target_os = "linux")]
pub(super) fn hard_link_at(source: &File, path: &Path, destination: &File) -> io::Result<()> {
    let path = path_cstring(path)?;
    // SAFETY: both descriptors and both NUL-terminated path pointers stay
    // valid for the duration of `linkat`.
    if unsafe {
        libc::linkat(
            source.as_raw_fd(),
            path.as_ptr(),
            destination.as_raw_fd(),
            path.as_ptr(),
            0,
        )
    } == 0
    {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
pub(super) fn hard_link_authenticated_source(
    source_directory: &File,
    path: &Path,
    destination: &File,
) -> io::Result<()> {
    hard_link_at(source_directory, path, destination)
}

#[cfg(target_os = "macos")]
pub(super) fn hard_link_authenticated_source(
    _source: &File,
    _path: &Path,
    _destination: &File,
) -> io::Result<()> {
    Err(io::Error::from_raw_os_error(libc::EOPNOTSUPP))
}

#[cfg(target_os = "linux")]
pub(super) fn try_clone_reflink_at(source: &File, destination: &File, path: &Path) -> Result<bool> {
    let destination_file = create_regular_file_at(destination, path)?;
    let result = unsafe {
        libc::ioctl(
            destination_file.as_raw_fd(),
            libc::FICLONE,
            source.as_raw_fd(),
        )
    };
    if result == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error().is_some_and(|code| {
        [
            libc::EOPNOTSUPP,
            libc::ENOTTY,
            libc::EINVAL,
            libc::EXDEV,
            libc::EPERM,
            libc::EACCES,
        ]
        .contains(&code)
    }) {
        drop(destination_file);
        let _ = unlink_at(destination, path, 0);
        return Ok(false);
    }
    Err(error.into())
}

#[cfg(target_os = "macos")]
pub(super) fn try_clone_reflink_at(source: &File, destination: &File, path: &Path) -> Result<bool> {
    let path = path_cstring(path)?;
    let result = unsafe {
        libc::fclonefileat(
            source.as_raw_fd(),
            destination.as_raw_fd(),
            path.as_ptr(),
            0,
        )
    };
    if result == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error().is_some_and(|code| {
        [
            libc::ENOTSUP,
            libc::EINVAL,
            libc::EXDEV,
            libc::EPERM,
            libc::EACCES,
        ]
        .contains(&code)
    }) {
        return Ok(false);
    }
    Err(error.into())
}

pub(super) fn hardlink_copy_fallback_error(error: &io::Error) -> bool {
    error.raw_os_error().is_some_and(|code| {
        [
            libc::EXDEV,
            libc::EPERM,
            libc::EACCES,
            libc::EMLINK,
            libc::EOPNOTSUPP,
            libc::ENOENT,
        ]
        .contains(&code)
    })
}

pub(super) fn validate_path_binding(path: &Path, expected: FileIdentity) -> Result<()> {
    let metadata = fs::symlink_metadata(path).map_err(source_topology_open_error)?;
    let actual = FileIdentity::from_metadata(&metadata);
    if !actual.is_directory() || !actual.is_same_object(expected) {
        return Err(IndexError::CurrentRepublishSourceTopology(
            "generation parent path changed during republish",
        ));
    }
    Ok(())
}

pub(super) fn validate_child_binding(
    parent: &File,
    path: &Path,
    expected: FileIdentity,
) -> Result<()> {
    let actual = stat_at(parent, path)?;
    if !actual.is_directory() || !actual.is_same_object(expected) {
        return Err(IndexError::CurrentRepublishSourceTopology(
            "active generation directory changed during republish",
        ));
    }
    Ok(())
}

pub(super) fn validate_file_binding(
    parent: &File,
    path: &Path,
    expected: FileIdentity,
) -> Result<()> {
    let actual = stat_at(parent, path)?;
    if !actual.is_regular() || actual != expected {
        return Err(IndexError::CurrentRepublishSourceTopology(
            "source file changed during republish",
        ));
    }
    Ok(())
}

pub(super) fn stat_at(parent: &File, path: &Path) -> Result<FileIdentity> {
    let path = path_cstring(path)?;
    // SAFETY: zeroed `stat` is initialized by a successful `fstatat`; the
    // descriptor and path remain valid for the call.
    let mut stat = unsafe { std::mem::zeroed::<libc::stat>() };
    let result = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            path.as_ptr(),
            &mut stat,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == 0 {
        Ok(FileIdentity::from_stat(&stat))
    } else {
        Err(source_topology_open_error(io::Error::last_os_error()))
    }
}

pub(super) fn directory_entries(directory: &File, maximum: usize) -> Result<Vec<OsString>> {
    // SAFETY: `dup` creates an independently owned descriptor.
    let duplicate = unsafe { libc::dup(directory.as_raw_fd()) };
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
        // SAFETY: `stream` remains open and `readdir`'s pointer is consumed
        // before the next call.
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
        if bytes == b"." || bytes == b".." {
            continue;
        }
        let actual = entries
            .len()
            .checked_add(1)
            .ok_or(IndexError::CountOverflow)?;
        if actual > maximum {
            return Err(IndexError::CurrentRepublishFileLimit { actual, maximum });
        }
        entries.push(OsString::from_vec(bytes.to_vec()));
    }
    entries.sort();
    Ok(entries)
}

#[cfg(target_os = "linux")]
pub(super) fn set_errno(value: libc::c_int) {
    // SAFETY: the returned pointer addresses this thread's errno.
    unsafe { *libc::__errno_location() = value };
}

#[cfg(target_os = "macos")]
pub(super) fn set_errno(value: libc::c_int) {
    // SAFETY: the returned pointer addresses this thread's errno.
    unsafe { *libc::__error() = value };
}

pub(super) fn admit_available_bytes(directory: &File, required: u64, recheck: bool) -> Result<()> {
    let available = available_bytes(directory, recheck)?;
    if available < required {
        return Err(IndexError::CurrentRepublishInsufficientHeadroom {
            available,
            required,
        });
    }
    Ok(())
}

pub(super) fn available_bytes(directory: &File, recheck: bool) -> Result<u64> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(available) = super::support::TEST_CLONE_OPTIONS.with(|options| {
        let options = options.borrow();
        if recheck {
            options
                .rechecked_available_bytes
                .or(options.available_bytes)
        } else {
            options.available_bytes
        }
    }) {
        return Ok(available);
    }
    #[cfg(not(any(test, feature = "test-support")))]
    let _ = recheck;
    // SAFETY: zeroed `statvfs` is initialized by successful `fstatvfs`.
    let mut stat = unsafe { std::mem::zeroed::<libc::statvfs>() };
    if unsafe { libc::fstatvfs(directory.as_raw_fd(), &mut stat) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok((stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64))
}

pub(super) fn source_topology_open_error(error: io::Error) -> IndexError {
    if error
        .raw_os_error()
        .is_some_and(|code| [libc::ELOOP, libc::ENOTDIR].contains(&code))
    {
        IndexError::CurrentRepublishSourceTopology("symlinked or non-directory republish source")
    } else {
        IndexError::Io(error)
    }
}
