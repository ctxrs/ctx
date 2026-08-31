//! A Tantivy directory whose atomic publications include the durability barrier
//! required before Tantivy may garbage-collect the previous generation.

use std::{
    ffi::OsStr,
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Read as _, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt as _;
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt as _;

use fs4::fs_std::FileExt as _;
use tantivy::directory::{
    error::{DeleteError, LockError, OpenDirectoryError, OpenReadError, OpenWriteError},
    Directory, DirectoryLock, FileHandle, FileSlice, Lock, MmapDirectory, WatchCallback,
    WatchHandle, WritePtr,
};
use tantivy::HasLen;
use uuid::Uuid;

use ctx_history_platform::platform_security::{
    restrict_private_file_handle, verify_private_file_handle,
};

#[cfg(any(test, feature = "test-support"))]
use crate::publication_probe::{
    publication_io_checkpoint, publication_io_observer_active, PublicationIoEvent,
    PublicationIoProbeGuard,
};

#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_WRITE, READ_CONTROL, WRITE_DAC,
};

const TEMPORARY_FILE_PREFIX: &str = ".ctx-tantivy-atomic-";
const TEMPORARY_FILE_ATTEMPTS: usize = 16;

#[cfg(any(test, feature = "test-support"))]
mod publication_failure_probe;

#[cfg(windows)]
mod windows_replace;

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
pub struct DurableMmapDirectory {
    inner: DurableDirectoryBackend,
    root_path: Arc<PathBuf>,
}

#[derive(Clone)]
enum DurableDirectoryBackend {
    Mmap(MmapDirectory),
    Anchored(Arc<crate::read_root::OpenedDirectory>),
}

#[derive(Debug)]
pub enum DurableAtomicWriteOutcome {
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
    pub fn open(directory_path: impl AsRef<Path>) -> Result<Self, OpenDirectoryError> {
        let directory_path = directory_path.as_ref();
        if let Some(opened) =
            crate::read_root::registered_read_directory(directory_path).map_err(|error| {
                OpenDirectoryError::wrap_io_error(error, directory_path.to_path_buf())
            })?
        {
            return Ok(Self {
                inner: DurableDirectoryBackend::Anchored(opened),
                root_path: Arc::new(directory_path.to_path_buf()),
            });
        }
        let inner = DurableDirectoryBackend::Mmap(MmapDirectory::open(directory_path)?);
        let root_path = canonical_root_path(directory_path)?;
        Ok(Self {
            inner,
            root_path: Arc::new(root_path),
        })
    }

    pub fn root_path(&self) -> &Path {
        &self.root_path
    }

    pub fn atomic_write_with_outcome(
        &self,
        path: &Path,
        data: &[u8],
    ) -> io::Result<DurableAtomicWriteOutcome> {
        if matches!(&self.inner, DurableDirectoryBackend::Anchored(_)) {
            return Err(read_only_directory_error());
        }
        match self.atomic_write_with_outcome_validated(path, data, || Ok(())) {
            Ok(outcome) => Ok(outcome),
            Err(crate::GenerationError::Io(error)) => Err(error),
            Err(error) => Err(io::Error::other(error)),
        }
    }

    pub(crate) fn atomic_write_with_outcome_validated<F>(
        &self,
        path: &Path,
        data: &[u8],
        validate_before_replace: F,
    ) -> crate::Result<DurableAtomicWriteOutcome>
    where
        F: FnOnce() -> crate::Result<()>,
    {
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
        atomic_replace_with_outcome_validated(
            &target_path,
            data,
            replace_file,
            move || parent_sync.sync(),
            validate_before_replace,
        )
    }

    fn resolve_path(&self, relative_path: &Path) -> PathBuf {
        self.root_path.join(relative_path)
    }
}

