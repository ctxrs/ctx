//! Writer exclusion and owner-private file helpers.

use ctx_history_platform::platform_security;
use fs2::FileExt;
#[cfg(any(test, not(unix)))]
use std::fs::OpenOptions;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[cfg(all(test, unix))]
use std::os::unix::fs::OpenOptionsExt as _;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
#[cfg(target_os = "windows")]
use std::os::windows::fs::OpenOptionsExt as _;

use same_file::Handle;

use super::{MaterializationProgress, SegmentMaterializerError};

const MATERIALIZER_LOCK_FILE: &str = "attribution-materializer.lock";
const MATERIALIZER_PROGRESS_FILE: &str = "attribution-progress.json";
const LOCK_WAIT: Duration = Duration::from_secs(2);

fn lock_is_contended(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::WouldBlock
        || error
            .raw_os_error()
            .is_some_and(|code| Some(code) == fs2::lock_contended_error().raw_os_error())
}

#[cfg(test)]
#[path = "locking/tests.rs"]
mod tests;

#[cfg(target_os = "windows")]
const NATIVE_DIRECTORY_SYNC_SUPPORTED: bool = false;

pub struct OperationLock {
    file: VerifiedFile,
    progress_file: Option<VerifiedFile>,
    progress: MaterializationProgress,
    started: Instant,
    last_reported: Option<Instant>,
}
impl OperationLock {
    pub fn acquire(root: &Path) -> Result<Self, SegmentMaterializerError> {
        Self::acquire_inner(root, None)
    }

