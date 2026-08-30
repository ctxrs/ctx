use std::{
    fs::{File, OpenOptions},
    io,
    path::Path,
};

use ctx_history_platform::platform_security::create_private_file_new;

use super::{io_error, transaction_lock_path, FlatResult};

/// Coordinates one semantic control/flat snapshot with source-backed writers.
/// The lock file is part of every initialized flat store; passive callers open
/// it read-only and therefore cannot create storage artifacts. New locks are
/// private at creation, while legacy regular locks remain compatible because
/// coordination authority comes from the retained no-follow handle and OS
/// lock, not from changing or reinterpreting their owner or DACL.
pub(crate) struct FlatStoreCoordinationGuard {
    lock: FileLock,
}

impl FlatStoreCoordinationGuard {
    pub(crate) fn lock_passive_snapshot(root: &Path) -> FlatResult<Self> {
        Ok(Self {
            lock: FileLock::try_shared_passive(&transaction_lock_path(root))?,
        })
    }

    pub(crate) fn lock_control_writer(root: &Path) -> FlatResult<Self> {
        Ok(Self {
            lock: FileLock::exclusive(&transaction_lock_path(root))?,
        })
    }

    pub(crate) fn validate_retained(&self) -> FlatResult<()> {
        self.lock.validate_retained()
    }
}

pub(super) struct FileLock {
    file: File,
    path: std::path::PathBuf,
}

impl FileLock {
    pub(super) fn shared(path: &Path) -> FlatResult<Self> {
        let file = open_lock(path, false)?;
        fs2::FileExt::lock_shared(&file).map_err(|source| io_error("lock shared", path, source))?;
        let lock = Self {
            file,
            path: path.to_path_buf(),
        };
        lock.validate_retained()?;
        Ok(lock)
    }

    fn try_shared_passive(path: &Path) -> FlatResult<Self> {
        let file = open_lock(path, false)?;
        fs2::FileExt::try_lock_shared(&file)
            .map_err(|source| io_error("try lock passive shared", path, source))?;
        let lock = Self {
            file,
            path: path.to_path_buf(),
        };
        lock.validate_retained()?;
        Ok(lock)
    }

    pub(super) fn exclusive(path: &Path) -> FlatResult<Self> {
        let file = open_lock(path, true)?;
        fs2::FileExt::lock_exclusive(&file)
            .map_err(|source| io_error("lock exclusive", path, source))?;
        let lock = Self {
            file,
            path: path.to_path_buf(),
        };
        lock.validate_retained()?;
        Ok(lock)
    }

    fn validate_retained(&self) -> FlatResult<()> {
        validate_lock_file(&self.path, &self.file)
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

pub(super) fn open_lock(path: &Path, create: bool) -> FlatResult<File> {
    open_lock_after_missing(path, create, || {})
}

fn open_lock_after_missing(
    path: &Path,
    create: bool,
    after_missing: impl FnOnce(),
) -> FlatResult<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    if create {
        options.write(true);
    }
    configure_nofollow_open(&mut options);
    let file = match options.open(path) {
        Ok(file) => file,
        Err(source) if create && source.kind() == io::ErrorKind::NotFound => {
            after_missing();
            match create_private_file_new(path) {
                Ok(file) => file,
                Err(source) if source.kind() == io::ErrorKind::AlreadyExists => options
                    .open(path)
                    .map_err(|source| io_error("open raced flat writer lock", path, source))?,
                Err(source) => return Err(io_error("create flat writer lock", path, source)),
            }
        }
        Err(source) => return Err(io_error("open flat writer lock", path, source)),
    };
    validate_lock_file(path, &file)?;
    Ok(file)
}

#[cfg(unix)]
fn configure_nofollow_open(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt as _;

    options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
}

#[cfg(windows)]
fn configure_nofollow_open(options: &mut OpenOptions) {
    use std::os::windows::fs::OpenOptionsExt as _;

    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;

    // Omitting delete sharing keeps the retained lock name bound on Windows.
    options
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
}

#[cfg(not(any(unix, windows)))]
fn configure_nofollow_open(_options: &mut OpenOptions) {}

fn validate_lock_file(path: &Path, file: &File) -> FlatResult<()> {
    let opened = file
        .metadata()
        .map_err(|source| io_error("inspect retained flat lock", path, source))?;
    let named = std::fs::symlink_metadata(path)
        .map_err(|source| io_error("inspect named flat lock", path, source))?;
    if !opened.is_file() || named.file_type().is_symlink() || !named.is_file() {
        return Err(super::FlatStoreError::Corrupt(format!(
            "{} is not a retained regular lock file",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;

        if opened.dev() != named.dev() || opened.ino() != named.ino() {
            return Err(super::FlatStoreError::Corrupt(format!(
                "{} was replaced after its lock handle was opened",
                path.display()
            )));
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;

        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        if opened.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || named.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        {
            return Err(super::FlatStoreError::Corrupt(format!(
                "{} is a reparse-point lock",
                path.display()
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passive_lock_accepts_a_legacy_regular_coordination_file() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("legacy.lock");
        std::fs::write(&path, b"legacy").unwrap();

        let lock = FileLock::try_shared_passive(&path).unwrap();

        lock.validate_retained().unwrap();
    }

    #[test]
    fn writer_create_race_reopens_the_winning_regular_lock() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("raced.lock");

        let file = open_lock_after_missing(&path, true, || {
            std::fs::write(&path, b"winner").unwrap();
        })
        .unwrap();

        assert!(file.metadata().unwrap().is_file());
        assert_eq!(std::fs::read(path).unwrap(), b"winner");
    }
}