pub fn reclaim_abandoned_atomic_writes(directory_path: &Path) -> io::Result<()> {
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
    #[cfg(not(windows))]
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
        match &self.inner {
            DurableDirectoryBackend::Mmap(inner) => inner.get_file_handle(path),
            DurableDirectoryBackend::Anchored(inner) => {
                let file = inner.open_file(path).map_err(|error| {
                    if error.kind() == io::ErrorKind::NotFound {
                        OpenReadError::FileDoesNotExist(path.to_path_buf())
                    } else {
                        OpenReadError::wrap_io_error(error, path.to_path_buf())
                    }
                })?;
                AnchoredFileHandle::new(file)
                    .map(|handle| Arc::new(handle) as Arc<dyn FileHandle>)
                    .map_err(|error| OpenReadError::wrap_io_error(error, path.to_path_buf()))
            }
        }
    }

    fn open_read(&self, path: &Path) -> Result<FileSlice, OpenReadError> {
        match &self.inner {
            DurableDirectoryBackend::Mmap(inner) => inner.open_read(path),
            DurableDirectoryBackend::Anchored(_) => self.get_file_handle(path).map(FileSlice::new),
        }
    }

    fn delete(&self, path: &Path) -> Result<(), DeleteError> {
        match &self.inner {
            DurableDirectoryBackend::Mmap(inner) => inner.delete(path),
            DurableDirectoryBackend::Anchored(_) => Err(DeleteError::IoError {
                io_error: Arc::new(read_only_directory_error()),
                filepath: path.to_path_buf(),
            }),
        }
    }

    fn exists(&self, path: &Path) -> Result<bool, OpenReadError> {
        match &self.inner {
            DurableDirectoryBackend::Mmap(inner) => inner.exists(path),
            DurableDirectoryBackend::Anchored(inner) => match inner.open_file(path) {
                Ok(_) => Ok(true),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(OpenReadError::wrap_io_error(error, path.to_path_buf())),
            },
        }
    }

    fn open_write(&self, path: &Path) -> Result<WritePtr, OpenWriteError> {
        match &self.inner {
            DurableDirectoryBackend::Mmap(inner) => inner.open_write(path),
            DurableDirectoryBackend::Anchored(_) => Err(OpenWriteError::wrap_io_error(
                read_only_directory_error(),
                path.to_path_buf(),
            )),
        }
    }

    fn atomic_read(&self, path: &Path) -> Result<Vec<u8>, OpenReadError> {
        match &self.inner {
            DurableDirectoryBackend::Mmap(inner) => inner.atomic_read(path),
            DurableDirectoryBackend::Anchored(inner) => {
                let mut file = inner.open_file(path).map_err(|error| {
                    if error.kind() == io::ErrorKind::NotFound {
                        OpenReadError::FileDoesNotExist(path.to_path_buf())
                    } else {
                        OpenReadError::wrap_io_error(error, path.to_path_buf())
                    }
                })?;
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes)
                    .map_err(|error| OpenReadError::wrap_io_error(error, path.to_path_buf()))?;
                Ok(bytes)
            }
        }
    }

    fn atomic_write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        self.atomic_write_with_outcome(path, data)?.into_io_result()
    }

    fn sync_directory(&self) -> io::Result<()> {
        match &self.inner {
            DurableDirectoryBackend::Mmap(inner) => inner.sync_directory(),
            DurableDirectoryBackend::Anchored(inner) => inner.sync(),
        }
    }

    fn acquire_lock(&self, lock: &Lock) -> Result<DirectoryLock, LockError> {
        match &self.inner {
            DurableDirectoryBackend::Mmap(_) => {
                let file = open_private_lock_file(&self.resolve_path(&lock.filepath))
                    .map_err(LockError::wrap_io_error)?;
                if lock.is_blocking {
                    file.lock_exclusive().map_err(LockError::wrap_io_error)?;
                } else if !file
                    .try_lock_exclusive()
                    .map_err(LockError::wrap_io_error)?
                {
                    return Err(LockError::LockBusy);
                }
                Ok(Box::new(PrivateFileLock { _file: file }).into())
            }
            // Immutable generations are protected from outer reclamation by
            // GenerationReadLease. Tantivy's meta lock only coordinates its
            // own mutable-directory GC, which cannot run through this
            // read-only capability.
            DurableDirectoryBackend::Anchored(_) => {
                let _ = lock;
                Ok(Box::new(()).into())
            }
        }
    }

    fn watch(&self, watch_callback: WatchCallback) -> tantivy::Result<WatchHandle> {
        match &self.inner {
            DurableDirectoryBackend::Mmap(inner) => inner.watch(watch_callback),
            DurableDirectoryBackend::Anchored(_) => {
                let _ = watch_callback;
                Ok(WatchHandle::empty())
            }
        }
    }
}

struct PrivateFileLock {
    _file: File,
}

