//! A Tantivy directory whose atomic publications include the durability barrier
//! required before Tantivy may garbage-collect the previous generation.

use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use tantivy::directory::{
    error::{DeleteError, LockError, OpenDirectoryError, OpenReadError, OpenWriteError},
    Directory, DirectoryLock, FileHandle, FileSlice, Lock, MmapDirectory, WatchCallback,
    WatchHandle, WritePtr,
};
use uuid::Uuid;

const TEMPORARY_FILE_PREFIX: &str = ".ctx-tantivy-atomic-";
const TEMPORARY_FILE_ATTEMPTS: usize = 16;

/// An [`MmapDirectory`] that does not return from `atomic_write` until the
/// replacement is durable.
///
/// Tantivy publishes `meta.json` and then immediately becomes free to garbage
/// collect files from the previous generation. `MmapDirectory` synchronizes
/// the temporary file before replacing `meta.json`, but on Unix its atomic
/// write does not synchronize the containing directory after the replacement.
/// This wrapper owns that final barrier.
///
/// A failure from the final directory synchronization happens after the
/// replacement became visible. Returning that error is intentional: reporting
/// success would claim a durability guarantee that the filesystem did not
/// provide.
#[derive(Clone)]
pub(crate) struct DurableMmapDirectory {
    inner: MmapDirectory,
    root_path: Arc<PathBuf>,
}

impl DurableMmapDirectory {
    pub(crate) fn open(directory_path: impl AsRef<Path>) -> Result<Self, OpenDirectoryError> {
        let directory_path = directory_path.as_ref();
        let inner = MmapDirectory::open(directory_path)?;
        let root_path = canonical_root_path(directory_path)?;
        Ok(Self {
            inner,
            root_path: Arc::new(root_path),
        })
    }

    pub(crate) fn root_path(&self) -> &Path {
        &self.root_path
    }

    fn resolve_path(&self, relative_path: &Path) -> PathBuf {
        self.root_path.join(relative_path)
    }
}

impl fmt::Debug for DurableMmapDirectory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("DurableMmapDirectory")
            .field(&self.root_path)
            .finish()
    }
}

impl Directory for DurableMmapDirectory {
    fn get_file_handle(&self, path: &Path) -> Result<Arc<dyn FileHandle>, OpenReadError> {
        self.inner.get_file_handle(path)
    }

    fn open_read(&self, path: &Path) -> Result<FileSlice, OpenReadError> {
        self.inner.open_read(path)
    }

    fn delete(&self, path: &Path) -> Result<(), DeleteError> {
        self.inner.delete(path)
    }

    fn exists(&self, path: &Path) -> Result<bool, OpenReadError> {
        self.inner.exists(path)
    }

    fn open_write(&self, path: &Path) -> Result<WritePtr, OpenWriteError> {
        self.inner.open_write(path)
    }

    fn atomic_read(&self, path: &Path) -> Result<Vec<u8>, OpenReadError> {
        self.inner.atomic_read(path)
    }

    fn atomic_write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        let target_path = self.resolve_path(path);
        let parent_path = target_path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("path {} has no parent directory", target_path.display()),
            )
        })?;
        // Open the directory before publication so failure to acquire the
        // synchronization handle cannot happen after the target is replaced.
        let parent_sync = ParentDirectorySync::open(parent_path)?;
        atomic_replace_with(&target_path, data, replace_file, move || parent_sync.sync())
    }

    fn sync_directory(&self) -> io::Result<()> {
        self.inner.sync_directory()
    }

    fn acquire_lock(&self, lock: &Lock) -> Result<DirectoryLock, LockError> {
        self.inner.acquire_lock(lock)
    }

    fn watch(&self, watch_callback: WatchCallback) -> tantivy::Result<WatchHandle> {
        self.inner.watch(watch_callback)
    }
}

fn canonical_root_path(directory_path: &Path) -> Result<PathBuf, OpenDirectoryError> {
    match directory_path.canonicalize() {
        Ok(canonical_path) => Ok(canonical_path),
        Err(io_error) => {
            // Match MmapDirectory's public Windows behavior for virtual drives,
            // where canonicalize can fail with ERROR_INVALID_FUNCTION even
            // though the directory was successfully opened.
            #[cfg(windows)]
            if io_error.raw_os_error() == Some(1) && directory_path.exists() {
                return Ok(directory_path.to_path_buf());
            }
            Err(OpenDirectoryError::wrap_io_error(
                io_error,
                directory_path.to_path_buf(),
            ))
        }
    }
}

fn atomic_replace_with<Replace, SyncParent>(
    target_path: &Path,
    data: &[u8],
    replace: Replace,
    sync_parent: SyncParent,
) -> io::Result<()>
where
    Replace: FnOnce(&Path, &Path) -> io::Result<()>,
    SyncParent: FnOnce() -> io::Result<()>,
{
    let parent_path = target_path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path {} has no parent directory", target_path.display()),
        )
    })?;
    let (temporary_path, mut temporary_file) = create_temporary_file(parent_path)?;

    let write_result = temporary_file
        .write_all(data)
        .and_then(|()| temporary_file.flush())
        .and_then(|()| temporary_file.sync_all());
    // Windows does not permit moving this file while its default, non-sharing
    // handle is open.
    drop(temporary_file);
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temporary_path);
        return Err(error);
    }

    if let Err(error) = replace(&temporary_path, target_path) {
        let _ = fs::remove_file(&temporary_path);
        return Err(error);
    }

    sync_parent()
}

