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
    #[cfg(windows)]
    root_authority: Option<PassiveRootAuthority>,
}

impl FlatStoreCoordinationGuard {
    pub(crate) fn lock_passive_snapshot(root: &Path) -> FlatResult<Self> {
        #[cfg(windows)]
        return Self::lock_passive_snapshot_windows(root, || {}, || {});
        #[cfg(not(windows))]
        Ok(Self {
            lock: FileLock::try_shared_passive(&transaction_lock_path(root))?,
        })
    }

    #[cfg(windows)]
    fn lock_passive_snapshot_windows(
        root: &Path,
        lock_admitted: impl FnOnce(),
        root_authority_admitted: impl FnOnce(),
    ) -> FlatResult<Self> {
        // The shared transaction lock is the admission gate.  In particular,
        // do not retain root authority until that admission succeeds: a writer
        // which already holds the exclusive lock must remain able to publish.
        let lock = FileLock::try_shared_passive(&transaction_lock_path(root))?;
        lock_admitted();

        // Once admitted, the no-delete lock handle pins the lock's root path
        // while this stricter directory authority is acquired before callers
        // can open SQLite or Flat children.
        let root_authority = PassiveRootAuthority::open(root)?;
        root_authority_admitted();
        Ok(Self {
            lock,
            root_authority: Some(root_authority),
        })
    }

    #[cfg(all(test, windows))]
    fn lock_passive_snapshot_with_admission_hooks(
        root: &Path,
        lock_admitted: impl FnOnce(),
        root_authority_admitted: impl FnOnce(),
    ) -> FlatResult<Self> {
        Self::lock_passive_snapshot_windows(root, lock_admitted, root_authority_admitted)
    }

    pub(crate) fn lock_control_writer(root: &Path) -> FlatResult<Self> {
        Ok(Self {
            lock: FileLock::exclusive(&transaction_lock_path(root))?,
            #[cfg(windows)]
            root_authority: None,
        })
    }

    pub(crate) fn validate_retained(&self) -> FlatResult<()> {
        #[cfg(windows)]
        if let Some(root_authority) = &self.root_authority {
            root_authority.validate_retained()?;
        }
        self.lock.validate_retained()
    }
}

#[cfg(windows)]
impl Drop for FlatStoreCoordinationGuard {
    fn drop(&mut self) {
        // This is deliberately not left to field-drop order.  A newly admitted
        // writer may publish as soon as the transaction lock is released, so
        // release the restrictive root share before that can happen.
        drop(self.root_authority.take());
        self.lock.unlock();
    }
}

/// Retains the absolute Windows semantic root with generic-read authority and
/// read sharing, but without write or delete sharing. The admitted directory
/// therefore cannot be mutated, renamed, replaced, or converted to a reparse
/// point while passive children are open.
#[cfg(windows)]
struct PassiveRootAuthority {
    path: std::path::PathBuf,
    handle: File,
}

#[cfg(windows)]
impl PassiveRootAuthority {
    fn open(root: &Path) -> FlatResult<Self> {
        use std::os::windows::fs::OpenOptionsExt as _;

        const FILE_SHARE_READ: u32 = 0x0000_0001;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        const FILE_GENERIC_READ: u32 = 0x0012_0089;

        let path = std::path::absolute(root)
            .map_err(|source| io_error("resolve passive root authority", root, source))?;
        let handle = OpenOptions::new()
            .access_mode(FILE_GENERIC_READ)
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&path)
            .map_err(|source| io_error("open passive root authority", &path, source))?;
        validate_retained_directory(&path, &handle)?;
        Ok(Self { path, handle })
    }

    fn validate_retained(&self) -> FlatResult<()> {
        validate_retained_directory(&self.path, &self.handle)
    }
}