fn open_private_lock_file(path: &Path) -> io::Result<File> {
    let mut create_options = private_lock_file_options();
    create_options.create_new(true);
    let (file, created) = match create_options.open(path) {
        Ok(file) => (file, true),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            (private_lock_file_options().open(path)?, false)
        }
        Err(error) => return Err(error),
    };

    // A new file must always be normalized because Unix umask can remove
    // owner bits and Windows can inherit a permissive ACL. Existing secure
    // locks stay untouched so acquiring them cannot advance their ctime.
    if created || verify_private_file_handle(&file).is_err() {
        restrict_private_file_handle(&file)?;
    }
    Ok(file)
}

fn private_lock_file_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true).truncate(false);
    #[cfg(unix)]
    options
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    #[cfg(windows)]
    options
        .access_mode(FILE_GENERIC_WRITE | READ_CONTROL | WRITE_DAC)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    options
}

#[derive(Debug)]
struct AnchoredFileHandle {
    file: File,
    len: usize,
}

impl AnchoredFileHandle {
    fn new(file: File) -> io::Result<Self> {
        let len = usize::try_from(file.metadata()?.len())
            .map_err(|_| io::Error::other("anchored generation file is too large"))?;
        Ok(Self { file, len })
    }
}

impl HasLen for AnchoredFileHandle {
    fn len(&self) -> usize {
        self.len
    }
}

impl FileHandle for AnchoredFileHandle {
    fn read_bytes(
        &self,
        range: std::ops::Range<usize>,
    ) -> io::Result<tantivy::directory::OwnedBytes> {
        if range.start > range.end || range.end > self.len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "anchored generation read is outside the file",
            ));
        }
        let mut bytes = vec![0_u8; range.len()];
        #[cfg(unix)]
        std::os::unix::fs::FileExt::read_exact_at(&self.file, &mut bytes, range.start as u64)?;
        #[cfg(windows)]
        {
            let mut read = 0_usize;
            while read < bytes.len() {
                let count = std::os::windows::fs::FileExt::seek_read(
                    &self.file,
                    &mut bytes[read..],
                    (range.start + read) as u64,
                )?;
                if count == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "anchored generation read",
                    ));
                }
                read += count;
            }
        }
        Ok(tantivy::directory::OwnedBytes::new(bytes))
    }
}

fn read_only_directory_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "anchored generation directories are read-only",
    )
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

#[cfg(test)]
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
    match atomic_replace_with_outcome_validated(target_path, data, replace, sync_parent, || Ok(()))
    {
        Ok(outcome) => Ok(outcome),
        Err(crate::GenerationError::Io(error)) => Err(error),
        Err(error) => Err(io::Error::other(error)),
    }
}

fn prepare_atomic_write(target_path: &Path, data: &[u8]) -> crate::Result<PathBuf> {
    atomic_write_checkpoint(AtomicWriteStage::BeforeTemporaryWrite, target_path)?;
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
    drop(temporary_file);
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temporary_path);
        return Err(error.into());
    }
    Ok(temporary_path)
}

fn failed_atomic_replacement(
    temporary_path: &Path,
    _target_path: &Path,
    error: io::Error,
) -> crate::GenerationError {
    #[cfg(any(test, feature = "test-support"))]
    let failure_probe = publication_io_observer_active()
        .then(|| publication_failure_probe::capture(temporary_path, _target_path, &error));
    let cleanup_result = fs::remove_file(temporary_path);
    #[cfg(any(test, feature = "test-support"))]
    if let Some(mut probe) = failure_probe {
        probe.source_cleanup = publication_failure_probe::io_result(&cleanup_result);
        let _ = publication_io_checkpoint(PublicationIoEvent::AtomicReplacementFailure(probe));
    }
    let _ = cleanup_result;
    error.into()
}

fn finish_atomic_write<SyncParent>(
    target_path: &Path,
    sync_parent: SyncParent,
) -> DurableAtomicWriteOutcome
where
    SyncParent: FnOnce() -> io::Result<()>,
{
    if let Err(error) = atomic_write_checkpoint(
        AtomicWriteStage::AfterReplaceBeforeDirectorySync,
        target_path,
    ) {
        return DurableAtomicWriteOutcome::VisibleButDurabilityUncertain(error);
    }
    match sync_parent() {
        Ok(()) => DurableAtomicWriteOutcome::Durable,
        Err(error) => DurableAtomicWriteOutcome::VisibleButDurabilityUncertain(error),
    }
}