    pub fn acquire_cancellable(
        root: &Path,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<Self, SegmentMaterializerError> {
        Self::acquire_inner(root, Some(cancelled))
    }

    fn acquire_inner(
        root: &Path,
        cancelled: Option<&(dyn Fn() -> bool + Sync)>,
    ) -> Result<Self, SegmentMaterializerError> {
        verify_private_root(root)?;
        let path = root.join(MATERIALIZER_LOCK_FILE);
        match platform_security::create_private_file_new(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(io_error(&path, error)),
        }
        let file = open_private_file(&path, true)?;
        let started = Instant::now();
        loop {
            if cancelled.is_some_and(|check| check()) {
                return Err(SegmentMaterializerError::Cancelled);
            }
            match file.file().try_lock_exclusive() {
                Ok(()) => break,
                Err(error) if lock_is_contended(&error) => {
                    if cancelled.is_none() && started.elapsed() >= LOCK_WAIT {
                        return Err(SegmentMaterializerError::Busy);
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => return Err(io_error(&path, error)),
            }
        }
        file.verify_identity()?;
        let progress_path = root.join(MATERIALIZER_PROGRESS_FILE);
        let progress_file = (|| {
            match platform_security::create_private_file_new(&progress_path) {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(io_error(&progress_path, error)),
            }
            open_private_file(&progress_path, true)
        })()
        .ok();
        let mut lock = Self {
            file,
            progress_file,
            progress: MaterializationProgress::default(),
            started: Instant::now(),
            last_reported: None,
        };
        lock.update_progress(|_| {}, true);
        Ok(lock)
    }
    pub fn verify_identity(&self) -> Result<(), SegmentMaterializerError> {
        self.file.verify_identity()
    }

    pub(super) fn progress(&self) -> MaterializationProgress {
        let mut progress = self.progress.clone();
        progress.elapsed_millis =
            u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        progress
    }

    /// Progress is an advisory sidecar, never part of committed index state.
    /// A partial or failed advisory write must not fail history or attribution work.
    pub(super) fn update_progress(
        &mut self,
        update: impl FnOnce(&mut MaterializationProgress),
        force: bool,
    ) {
        update(&mut self.progress);
        if !force
            && self
                .last_reported
                .is_some_and(|last| last.elapsed() < Duration::from_millis(500))
        {
            return;
        }
        self.last_reported = Some(Instant::now());
        self.progress.elapsed_millis =
            u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let Some(progress_file) = &self.progress_file else {
            return;
        };
        let _ = (|| -> io::Result<()> {
            let bytes = serde_json::to_vec(&self.progress)?;
            let mut file = progress_file.file();
            file.seek(SeekFrom::Start(0))?;
            file.set_len(0)?;
            file.write_all(&bytes)
        })();
    }

    /// Does not create files or wait for a writer. Stale bytes after a crash are ignored.
    pub(crate) fn read_progress(
        root: &Path,
    ) -> Result<Option<MaterializationProgress>, SegmentMaterializerError> {
        let path = root.join(MATERIALIZER_LOCK_FILE);
        let file = match open_private_file(&path, false) {
            Ok(file) => file,
            Err(SegmentMaterializerError::Io { source, .. })
                if source.kind() == io::ErrorKind::NotFound =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        match FileExt::try_lock_shared(file.file()) {
            Ok(()) => {
                FileExt::unlock(file.file()).map_err(|error| io_error(&path, error))?;
                Ok(None)
            }
            Err(error) if lock_is_contended(&error) => {
                // The writer can be between truncate and write. Its lock still proves
                // activity, even when this read has no complete counter snapshot.
                let snapshot = (|| -> Result<MaterializationProgress, SegmentMaterializerError> {
                    let progress_path = root.join(MATERIALIZER_PROGRESS_FILE);
                    let progress = open_private_file(&progress_path, false)?;
                    let mut bytes = Vec::new();
                    progress
                        .file()
                        .take(4096)
                        .read_to_end(&mut bytes)
                        .map_err(|error| io_error(&progress_path, error))?;
                    serde_json::from_slice(&bytes).map_err(|_| SegmentMaterializerError::Encoding)
                })();
                Ok(Some(snapshot.unwrap_or_else(|_| MaterializationProgress {
                    phase: super::MaterializationPhase::SnapshotUnavailable,
                    ..Default::default()
                })))
            }
            Err(error) => Err(io_error(&path, error)),
        }
    }
}

/// Owner-private, singly linked regular file pinned to one opened identity.
pub struct VerifiedFile {
    file: File,
    path: PathBuf,
    identity: Handle,
}

impl VerifiedFile {
    pub const fn file(&self) -> &File {
        &self.file
    }

    #[cfg(test)]
    pub const fn file_mut(&mut self) -> &mut File {
        &mut self.file
    }

    pub fn len(&self) -> Result<u64, SegmentMaterializerError> {
        self.file
            .metadata()
            .map(|metadata| metadata.len())
            .map_err(|source| io_error(&self.path, source))
    }

    pub fn verify_identity(&self) -> Result<(), SegmentMaterializerError> {
        if private_file_identity(&self.path, &self.file)? == self.identity {
            Ok(())
        } else {
            Err(SegmentMaterializerError::Corrupt(
                "private file identity changed while open",
            ))
        }
    }
}

pub fn prepare_private_root(root: &Path) -> Result<(), SegmentMaterializerError> {
    let created = match fs::create_dir(root) {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => false,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(root).map_err(|source| io_error(root, source))?;
            true
        }
        Err(source) => return Err(io_error(root, source)),
    };
    if created {
        #[cfg(unix)]
        fs::set_permissions(root, fs::Permissions::from_mode(0o700))
            .map_err(|source| io_error(root, source))?;
        #[cfg(target_os = "windows")]
        platform_security::restrict_private_directory(root)
            .map_err(|source| io_error(root, source))?;
    }
    verify_private_root(root)
}

pub fn verify_private_root(root: &Path) -> Result<(), SegmentMaterializerError> {
    let named = fs::symlink_metadata(root).map_err(|source| io_error(root, source))?;
    if !named.file_type().is_dir() {
        return Err(SegmentMaterializerError::Corrupt(
            "materializer root is not a direct directory",
        ));
    }
    #[cfg(unix)]
    if named.uid() != effective_user_id() || named.mode() & 0o077 != 0 {
        return Err(SegmentMaterializerError::Corrupt(
            "materializer root is not owner-private",
        ));
    }
    #[cfg(target_os = "windows")]
    platform_security::verify_private_directory(root).map_err(|source| io_error(root, source))?;
    let file = open_directory_nofollow(root).map_err(|source| io_error(root, source))?;
    let opened = Handle::from_file(file.try_clone().map_err(|source| io_error(root, source))?)
        .map_err(|source| io_error(root, source))?;
    let first = Handle::from_path(root).map_err(|source| io_error(root, source))?;
    let final_metadata = fs::symlink_metadata(root).map_err(|source| io_error(root, source))?;
    let final_named = Handle::from_path(root).map_err(|source| io_error(root, source))?;
    if opened != first || opened != final_named || !final_metadata.file_type().is_dir() {
        return Err(SegmentMaterializerError::Corrupt(
            "materializer root identity changed during validation",
        ));
    }
    #[cfg(unix)]
    if final_metadata.uid() != effective_user_id() || final_metadata.mode() & 0o077 != 0 {
        return Err(SegmentMaterializerError::Corrupt(
            "materializer root security changed during validation",
        ));
    }
    Ok(())
}

pub fn sync_private_root(root: &Path) -> Result<(), SegmentMaterializerError> {
    verify_private_root(root)?;
    #[cfg(unix)]
    {
        let directory = open_directory_nofollow(root).map_err(|source| io_error(root, source))?;
        directory
            .sync_all()
            .map_err(|source| io_error(root, source))
    }
    #[cfg(not(unix))]
    {
        Ok(())
    }
}

#[cfg(test)]
pub fn create_private_file(path: &Path) -> Result<VerifiedFile, SegmentMaterializerError> {
    verify_private_parent(path)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    #[cfg(target_os = "windows")]
    options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    let file = options
        .open(path)
        .map_err(|source| io_error(path, source))?;
    #[cfg(unix)]
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|source| io_error(path, source))?;
    #[cfg(target_os = "windows")]
    platform_security::restrict_private_file_handle(&file)
        .map_err(|source| io_error(path, source))?;
    verified_file(path, file)
}

pub fn open_private_file(
    path: &Path,
    writable: bool,
) -> Result<VerifiedFile, SegmentMaterializerError> {
    verify_private_parent(path)?;
    let named = fs::symlink_metadata(path).map_err(|source| io_error(path, source))?;
    if !named.file_type().is_file() {
        return Err(SegmentMaterializerError::Corrupt(
            "materializer path is not a direct regular file",
        ));
    }
    let file = open_file_nofollow(path, writable).map_err(|source| io_error(path, source))?;
    #[cfg(target_os = "windows")]
    platform_security::verify_private_file_handle(&file)
        .map_err(|source| io_error(path, source))?;
    verified_file(path, file)
}

pub fn remove_private_file_if_exists(path: &Path) -> Result<(), SegmentMaterializerError> {
    let file = match open_private_file(path, false) {
        Ok(file) => file,
        Err(SegmentMaterializerError::Io { source, .. })
            if source.kind() == io::ErrorKind::NotFound =>
        {
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    file.verify_identity()?;
    fs::remove_file(path).map_err(|source| io_error(path, source))?;
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(SegmentMaterializerError::Corrupt(
            "materializer removal target was substituted",
        )),
        Err(source) => Err(io_error(path, source)),
    }
}

fn verify_private_parent(path: &Path) -> Result<(), SegmentMaterializerError> {
    let parent = path.parent().ok_or(SegmentMaterializerError::Corrupt(
        "materializer file has no parent directory",
    ))?;
    verify_private_root(parent)
}

fn verified_file(path: &Path, file: File) -> Result<VerifiedFile, SegmentMaterializerError> {
    let identity = private_file_identity(path, &file)?;
    Ok(VerifiedFile {
        file,
        path: path.to_owned(),
        identity,
    })
}

fn private_file_identity(path: &Path, file: &File) -> Result<Handle, SegmentMaterializerError> {
    let opened_metadata = file.metadata().map_err(|source| io_error(path, source))?;
    let named_metadata = fs::symlink_metadata(path).map_err(|source| io_error(path, source))?;
    if !opened_metadata.file_type().is_file() || !named_metadata.file_type().is_file() {
        return Err(SegmentMaterializerError::Corrupt(
            "materializer path is not a direct regular file",
        ));
    }
    #[cfg(unix)]
    if opened_metadata.uid() != effective_user_id()
        || named_metadata.uid() != effective_user_id()
        || opened_metadata.mode() & 0o7777 != 0o600
        || named_metadata.mode() & 0o7777 != 0o600
        || opened_metadata.nlink() != 1
        || named_metadata.nlink() != 1
    {
        return Err(SegmentMaterializerError::Corrupt(
            "materializer file is not owner-private and singly linked",
        ));
    }
    #[cfg(target_os = "windows")]
    if windows_file_identity::link_count(file).map_err(|source| io_error(path, source))? != 1 {
        return Err(SegmentMaterializerError::Corrupt(
            "materializer file is not singly linked",
        ));
    }
    let opened = Handle::from_file(file.try_clone().map_err(|source| io_error(path, source))?)
        .map_err(|source| io_error(path, source))?;
    let named = Handle::from_path(path).map_err(|source| io_error(path, source))?;
    let final_metadata = fs::symlink_metadata(path).map_err(|source| io_error(path, source))?;
    let final_named = Handle::from_path(path).map_err(|source| io_error(path, source))?;
    if opened != named || opened != final_named || !final_metadata.file_type().is_file() {
        return Err(SegmentMaterializerError::Corrupt(
            "materializer file identity changed during validation",
        ));
    }
    #[cfg(unix)]
    if final_metadata.uid() != effective_user_id()
        || final_metadata.mode() & 0o7777 != 0o600
        || final_metadata.nlink() != 1
    {
        return Err(SegmentMaterializerError::Corrupt(
            "materializer file security changed during validation",
        ));
    }
    Ok(opened)
}

#[cfg(unix)]
fn open_file_nofollow(path: &Path, writable: bool) -> io::Result<File> {
    let access = if writable {
        rustix::fs::OFlags::RDWR
    } else {
        rustix::fs::OFlags::RDONLY
    };
    rustix::fs::open(
        path,
        access | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NOFOLLOW,
        rustix::fs::Mode::empty(),
    )
    .map(File::from)
    .map_err(Into::into)
}

#[cfg(target_os = "windows")]
fn open_file_nofollow(path: &Path, writable: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(writable)
        .custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(not(any(unix, target_os = "windows")))]
fn open_file_nofollow(path: &Path, writable: bool) -> io::Result<File> {
    OpenOptions::new().read(true).write(writable).open(path)
}

#[cfg(unix)]
fn open_directory_nofollow(path: &Path) -> io::Result<File> {
    rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::DIRECTORY,
        rustix::fs::Mode::empty(),
    )
    .map(File::from)
    .map_err(Into::into)
}

#[cfg(target_os = "windows")]
fn open_directory_nofollow(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(
            windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS
                | windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT,
        )
        .open(path)
}

#[cfg(not(any(unix, target_os = "windows")))]
fn open_directory_nofollow(path: &Path) -> io::Result<File> {
    File::open(path)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn effective_user_id() -> u32 {
    rustix::process::geteuid().as_raw()
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
#[allow(unsafe_code)]
fn effective_user_id() -> u32 {
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    // SAFETY: `geteuid` takes no arguments and returns the effective UID.
    unsafe { geteuid() }
}

fn io_error(path: &Path, source: io::Error) -> SegmentMaterializerError {
    SegmentMaterializerError::Io {
        operation: "access private materializer storage",
        path: path.to_owned(),
        source,
    }
}

#[cfg(target_os = "windows")]
mod windows_file_identity {
    #![allow(unsafe_code)]

    use std::fs::File;
    use std::io;
    use std::os::windows::io::AsRawHandle as _;

    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };

    pub fn link_count(file: &File) -> io::Result<u32> {
        // SAFETY: Win32 initializes every field before this value is read.
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &raw mut info) } == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(info.nNumberOfLinks)
        }
    }
}