#[cfg(windows)]
fn validate_retained_directory(path: &Path, handle: &File) -> FlatResult<()> {
    use std::os::windows::fs::MetadataExt as _;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    let metadata = handle
        .metadata()
        .map_err(|source| io_error("inspect passive root authority", path, source))?;
    if metadata.is_dir() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
        Ok(())
    } else {
        Err(super::FlatStoreError::Corrupt(format!(
            "{} is not a retained non-reparse semantic root component",
            path.display()
        )))
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

    fn unlock(&self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        self.unlock();
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

    #[cfg(windows)]
    #[test]
    fn passive_transaction_lock_precedes_root_authority() {
        let temporary = tempfile::tempdir().unwrap();
        let parent = temporary.path().join("search");
        let root = parent.join("semantic");
        let displaced_root = parent.join("semantic-displaced");
        let displaced_parent = temporary.path().join("search-displaced");
        let lock = transaction_lock_path(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&lock, b"").unwrap();
        let mut attempts = None;

        let guard = FlatStoreCoordinationGuard::lock_passive_snapshot_with_admission_hooks(
            &root,
            || {
                attempts = Some((
                    std::fs::rename(&lock, root.join("flat_transaction.lock.displaced")),
                    std::fs::rename(&root, &displaced_root),
                    std::fs::rename(&parent, &displaced_parent),
                ));
            },
            || {},
        )
        .unwrap();
        let (lock_replacement, root_replacement, parent_replacement) =
            attempts.expect("root replacement must be attempted in the admission hook");

        assert!(
            lock_replacement.is_err(),
            "the admission hook must run after the no-delete child lock is retained"
        );
        assert!(
            root_replacement.is_err(),
            "the admitted transaction lock must deny direct-root replacement"
        );
        assert!(
            parent_replacement.is_err(),
            "the admitted transaction lock must pin the admitted ancestor path"
        );
        guard.validate_retained().unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn passive_root_authority_pins_its_path_without_a_child_lock_handle() {
        let temporary = tempfile::tempdir().unwrap();
        let parent = temporary.path().join("search");
        let root = parent.join("semantic");
        let displaced_root = parent.join("semantic-displaced");
        let displaced_parent = temporary.path().join("search-displaced");
        let lock = transaction_lock_path(&root);
        let displaced_lock = root.join("flat_transaction.lock.displaced");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&lock, b"").unwrap();

        let authority = PassiveRootAuthority::open(&root).unwrap();
        std::fs::rename(&lock, &displaced_lock)
            .expect("no child lock handle may contribute replacement authority");
        std::fs::remove_file(&displaced_lock)
            .expect("root authority alone must permit child deletion");
        assert!(
            std::fs::rename(&root, &displaced_root).is_err(),
            "retained root authority must deny direct-root replacement"
        );
        assert!(
            std::fs::rename(&parent, &displaced_parent).is_err(),
            "retained root authority must pin the admitted ancestor path"
        );
        authority.validate_retained().unwrap();
        drop(authority);

        std::fs::rename(&root, &displaced_root)
            .expect("dropping root authority must release the root path");
        std::fs::rename(&displaced_root, &root).unwrap();
        std::fs::rename(&parent, &displaced_parent)
            .expect("dropping root authority must release the ancestor path");
    }

    #[cfg(windows)]
    #[test]
    fn writer_publication_survives_overlapping_passive_admission() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("semantic");
        super::super::ensure_store_directories(&root).unwrap();
        let writer = FlatStoreCoordinationGuard::lock_control_writer(&root).unwrap();
        let segments = super::super::segments_directory(&root);
        let staged = segments.join(".artifact.staged");
        let published = segments.join("artifact.bin");
        std::fs::write(&staged, b"published while passive admission overlaps").unwrap();
        let passive_root = root.clone();

        let passive = std::thread::spawn(move || {
            FlatStoreCoordinationGuard::lock_passive_snapshot_with_admission_hooks(
                &passive_root,
                || panic!("writer-first admission must fail before root authority is opened"),
                || {},
            )
        });
        let publication = super::super::commit_unique_file(&staged, &published);

        publication.expect("live passive root authority must permit child publication");
        let passive = passive.join().expect("join passive admission");
        assert!(
            passive.is_err(),
            "writer-first exclusive coordination must make passive admission unavailable"
        );
        writer.validate_retained().unwrap();
        assert_eq!(
            std::fs::read(published).unwrap(),
            b"published while passive admission overlaps"
        );
    }

    #[cfg(windows)]
    #[test]
    fn passive_first_handoff_releases_root_authority_before_writer_admission() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("semantic");
        super::super::ensure_store_directories(&root).unwrap();
        let staged = super::super::segments_directory(&root).join(".artifact.staged");
        let published = super::super::segments_directory(&root).join("artifact.bin");
        std::fs::write(&staged, b"published after passive teardown").unwrap();

        let passive = FlatStoreCoordinationGuard::lock_passive_snapshot(&root).unwrap();
        let writer_lock = transaction_lock_path(&root);
        let writer_file = open_lock(&writer_lock, true).unwrap();
        assert!(
            fs2::FileExt::try_lock_exclusive(&writer_file).is_err(),
            "a passive shared admission must exclude a writer"
        );
        drop(passive);

        fs2::FileExt::try_lock_exclusive(&writer_file)
            .expect("passive teardown must release shared admission after root authority");
        super::super::commit_unique_file(&staged, &published)
            .expect("writer publication must proceed once passive authority is gone");
        fs2::FileExt::unlock(&writer_file).unwrap();
        assert_eq!(
            std::fs::read(published).unwrap(),
            b"published after passive teardown"
        );
    }
}
