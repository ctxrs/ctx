//! A Tantivy directory whose atomic publications include the durability barrier
//! required before Tantivy may garbage-collect the previous generation.

use std::{
    ffi::OsStr,
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
/// provide. Higher-level publication code must reconcile the visible target;
/// predecessor migration exposes that case as a committed recovery outcome.
#[derive(Clone)]
pub(crate) struct DurableMmapDirectory {
    inner: MmapDirectory,
    root_path: Arc<PathBuf>,
}

#[derive(Debug)]
pub(crate) enum DurableAtomicWriteOutcome {
    Durable,
    VisibleButDurabilityUncertain(io::Error),
}

impl DurableAtomicWriteOutcome {
    fn into_io_result(self) -> io::Result<()> {
        match self {
            Self::Durable => Ok(()),
            Self::VisibleButDurabilityUncertain(error) => Err(error),
        }
    }
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

    pub(crate) fn atomic_write_with_outcome(
        &self,
        path: &Path,
        data: &[u8],
    ) -> io::Result<DurableAtomicWriteOutcome> {
        let target_path = self.resolve_path(path);
        let parent_path = target_path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("path {} has no parent directory", target_path.display()),
            )
        })?;
        // Open the synchronization handle before publication. Any later error
        // is therefore known to occur either before replacement or after the
        // target became visible.
        let parent_sync = ParentDirectorySync::open(parent_path)?;
        atomic_replace_with_outcome(&target_path, data, replace_file, move || parent_sync.sync())
    }

    fn resolve_path(&self, relative_path: &Path) -> PathBuf {
        self.root_path.join(relative_path)
    }
}

pub(crate) fn reclaim_abandoned_atomic_writes(directory_path: &Path) -> io::Result<()> {
    let mut removed_file = false;
    for entry in fs::read_dir(directory_path)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() || !is_atomic_temporary_file(&entry.file_name()) {
            continue;
        }
        match fs::remove_file(entry.path()) {
            Ok(()) => removed_file = true,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    if removed_file {
        ParentDirectorySync::open(directory_path)?.sync()?;
    }
    Ok(())
}

/// Atomically replaces `target` with an already-synchronized staged file.
///
/// Both paths must have the same parent so the operation cannot degrade into a
/// cross-filesystem copy. The published file and its directory entry are
/// synchronized before this function returns. Windows uses `MoveFileExW` with
/// `MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH`; directory flushing is
/// intentionally skipped there because it is not a reliable Windows durability
/// primitive.
pub fn durable_atomic_replace_file(source: &Path, target: &Path) -> io::Result<()> {
    let source_parent = source.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("source path {} has no parent directory", source.display()),
        )
    })?;
    let target_parent = target.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("target path {} has no parent directory", target.display()),
        )
    })?;
    if source_parent != target_parent {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "atomic replacement requires one directory: {} and {} differ",
                source_parent.display(),
                target_parent.display()
            ),
        ));
    }

    // Acquire the directory synchronization handle before publication so a
    // failure to open it cannot occur after the target becomes visible.
    let parent_sync = ParentDirectorySync::open(target_parent)?;
    replace_file(source, target)?;
    File::open(target)?.sync_all()?;
    parent_sync.sync()
}

fn is_atomic_temporary_file(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    let Some(identifier) = name
        .strip_prefix(TEMPORARY_FILE_PREFIX)
        .and_then(|name| name.strip_suffix(".tmp"))
    else {
        return false;
    };
    identifier.len() == 32
        && identifier
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
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
        self.atomic_write_with_outcome(path, data)?.into_io_result()
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

#[cfg(test)]
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
    atomic_replace_with_outcome(target_path, data, replace, sync_parent)?.into_io_result()
}

fn atomic_replace_with_outcome<Replace, SyncParent>(
    target_path: &Path,
    data: &[u8],
    replace: Replace,
    sync_parent: SyncParent,
) -> io::Result<DurableAtomicWriteOutcome>
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

    atomic_write_checkpoint(AtomicWriteStage::BeforeTemporaryWrite, target_path)?;

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

    atomic_write_checkpoint(
        AtomicWriteStage::AfterTemporarySyncBeforeReplace,
        target_path,
    )?;
    atomic_write_checkpoint(AtomicWriteStage::BeforeReplace, target_path)?;

    if let Err(error) = replace(&temporary_path, target_path) {
        let _ = fs::remove_file(&temporary_path);
        return Err(error);
    }

    if let Err(error) = atomic_write_checkpoint(
        AtomicWriteStage::AfterReplaceBeforeDirectorySync,
        target_path,
    ) {
        return Ok(DurableAtomicWriteOutcome::VisibleButDurabilityUncertain(
            error,
        ));
    }

    match sync_parent() {
        Ok(()) => Ok(DurableAtomicWriteOutcome::Durable),
        Err(error) => Ok(DurableAtomicWriteOutcome::VisibleButDurabilityUncertain(
            error,
        )),
    }
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AtomicWriteStage {
    BeforeTemporaryWrite,
    AfterTemporarySyncBeforeReplace,
    BeforeReplace,
    AfterReplaceBeforeDirectorySync,
}

