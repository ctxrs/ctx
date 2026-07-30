use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
use std::{
    fs::File,
    io::Read,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
};

use super::{source_access, ContinueNativePathError};

#[cfg(target_os = "linux")]
pub(super) struct RootMutationWatch {
    file: Mutex<File>,
    mutated: AtomicBool,
}

#[cfg(target_os = "linux")]
impl std::fmt::Debug for RootMutationWatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RootMutationWatch")
            .field("mutated", &self.mutated.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

#[cfg(target_os = "linux")]
impl RootMutationWatch {
    pub(super) fn new(root: &Path) -> Result<Self, ContinueNativePathError> {
        use std::os::fd::FromRawFd;

        let descriptor = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if descriptor < 0 {
            return Err(source_access(root, std::io::Error::last_os_error()));
        }
        let watch = Self {
            file: Mutex::new(unsafe { File::from_raw_fd(descriptor) }),
            mutated: AtomicBool::new(false),
        };
        watch.add(root)?;
        Ok(watch)
    }

    pub(super) fn add(&self, path: &Path) -> Result<(), ContinueNativePathError> {
        use std::{ffi::CString, os::unix::ffi::OsStrExt, os::unix::io::AsRawFd};

        let path_bytes = path.as_os_str().as_bytes();
        let path = CString::new(path_bytes).map_err(|_| ContinueNativePathError::SourceAccess {
            path: path.to_path_buf(),
            message: "Continue inventory path contains an interior NUL".to_owned(),
        })?;
        let guard = self
            .file
            .lock()
            .map_err(|_| ContinueNativePathError::SourceAccess {
                path: PathBuf::from("<continue-inventory-watch>"),
                message: "Continue inventory mutation watch is poisoned".to_owned(),
            })?;
        let mask = libc::IN_ATTRIB
            | libc::IN_CLOSE_WRITE
            | libc::IN_CREATE
            | libc::IN_DELETE
            | libc::IN_DELETE_SELF
            | libc::IN_MODIFY
            | libc::IN_MOVE_SELF
            | libc::IN_MOVED_FROM
            | libc::IN_MOVED_TO;
        let result = unsafe { libc::inotify_add_watch(guard.as_raw_fd(), path.as_ptr(), mask) };
        if result < 0 {
            return Err(source_access(
                Path::new(path.to_str().unwrap_or("<continue-inventory-watch>")),
                std::io::Error::last_os_error(),
            ));
        }
        Ok(())
    }

    pub(super) fn mutated(&self) -> bool {
        if self.mutated.load(Ordering::Acquire) {
            return true;
        }
        let Ok(mut file) = self.file.lock() else {
            self.mutated.store(true, Ordering::Release);
            return true;
        };
        let mut buffer = [0_u8; 8 * 1024];
        loop {
            match file.read(&mut buffer) {
                Ok(0) => break,
                Ok(_) => self.mutated.store(true, Ordering::Release),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => {
                    self.mutated.store(true, Ordering::Release);
                    break;
                }
            }
        }
        self.mutated.load(Ordering::Acquire)
    }
}

#[cfg(not(target_os = "linux"))]
#[derive(Debug)]
pub(super) struct RootMutationWatch;

#[cfg(not(target_os = "linux"))]
impl RootMutationWatch {
    pub(super) fn new(_root: &Path) -> Result<Self, ContinueNativePathError> {
        Ok(Self)
    }

    pub(super) fn add(&self, _path: &Path) -> Result<(), ContinueNativePathError> {
        Ok(())
    }

    pub(super) fn mutated(&self) -> bool {
        false
    }
}
