//! Ordinary owner-private files; all reads retain the selected descriptor.
#[cfg(unix)]
use std::fs;
use std::fs::File;
use std::io;
use std::path::Path;

pub(crate) use ctx_history_platform::platform_security::{
    create_private_directory_all, create_private_file_new,
};

pub(crate) fn open(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        ctx_history_platform::platform_security::verify_private_file_handle(&file)?;
        // A selected active manifest may have been unlinked by atomic
        // publication after open; the retained descriptor still names its
        // complete immutable bytes. Multiple links are never index files.
        if file.metadata()?.nlink() > 1 {
            return Err(io::Error::other("index file has multiple links"));
        }
        Ok(file)
    }
    #[cfg(windows)]
    {
        ctx_history_platform::platform_security::open_verified_private_file(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "index file platform",
        ))
    }
}

pub(crate) fn sync_directory(root: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(root)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = root;
        // Windows atomic replacement uses WRITE_THROUGH below.
        Ok(())
    }
}

#[cfg(unix)]
pub(crate) fn replace(candidate: &Path, active: &Path) -> io::Result<()> {
    fs::rename(candidate, active)
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn replace(_: &Path, _: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic index publication",
    ))
}

#[cfg(windows)]
#[allow(unsafe_code)]
pub(crate) fn replace(candidate: &Path, active: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let candidate: Vec<_> = candidate.as_os_str().encode_wide().chain(Some(0)).collect();
    let active: Vec<_> = active.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: both owned buffers are NUL-terminated and live through this call.
    let result = unsafe {
        MoveFileExW(
            candidate.as_ptr(),
            active.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