#[cfg(not(test))]
#[derive(Debug, Clone, Copy)]
enum AtomicWriteStage {
    BeforeTemporaryWrite,
    AfterTemporarySyncBeforeReplace,
    BeforeReplace,
    AfterReplaceBeforeDirectorySync,
}

#[cfg(test)]
type AtomicWriteTestHook = Box<dyn for<'a> FnMut(AtomicWriteStage, &'a Path) -> io::Result<()>>;

#[cfg(test)]
thread_local! {
    static ATOMIC_WRITE_TEST_HOOK: std::cell::RefCell<Option<AtomicWriteTestHook>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
pub(crate) struct AtomicWriteTestHookGuard(Option<AtomicWriteTestHook>);

#[cfg(test)]
impl AtomicWriteTestHookGuard {
    pub(crate) fn set<F>(hook: F) -> Self
    where
        F: for<'a> FnMut(AtomicWriteStage, &'a Path) -> io::Result<()> + 'static,
    {
        let previous = ATOMIC_WRITE_TEST_HOOK.with(|active| active.replace(Some(Box::new(hook))));
        Self(previous)
    }
}

#[cfg(test)]
impl Drop for AtomicWriteTestHookGuard {
    fn drop(&mut self) {
        ATOMIC_WRITE_TEST_HOOK.with(|active| active.replace(self.0.take()));
    }
}

#[cfg(test)]
fn atomic_write_checkpoint(stage: AtomicWriteStage, target: &Path) -> io::Result<()> {
    ATOMIC_WRITE_TEST_HOOK.with(|active| {
        let mut active = active.borrow_mut();
        match active.as_mut() {
            Some(hook) => hook(stage, target),
            None => Ok(()),
        }
    })
}

#[cfg(not(test))]
fn atomic_write_checkpoint(_stage: AtomicWriteStage, _target: &Path) -> io::Result<()> {
    Ok(())
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
    fn durable_staged_file_replacement_supports_first_publish_and_replace() {
        let temporary_directory = tempdir().unwrap();
        let target = temporary_directory.path().join("projection.sqlite");
        let first = temporary_directory.path().join("projection.first");
        fs::write(&first, b"first").unwrap();
        File::open(&first).unwrap().sync_all().unwrap();

        durable_atomic_replace_file(&first, &target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"first");
        assert!(!first.exists());

        let replacement = temporary_directory.path().join("projection.replacement");
        fs::write(&replacement, b"replacement").unwrap();
        File::open(&replacement).unwrap().sync_all().unwrap();

        durable_atomic_replace_file(&replacement, &target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"replacement");
        assert!(!replacement.exists());
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

    #[test]
    fn abandoned_atomic_write_reclamation_is_limited_to_owned_regular_files() {
        let temporary_directory = tempdir().unwrap();
        let owned = temporary_directory
            .path()
            .join(".ctx-tantivy-atomic-0123456789abcdef0123456789abcdef.tmp");
        let near_miss = temporary_directory
            .path()
            .join(".ctx-tantivy-atomic-0123456789abcdef0123456789abcdef.tmp.keep");
        let foreign = temporary_directory.path().join("foreign.tmp");
        let matching_directory = temporary_directory
            .path()
            .join(".ctx-tantivy-atomic-fedcba9876543210fedcba9876543210.tmp");
        fs::write(&owned, b"abandoned").unwrap();
        fs::write(&near_miss, b"preserve").unwrap();
        fs::write(&foreign, b"preserve").unwrap();
        fs::create_dir(&matching_directory).unwrap();

        reclaim_abandoned_atomic_writes(temporary_directory.path()).unwrap();

        assert!(!owned.exists());
        assert!(near_miss.is_file());
        assert!(foreign.is_file());
        assert!(matching_directory.is_dir());
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