fn atomic_replace_with_outcome_validated<Replace, SyncParent, Validate>(
    target_path: &Path,
    data: &[u8],
    replace: Replace,
    sync_parent: SyncParent,
    validate_before_replace: Validate,
) -> crate::Result<DurableAtomicWriteOutcome>
where
    Replace: FnOnce(&Path, &Path) -> io::Result<()>,
    SyncParent: FnOnce() -> io::Result<()>,
    Validate: FnOnce() -> crate::Result<()>,
{
    let temporary_path = prepare_atomic_write(target_path, data)?;
    atomic_write_checkpoint(
        AtomicWriteStage::AfterTemporarySyncBeforeReplace,
        target_path,
    )?;

    // The validator can reject a raced candidate while the previous target is
    // still authoritative. Once it succeeds, the replacement checkpoint is
    // the final fallible test fence before replacement.
    if let Err(error) = validate_before_replace() {
        let _ = fs::remove_file(&temporary_path);
        return Err(error);
    }
    atomic_write_checkpoint(AtomicWriteStage::BeforeReplace, target_path)?;

    if let Err(error) = replace(&temporary_path, target_path) {
        return Err(failed_atomic_replacement(
            &temporary_path,
            target_path,
            error,
        ));
    }
    Ok(finish_atomic_write(target_path, sync_parent))
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtomicWriteStage {
    BeforeTemporaryWrite,
    AfterTemporarySyncBeforeReplace,
    BeforeReplace,
    AfterReplaceBeforeDirectorySync,
}

#[cfg(not(any(test, feature = "test-support")))]
#[derive(Debug, Clone, Copy)]
enum AtomicWriteStage {
    BeforeTemporaryWrite,
    AfterTemporarySyncBeforeReplace,
    BeforeReplace,
    AfterReplaceBeforeDirectorySync,
}

#[cfg(any(test, feature = "test-support"))]
pub struct AtomicWriteTestHookGuard {
    _guard: PublicationIoProbeGuard,
}

#[cfg(any(test, feature = "test-support"))]
impl AtomicWriteTestHookGuard {
    pub fn set<F>(mut hook: F) -> Self
    where
        F: for<'a> FnMut(AtomicWriteStage, &'a Path) -> io::Result<()> + 'static,
    {
        Self {
            _guard: PublicationIoProbeGuard::set_raw(move |event| match event {
                PublicationIoEvent::Atomic(stage, target) => hook(stage, target),
                PublicationIoEvent::CandidateGenerationSync => Ok(()),
                #[cfg(windows)]
                PublicationIoEvent::TerminalSealOpen => Ok(()),
                PublicationIoEvent::AtomicReplacementFailure(_) => Ok(()),
            }),
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
fn atomic_write_checkpoint(stage: AtomicWriteStage, target: &Path) -> io::Result<()> {
    publication_io_checkpoint(PublicationIoEvent::Atomic(stage, target))
}

#[cfg(not(any(test, feature = "test-support")))]
fn atomic_write_checkpoint(_stage: AtomicWriteStage, _target: &Path) -> io::Result<()> {
    Ok(())
}

fn create_temporary_file(parent_path: &Path) -> io::Result<(PathBuf, File)> {
    for _ in 0..TEMPORARY_FILE_ATTEMPTS {
        let temporary_path = parent_path.join(format!(
            "{TEMPORARY_FILE_PREFIX}{}.tmp",
            Uuid::new_v4().simple()
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        #[cfg(windows)]
        options.access_mode(FILE_GENERIC_WRITE | READ_CONTROL | WRITE_DAC);
        match options.open(&temporary_path) {
            Ok(file) => {
                if let Err(error) = restrict_private_file_handle(&file) {
                    drop(file);
                    let _ = fs::remove_file(&temporary_path);
                    return Err(error);
                }
                return Ok((temporary_path, file));
            }
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
    windows_replace::WindowsAtomicReplacement::prepare(source, target)?.replace()
}

#[cfg(test)]
mod publication_probe_tests;

#[cfg(test)]
mod publication_failure_probe_tests;

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    #[cfg(not(windows))]
    use std::{cell::RefCell, rc::Rc};

    use tempfile::tempdir;

    use super::*;

    #[cfg(unix)]
    #[test]
    fn lock_rejects_symlink_without_changing_its_target() {
        use std::os::unix::fs::{symlink, PermissionsExt as _};

        let temporary_directory = tempdir().unwrap();
        let target = temporary_directory.path().join("outside");
        fs::write(&target, b"preserve").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o664)).unwrap();
        let lock_path = temporary_directory.path().join("writer.lock");
        symlink(&target, &lock_path).unwrap();
        let directory = DurableMmapDirectory::open(temporary_directory.path()).unwrap();

        let error = match directory.acquire_lock(&Lock {
            filepath: PathBuf::from("writer.lock"),
            is_blocking: false,
        }) {
            Ok(_) => panic!("symlink lock unexpectedly opened"),
            Err(error) => error,
        };

        assert!(matches!(error, LockError::IoError(_)));
        assert_eq!(fs::read(&target).unwrap(), b"preserve");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o664
        );
    }

    #[test]
    fn private_lock_preserves_nonblocking_exclusion_and_releases_on_drop() {
        let temporary_directory = tempdir().unwrap();
        let directory = DurableMmapDirectory::open(temporary_directory.path()).unwrap();
        let lock = Lock {
            filepath: PathBuf::from("writer.lock"),
            is_blocking: false,
        };

        let held = directory.acquire_lock(&lock).unwrap();
        assert!(matches!(
            directory.acquire_lock(&lock),
            Err(LockError::LockBusy)
        ));
        drop(held);
        directory.acquire_lock(&lock).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn private_lock_preserves_ctime_when_existing_file_is_already_private() {
        use std::{
            os::unix::fs::{MetadataExt as _, PermissionsExt as _},
            time::Duration,
        };

        let temporary_directory = tempdir().unwrap();
        let lock_path = temporary_directory.path().join("writer.lock");
        fs::write(&lock_path, b"").unwrap();
        fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o600)).unwrap();
        let before = fs::metadata(&lock_path).unwrap();
        std::thread::sleep(Duration::from_millis(1_100));

        let directory = DurableMmapDirectory::open(temporary_directory.path()).unwrap();
        let _held = directory
            .acquire_lock(&Lock {
                filepath: PathBuf::from("writer.lock"),
                is_blocking: false,
            })
            .unwrap();

        let after = fs::metadata(&lock_path).unwrap();
        assert_eq!(
            (after.ctime(), after.ctime_nsec()),
            (before.ctime(), before.ctime_nsec())
        );
        assert_eq!(after.permissions().mode() & 0o777, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn private_lock_repairs_permissive_existing_file() {
        use std::os::unix::fs::PermissionsExt as _;

        let temporary_directory = tempdir().unwrap();
        let lock_path = temporary_directory.path().join("writer.lock");
        fs::write(&lock_path, b"").unwrap();
        fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o664)).unwrap();

        let directory = DurableMmapDirectory::open(temporary_directory.path()).unwrap();
        let _held = directory
            .acquire_lock(&Lock {
                filepath: PathBuf::from("writer.lock"),
                is_blocking: false,
            })
            .unwrap();

        assert_eq!(
            fs::metadata(lock_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

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

    #[cfg(not(windows))]
    #[test]
    fn validated_atomic_write_runs_validator_before_before_replace_checkpoint() {
        let temporary_directory = tempdir().unwrap();
        let directory = DurableMmapDirectory::open(temporary_directory.path()).unwrap();
        let observed = Rc::new(RefCell::new(Vec::new()));
        let observed_by_hook = Rc::clone(&observed);
        let hook = AtomicWriteTestHookGuard::set(move |stage, _path| {
            if stage == AtomicWriteStage::BeforeReplace {
                observed_by_hook.borrow_mut().push("before-replace");
            }
            Ok(())
        });

        directory
            .atomic_write_with_outcome_validated(Path::new("meta.json"), b"candidate", || {
                observed.borrow_mut().push("validator");
                Ok(())
            })
            .unwrap();
        drop(hook);

        assert_eq!(*observed.borrow(), ["validator", "before-replace"]);
    }

    #[test]
    fn durable_staged_file_replacement_supports_first_publish_and_replace() {
        let temporary_directory = tempdir().unwrap();
        let target = temporary_directory.path().join("projection.sqlite");
        let first = temporary_directory.path().join("projection.first");
        let mut first_file = File::create(&first).unwrap();
        first_file.write_all(b"first").unwrap();
        first_file.sync_all().unwrap();
        drop(first_file);

        durable_atomic_replace_file(&first, &target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"first");
        assert!(!first.exists());

        let replacement = temporary_directory.path().join("projection.replacement");
        let mut replacement_file = File::create(&replacement).unwrap();
        replacement_file.write_all(b"replacement").unwrap();
        replacement_file.sync_all().unwrap();
        drop(replacement_file);

        durable_atomic_replace_file(&replacement, &target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"replacement");
        assert!(!replacement.exists());
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