fn create_temporary_file(parent_path: &Path) -> io::Result<(PathBuf, File)> {
    for _ in 0..TEMPORARY_FILE_ATTEMPTS {
        let temporary_path = parent_path.join(format!(
            "{TEMPORARY_FILE_PREFIX}{}.tmp",
            Uuid::new_v4().simple()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary_path)
        {
            Ok(file) => return Ok((temporary_path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique Tantivy atomic-write file",
    ))
}

#[cfg(not(windows))]
struct ParentDirectorySync(File);

#[cfg(not(windows))]
impl ParentDirectorySync {
    fn open(parent_path: &Path) -> io::Result<Self> {
        File::open(parent_path).map(Self)
    }

    fn sync(self) -> io::Result<()> {
        self.0.sync_all()
    }
}

#[cfg(windows)]
struct ParentDirectorySync;

#[cfg(windows)]
impl ParentDirectorySync {
    fn open(_parent_path: &Path) -> io::Result<Self> {
        Ok(Self)
    }

    fn sync(self) -> io::Result<()> {
        // MoveFileExW with MOVEFILE_WRITE_THROUGH, used below, does not
        // return until the move has reached disk. Opening and flushing a
        // directory handle is not a reliable substitute on Windows: it is a
        // no-op on local disks and can fail on virtual drives.
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(source: &Path, target: &Path) -> io::Result<()> {
    fs::rename(source, target)
}

#[cfg(windows)]
fn replace_file(source: &Path, target: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        #[link_name = "MoveFileExW"]
        fn move_file_ex_w(
            existing_file_name: *const u16,
            new_file_name: *const u16,
            flags: u32,
        ) -> i32;
    }

    fn nul_terminated(path: &Path) -> io::Result<Vec<u16>> {
        let mut path_wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
        if path_wide.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Windows path contains an interior NUL",
            ));
        }
        path_wide.push(0);
        Ok(path_wide)
    }

    let source_wide = nul_terminated(source)?;
    let target_wide = nul_terminated(target)?;
    // SAFETY: both path buffers are NUL-terminated and remain alive for the
    // duration of the call.
    let moved = unsafe {
        move_file_ex_w(
            source_wide.as_ptr(),
            target_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn atomic_write_replaces_existing_file() {
        let temporary_directory = tempdir().unwrap();
        let directory = DurableMmapDirectory::open(temporary_directory.path()).unwrap();
        let path = Path::new("meta.json");

        directory.atomic_write(path, b"previous").unwrap();
        directory.atomic_write(path, b"replacement").unwrap();

        assert_eq!(directory.atomic_read(path).unwrap(), b"replacement");
        assert_no_temporary_files(temporary_directory.path());
    }

    #[test]
    fn replacement_failure_preserves_previous_file_and_removes_temporary_file() {
        let temporary_directory = tempdir().unwrap();
        let target_path = temporary_directory.path().join("meta.json");
        fs::write(&target_path, b"previous").unwrap();

        let error = atomic_replace_with(
            &target_path,
            b"replacement",
            |temporary_path, target_path| {
                assert_eq!(fs::read(temporary_path).unwrap(), b"replacement");
                assert_eq!(fs::read(target_path).unwrap(), b"previous");
                Err(io::Error::other("injected replacement failure"))
            },
            || panic!("parent sync must not run after replacement failure"),
        )
        .unwrap_err();

        assert_eq!(error.to_string(), "injected replacement failure");
        assert_eq!(fs::read(&target_path).unwrap(), b"previous");
        assert_no_temporary_files(temporary_directory.path());
    }

    #[test]
    fn parent_sync_failure_is_reported_after_replacement_is_visible() {
        let temporary_directory = tempdir().unwrap();
        let target_path = temporary_directory.path().join("meta.json");
        fs::write(&target_path, b"previous").unwrap();
        let sync_attempted = AtomicBool::new(false);

        let error = atomic_replace_with(&target_path, b"replacement", replace_file, || {
            sync_attempted.store(true, Ordering::SeqCst);
            Err(io::Error::other("injected parent sync failure"))
        })
        .unwrap_err();

        assert_eq!(error.to_string(), "injected parent sync failure");
        assert!(sync_attempted.load(Ordering::SeqCst));
        assert_eq!(fs::read(&target_path).unwrap(), b"replacement");
        assert_no_temporary_files(temporary_directory.path());
    }

    fn assert_no_temporary_files(directory: &Path) {
        let temporary_files = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.to_string_lossy().starts_with(TEMPORARY_FILE_PREFIX))
            .collect::<Vec<_>>();
        assert!(temporary_files.is_empty(), "{temporary_files:?}");
    }
}
