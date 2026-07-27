use super::*;

#[derive(Debug)]
pub(super) struct ContinuePathSpool {
    pub(super) file: File,
    pub(super) entries: usize,
    pub(super) bytes: u64,
    pub(super) maximum_record_bytes: usize,
}

impl ContinuePathSpool {
    pub(super) fn new(_root: &Path) -> Result<Self, ContinueNativePathError> {
        let file = tempfile::tempfile().map_err(|source| ContinueNativePathError::SystemIo {
            operation: "create Continue path spool",
            source,
        })?;
        Ok(Self {
            file,
            entries: 0,
            bytes: 0,
            maximum_record_bytes: 0,
        })
    }

    pub(super) fn push(&mut self, path: &Path) -> Result<(), ContinueNativePathError> {
        let encoded = encode_path(path).ok_or_else(|| ContinueNativePathError::SourceAccess {
            path: path.to_path_buf(),
            message: "Continue source path cannot be represented in the private path spool"
                .to_owned(),
        })?;
        if encoded.len() > MAX_SPOOLED_PATH_BYTES {
            return Err(ContinueNativePathError::SourceAccess {
                path: path.to_path_buf(),
                message: "Continue source path exceeds the private path-spool record limit"
                    .to_owned(),
            });
        }
        let length =
            u32::try_from(encoded.len()).map_err(|_| ContinueNativePathError::SourceAccess {
                path: path.to_path_buf(),
                message: "Continue source path exceeds u32 path-spool encoding".to_owned(),
            })?;
        if let Some(source) = injected_io_failure(ContinueInjectedIoOperation::SpoolWrite, path) {
            return Err(ContinueNativePathError::SystemIo {
                operation: "write Continue path spool",
                source,
            });
        }
        self.file
            .write_all(&length.to_le_bytes())
            .and_then(|_| self.file.write_all(&encoded))
            .map_err(|source| ContinueNativePathError::SystemIo {
                operation: "write Continue path spool",
                source,
            })?;
        self.entries = self.entries.saturating_add(1);
        self.bytes = self
            .bytes
            .saturating_add(u64::from(length).saturating_add(4));
        self.maximum_record_bytes = self.maximum_record_bytes.max(encoded.len());
        Ok(())
    }

    pub(super) fn iter(&self) -> Result<ContinuePathIter, ContinueNativePathError> {
        let file = self
            .file
            .try_clone()
            .map_err(|source| ContinueNativePathError::SystemIo {
                operation: "clone Continue path spool",
                source,
            })?;
        Ok(ContinuePathIter {
            file,
            offset: 0,
            remaining: self.entries,
        })
    }
}

pub(crate) struct ContinuePathIter {
    file: File,
    offset: u64,
    remaining: usize,
}

impl Iterator for ContinuePathIter {
    type Item = Result<PathBuf, ContinueNativePathError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let mut length = [0_u8; 4];
        if let Err(error) = read_exact_at(&self.file, &mut length, self.offset) {
            self.remaining = 0;
            return Some(Err(ContinueNativePathError::SystemIo {
                operation: "read Continue path spool",
                source: error,
            }));
        }
        self.offset = self.offset.saturating_add(4);
        let length = u32::from_le_bytes(length) as usize;
        if length > MAX_SPOOLED_PATH_BYTES {
            self.remaining = 0;
            return Some(Err(ContinueNativePathError::Invariant {
                message: "private Continue path spool contains an oversized record",
            }));
        }
        let mut encoded = vec![0_u8; length];
        if let Err(error) = read_exact_at(&self.file, &mut encoded, self.offset) {
            self.remaining = 0;
            return Some(Err(ContinueNativePathError::SystemIo {
                operation: "read Continue path spool",
                source: error,
            }));
        }
        self.offset = self
            .offset
            .saturating_add(u64::try_from(length).unwrap_or(u64::MAX));
        self.remaining = self.remaining.saturating_sub(1);
        Some(
            decode_path(encoded).ok_or(ContinueNativePathError::Invariant {
                message: "private Continue path spool contains an invalid path record",
            }),
        )
    }
}

#[cfg(unix)]
pub(super) fn read_exact_at(
    file: &File,
    buffer: &mut [u8],
    mut offset: u64,
) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;

    let mut remaining = buffer;
    while !remaining.is_empty() {
        let read = file.read_at(remaining, offset)?;
        if read == 0 {
            return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
        }
        offset = offset.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        remaining = &mut remaining[read..];
    }
    Ok(())
}

#[cfg(windows)]
pub(super) fn read_exact_at(
    file: &File,
    buffer: &mut [u8],
    mut offset: u64,
) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;

    let mut remaining = buffer;
    while !remaining.is_empty() {
        let read = file.seek_read(remaining, offset)?;
        if read == 0 {
            return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
        }
        offset = offset.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        remaining = &mut remaining[read..];
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
pub(super) fn read_exact_at(file: &File, buffer: &mut [u8], offset: u64) -> std::io::Result<()> {
    use std::io::{Read as _, Seek as _, SeekFrom};

    let mut file = file.try_clone()?;
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(buffer)
}

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
                Ok(_) => {
                    self.mutated.store(true, Ordering::Release);
                }
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
