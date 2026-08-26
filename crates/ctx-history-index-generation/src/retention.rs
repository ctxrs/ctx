use std::{
    fs::{self, File, OpenOptions},
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt as _;
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt as _;

#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_WRITE, READ_CONTROL, WRITE_DAC,
};

use ctx_history_platform::platform_security::{
    restrict_private_file_handle, verify_private_file, verify_private_file_handle,
};
use serde::{Deserialize, Serialize};
use tantivy::directory::Lock;

use crate::{
    acquire_generation_writer_lock_with_retry, durable_atomic_replace_file, is_generation_id,
    load_active_generation_pointer, slot_path, sync_directory, DurableMmapDirectory,
    GenerationError as IndexError, GenerationReadRoot, GenerationSlot, Result,
    GENERATION_WRITER_LOCK_FILE, INDEX_GENERATIONS_DIRECTORY,
};

mod read_lease;

pub(crate) use read_lease::{
    acquire_existing_generation_directory_read_authority, ensure_generation_read_lease_coordinator,
    try_generation_directory_reclaim_authority, try_generation_id_reclaim_authority,
    ExistingGenerationDirectoryReadAuthority,
};
pub use read_lease::{
    acquire_generation_read_lease, acquire_generation_read_lease_from_root,
    acquire_retained_generation_read_lease, acquire_retained_generation_read_lease_from_root,
    GenerationReadLease,
};

const GENERATION_RETENTION_LEASE_FILE: &str = "generation-retention-lease.json";
const GENERATION_RETENTION_LEASE_STAGED_FILE: &str = ".generation-retention-lease.next";
const GENERATION_RETENTION_LEASE_VERSION: u16 = 1;
const MAX_GENERATION_RETENTION_LEASE_BYTES: u64 = 4 * 1024;
const MAX_GENERATION_RETENTION_OWNER_KIND_BYTES: usize = 64;
/// The sole bounded durable hold on an immutable generation outside the
/// active/previous publication pair.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationRetentionLease {
    version: u16,
    owner_kind: String,
    owner_id: String,
    target: GenerationSlot,
}

impl GenerationRetentionLease {
    pub fn owner_kind(&self) -> &str {
        &self.owner_kind
    }

    pub fn owner_id(&self) -> &str {
        &self.owner_id
    }

    pub fn generation_id(&self) -> &str {
        self.target.generation_id()
    }

    #[doc(hidden)]
    pub fn target(&self) -> &GenerationSlot {
        &self.target
    }

    fn validate(&self) -> Result<()> {
        if self.version != GENERATION_RETENTION_LEASE_VERSION {
            return Err(IndexError::UnsupportedGenerationRetentionLease(u32::from(
                self.version,
            )));
        }
        if self.owner_kind.is_empty()
            || self.owner_kind.len() > MAX_GENERATION_RETENTION_OWNER_KIND_BYTES
            || !self
                .owner_kind
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
            || !is_generation_id(&self.owner_id)
        {
            return Err(IndexError::InvalidGenerationRetentionLeaseOwner);
        }
        self.target.validate()
    }
}

/// Atomically acquires the one durable lease while serialized with Core
/// publication and reclamation. Exact replay by the same owner is idempotent.
pub fn acquire_generation_retention_lease(
    root: impl AsRef<Path>,
    generation_id: &str,
    owner_kind: &str,
    owner_id: &str,
) -> Result<GenerationRetentionLease> {
    if !is_generation_id(generation_id) {
        return Err(IndexError::InvalidGenerationId);
    }
    let root = canonical_index_root(root.as_ref())?;
    let directory = DurableMmapDirectory::open(&root).map_err(tantivy::TantivyError::from)?;
    let lock = Lock {
        filepath: PathBuf::from(GENERATION_WRITER_LOCK_FILE),
        is_blocking: false,
    };
    let _publication_lock = acquire_generation_writer_lock_with_retry(&directory, &lock)?;

    if let Some(existing) = load_generation_retention_lease(&root)? {
        if existing.generation_id() == generation_id
            && existing.owner_kind == owner_kind
            && existing.owner_id == owner_id
        {
            return Ok(existing);
        }
        return Err(IndexError::GenerationRetentionLeaseConflict {
            retained_generation_id: existing.generation_id().to_owned(),
            owner_kind: existing.owner_kind,
        });
    }

    let pointer =
        load_active_generation_pointer(&root)?.ok_or(IndexError::MissingActiveGenerationPointer)?;
    let target = std::iter::once(pointer.active())
        .chain(pointer.previous())
        .find(|slot| slot.generation_id() == generation_id)
        .cloned()
        .ok_or_else(|| IndexError::GenerationRetentionLeaseTargetNotRetained {
            requested_generation_id: generation_id.to_owned(),
        })?;
    let lease = GenerationRetentionLease {
        version: GENERATION_RETENTION_LEASE_VERSION,
        owner_kind: owner_kind.to_owned(),
        owner_id: owner_id.to_owned(),
        target,
    };
    lease.validate()?;
    publish_lease(&root, &lease)?;
    Ok(lease)
}

/// Loads and strictly validates the sole lease. Corrupt, oversized, or
/// non-private state is actionable and never broadens retention.
pub fn load_generation_retention_lease(
    root: impl AsRef<Path>,
) -> Result<Option<GenerationRetentionLease>> {
    let path = lease_path(root.as_ref());
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAX_GENERATION_RETENTION_LEASE_BYTES
        || verify_private_file(&path).is_err()
    {
        return Err(IndexError::InvalidGenerationRetentionLease);
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    File::open(&path)?
        .take(MAX_GENERATION_RETENTION_LEASE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_GENERATION_RETENTION_LEASE_BYTES {
        return Err(IndexError::InvalidGenerationRetentionLease);
    }
    let lease: GenerationRetentionLease =
        serde_json::from_slice(&bytes).map_err(|_| IndexError::InvalidGenerationRetentionLease)?;
    if serde_json::to_vec(&lease)? != bytes {
        return Err(IndexError::InvalidGenerationRetentionLease);
    }
    lease.validate()?;
    let target = slot_path(root.as_ref(), lease.target());
    let target_metadata =
        fs::symlink_metadata(target).map_err(|_| IndexError::InvalidGenerationRetentionLease)?;
    if !target_metadata.is_dir() || target_metadata.file_type().is_symlink() {
        return Err(IndexError::InvalidGenerationRetentionLease);
    }
    Ok(Some(lease))
}

fn load_generation_retention_lease_from_read_root(
    root: &GenerationReadRoot,
) -> Result<Option<GenerationRetentionLease>> {
    let file = match root.open_file(Path::new(GENERATION_RETENTION_LEASE_FILE)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.len() > MAX_GENERATION_RETENTION_LEASE_BYTES
        || verify_private_file_handle(&file).is_err()
    {
        return Err(IndexError::InvalidGenerationRetentionLease);
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.take(MAX_GENERATION_RETENTION_LEASE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_GENERATION_RETENTION_LEASE_BYTES {
        return Err(IndexError::InvalidGenerationRetentionLease);
    }
    let lease: GenerationRetentionLease =
        serde_json::from_slice(&bytes).map_err(|_| IndexError::InvalidGenerationRetentionLease)?;
    if serde_json::to_vec(&lease)? != bytes {
        return Err(IndexError::InvalidGenerationRetentionLease);
    }
    lease.validate()?;
    root.opened()
        .open_directory(&Path::new(INDEX_GENERATIONS_DIRECTORY).join(lease.target().directory()))
        .map_err(|_| IndexError::InvalidGenerationRetentionLease)?;
    Ok(Some(lease))
}

/// Releases exactly the observed owner under the publication lock. The next
/// writer open/publication performs ordinary bounded reclamation.
pub fn release_generation_retention_lease(
    root: impl AsRef<Path>,
    expected: &GenerationRetentionLease,
) -> Result<bool> {
    let root = match canonical_existing_index_root(root.as_ref())? {
        Some(root) => root,
        None => return Ok(false),
    };
    let directory = DurableMmapDirectory::open(&root).map_err(tantivy::TantivyError::from)?;
    let lock = Lock {
        filepath: PathBuf::from(GENERATION_WRITER_LOCK_FILE),
        is_blocking: false,
    };
    let _publication_lock = acquire_generation_writer_lock_with_retry(&directory, &lock)?;
    let Some(current) = load_generation_retention_lease(&root)? else {
        return Ok(false);
    };
    if &current != expected {
        return Err(IndexError::GenerationRetentionLeaseOwnerMismatch);
    }
    remove_lease_file(&root)?;
    Ok(true)
}

fn canonical_index_root(root: &Path) -> Result<PathBuf> {
    if !root.is_dir() {
        return Err(IndexError::MissingActiveGenerationPointer);
    }
    canonical_existing_index_root(root)?.ok_or(IndexError::MissingActiveGenerationPointer)
}

fn canonical_existing_index_root(root: &Path) -> Result<Option<PathBuf>> {
    match fs::canonicalize(root) {
        Ok(root) => Ok(Some(root)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn lease_path(root: &Path) -> PathBuf {
    root.join(GENERATION_RETENTION_LEASE_FILE)
}

fn publish_lease(root: &Path, lease: &GenerationRetentionLease) -> Result<()> {
    let bytes = serde_json::to_vec(lease)?;
    let staged = root.join(GENERATION_RETENTION_LEASE_STAGED_FILE);
    match fs::remove_file(&staged) {
        Ok(()) => sync_directory(root)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let mut file = create_private_lease_stage(&staged)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    let target = lease_path(root);
    if let Err(error) = durable_atomic_replace_file(&staged, &target) {
        let _ = fs::remove_file(&staged);
        return Err(error.into());
    }
    if verify_private_file(&target).is_err() {
        return Err(IndexError::InvalidGenerationRetentionLease);
    }
    Ok(())
}

fn create_private_lease_stage(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    #[cfg(windows)]
    options
        .access_mode(FILE_GENERIC_WRITE | READ_CONTROL | WRITE_DAC)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    let file = options.open(path)?;
    if let Err(error) = restrict_private_file_handle(&file) {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(error);
    }
    Ok(file)
}

fn remove_lease_file(root: &Path) -> Result<()> {
    match fs::remove_file(lease_path(root)) {
        Ok(()) => sync_directory(root).map_err(Into::into),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use std::{process::Command, thread, time::Duration};

    use ctx_history_platform::platform_security::restrict_private_file_handle;
    use tempfile::tempdir;

    use super::*;
    use crate::certification::certification_path;
    use crate::{
        manifest_path, publish_active_generation_pointer, reclaim_inactive_generation_directories,
        reclaim_unreferenced_certifications, reclaim_unreferenced_manifests, sha256_hex,
        write_manifest_bytes, ActiveGenerationPointer, INDEX_GENERATIONS_DIRECTORY,
    };

    const CHILD_ROOT: &str = "CTX_GENERATION_READ_LEASE_CHILD_ROOT";
    const CHILD_GENERATION: &str = "CTX_GENERATION_READ_LEASE_CHILD_GENERATION";
    const CHILD_MARKER: &str = "CTX_GENERATION_READ_LEASE_CHILD_MARKER";
    const CREATION_CRASH_ROOT: &str = "CTX_GENERATION_READ_LEASE_CREATION_CRASH_ROOT";
    #[cfg(unix)]
    const FORK_CHILD_ROOT: &str = "CTX_GENERATION_READ_LEASE_FORK_CHILD_ROOT";
    #[cfg(unix)]
    const FORK_CHILD_GENERATION: &str = "CTX_GENERATION_READ_LEASE_FORK_CHILD_GENERATION";
    #[cfg(unix)]
    const FORK_CHILD_READY: &str = "CTX_GENERATION_READ_LEASE_FORK_CHILD_READY";
    #[cfg(unix)]
    const FORK_CHILD_RELEASE: &str = "CTX_GENERATION_READ_LEASE_FORK_CHILD_RELEASE";
    #[cfg(unix)]
    const FORK_CHILD_RELEASED: &str = "CTX_GENERATION_READ_LEASE_FORK_CHILD_RELEASED";

    fn create_slot(root: &Path, digit: char) -> GenerationSlot {
        let bytes = format!(r#"{{"generation":"{digit}"}}"#).into_bytes();
        create_slot_with_bytes(root, digit, &bytes)
    }

    fn create_slot_with_bytes(root: &Path, digit: char, bytes: &[u8]) -> GenerationSlot {
        let generation_id = sha256_hex(bytes);
        let directory = format!("generation-{}", digit.to_string().repeat(32));
        fs::create_dir_all(root.join(INDEX_GENERATIONS_DIRECTORY).join(&directory)).unwrap();
        write_manifest_bytes(root, &generation_id, bytes).unwrap();
        let slot = GenerationSlot::new(
            generation_id,
            directory,
            sha256_hex(format!("physical-{digit}").as_bytes()),
        )
        .unwrap();
        let certification = certification_path(root, &slot);
        fs::create_dir_all(certification.parent().unwrap()).unwrap();
        fs::write(certification, b"test-certification").unwrap();
        slot
    }

    #[test]
    fn generation_read_lease_crash_child() {
        let (Ok(root), Ok(generation_id), Ok(marker)) = (
            std::env::var(CHILD_ROOT),
            std::env::var(CHILD_GENERATION),
            std::env::var(CHILD_MARKER),
        ) else {
            return;
        };
        let _lease = acquire_generation_read_lease(root, &generation_id).unwrap();
        fs::write(marker, b"ready").unwrap();
        loop {
            thread::sleep(Duration::from_millis(100));
        }
    }

    #[test]
    fn generation_read_lease_creation_crash_child() {
        let Ok(root) = std::env::var(CREATION_CRASH_ROOT) else {
            return;
        };
        ensure_generation_read_lease_coordinator(Path::new(&root)).unwrap();
        panic!("coordinator creation crash injection did not exit");
    }

    #[cfg(unix)]
    #[test]
    fn generation_read_lease_fork_parent() {
        let (Ok(root), Ok(generation_id), Ok(ready), Ok(release), Ok(released)) = (
            std::env::var(FORK_CHILD_ROOT),
            std::env::var(FORK_CHILD_GENERATION),
            std::env::var(FORK_CHILD_READY),
            std::env::var(FORK_CHILD_RELEASE),
            std::env::var(FORK_CHILD_RELEASED),
        ) else {
            return;
        };
        let inherited_lease = acquire_generation_read_lease(&root, &generation_id).unwrap();
        let fork_result = unsafe { libc::fork() };
        if fork_result < 0 {
            panic!("fork failed: {}", std::io::Error::last_os_error());
        }
        if fork_result > 0 {
            unsafe { libc::_exit(0) };
        }

        assert!(matches!(
            inherited_lease.with_root_access(|_| ()),
            Err(IndexError::InvalidGenerationRetentionLease)
        ));
        let child_lease = acquire_generation_read_lease(&root, &generation_id).unwrap();
        // This closes the inherited lease object after the child has acquired
        // its own locks. The fork recovery path must keep the old descriptor
        // pinned so this close cannot discard the child's new POSIX locks.
        drop(inherited_lease);
        fs::write(&ready, b"ready").unwrap();
        for _ in 0..1_000 {
            if Path::new(&release).is_file() {
                drop(child_lease);
                fs::write(released, b"released").unwrap();
                unsafe { libc::_exit(0) };
            }
            thread::sleep(Duration::from_millis(10));
        }
        unsafe { libc::_exit(87) };
    }

    #[cfg(unix)]
    fn wait_for_file(path: &Path, message: &str) {
        for _ in 0..500 {
            if path.is_file() {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("{message}");
    }

    #[test]
    fn interrupted_coordinator_creation_recovers() {
        for (stage, expected) in [
            ("zero", &[][..]),
            (
                "partial",
                &read_lease::COORDINATOR_MAGIC[..read_lease::COORDINATOR_MAGIC.len() / 2],
            ),
        ] {
            let root = tempdir().unwrap();
            let status = Command::new(std::env::current_exe().unwrap())
                .arg("--exact")
                .arg("retention::tests::generation_read_lease_creation_crash_child")
                .arg("--nocapture")
                .arg("--test-threads=1")
                .env(CREATION_CRASH_ROOT, root.path())
                .env(read_lease::COORDINATOR_CREATION_CRASH_ENV, stage)
                .status()
                .unwrap();
            assert_eq!(
                status.code(),
                Some(read_lease::COORDINATOR_CREATION_CRASH_EXIT_CODE),
                "{stage} creation crash child"
            );
            let coordinator = root.path().join(read_lease::COORDINATOR_FILE);
            assert_eq!(fs::read(&coordinator).unwrap(), expected, "{stage}");
            ensure_generation_read_lease_coordinator(root.path()).unwrap();
            assert_eq!(
                fs::read(coordinator).unwrap(),
                read_lease::COORDINATOR_MAGIC,
                "{stage} recovery"
            );
            assert_eq!(
                fs::metadata(
                    root.path()
                        .join(read_lease::COORDINATOR_INITIALIZATION_FILE)
                )
                .unwrap()
                .len(),
                0
            );
        }
    }

    #[test]
    fn overlapping_readers_share_exact_ranges_until_the_last_release() {
        let root = tempdir().unwrap();
        let slot = create_slot(root.path(), 'd');
        publish_active_generation_pointer(
            root.path(),
            &ActiveGenerationPointer::new(slot.clone(), None).unwrap(),
        )
        .unwrap();
        let first = acquire_generation_read_lease(root.path(), slot.generation_id()).unwrap();
        let second = acquire_generation_read_lease(root.path(), slot.generation_id()).unwrap();
        assert!(
            try_generation_id_reclaim_authority(root.path(), slot.generation_id())
                .unwrap()
                .is_none()
        );
        assert!(
            try_generation_directory_reclaim_authority(root.path(), slot.directory())
                .unwrap()
                .is_none()
        );
        drop(first);
        assert!(
            try_generation_id_reclaim_authority(root.path(), slot.generation_id())
                .unwrap()
                .is_none()
        );
        drop(second);
        assert!(
            try_generation_id_reclaim_authority(root.path(), slot.generation_id())
                .unwrap()
                .is_some()
        );
        assert!(
            try_generation_directory_reclaim_authority(root.path(), slot.directory())
                .unwrap()
                .is_some()
        );

        let coordinator = root.path().join(read_lease::COORDINATOR_FILE);
        let before = fs::metadata(&coordinator).unwrap();
        for _ in 0..64 {
            drop(acquire_generation_read_lease(root.path(), slot.generation_id()).unwrap());
        }
        let after = fs::metadata(&coordinator).unwrap();
        assert_eq!(after.len(), read_lease::COORDINATOR_MAGIC.len() as u64);
        assert_eq!(after.modified().unwrap(), before.modified().unwrap());
        assert_eq!(
            fs::read_dir(root.path())
                .unwrap()
                .filter_map(|entry| entry.ok())
                .filter(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".ctx-generation-read-lease"))
                .count(),
            1,
            "reader opens created per-generation or per-read lock objects"
        );
    }

    #[test]
    fn exact_generations_are_independent_and_delta_base_manifest_is_pinned() {
        let root = tempdir().unwrap();
        let active = create_slot(root.path(), 'a');
        let previous = create_slot(root.path(), 'b');
        publish_active_generation_pointer(
            root.path(),
            &ActiveGenerationPointer::new(active.clone(), Some(previous.clone())).unwrap(),
        )
        .unwrap();

        let previous_lease =
            acquire_generation_read_lease(root.path(), previous.generation_id()).unwrap();
        assert_eq!(previous_lease.target(), &previous);
        assert!(
            try_generation_id_reclaim_authority(root.path(), previous.generation_id())
                .unwrap()
                .is_none()
        );
        assert!(
            try_generation_directory_reclaim_authority(root.path(), previous.directory())
                .unwrap()
                .is_none()
        );
        assert!(
            try_generation_id_reclaim_authority(root.path(), active.generation_id())
                .unwrap()
                .is_some()
        );
        assert!(
            try_generation_directory_reclaim_authority(root.path(), active.directory())
                .unwrap()
                .is_some()
        );
        let active_lease =
            acquire_generation_read_lease(root.path(), active.generation_id()).unwrap();
        assert_eq!(active_lease.target(), &active);
        drop(active_lease);
        drop(previous_lease);

        let base = create_slot(root.path(), 'c');
        let delta_bytes = format!(
            r#"{{"storage_format":"ctx-manifest-flat-delta-v1","base_generation_id":"{}"}}"#,
            base.generation_id()
        )
        .into_bytes();
        let delta = create_slot_with_bytes(root.path(), 'd', &delta_bytes);
        publish_active_generation_pointer(
            root.path(),
            &ActiveGenerationPointer::new(delta.clone(), None).unwrap(),
        )
        .unwrap();
        let delta_lease =
            acquire_generation_read_lease(root.path(), delta.generation_id()).unwrap();
        assert!(
            try_generation_id_reclaim_authority(root.path(), base.generation_id())
                .unwrap()
                .is_none()
        );
        drop(delta_lease);
        assert!(
            try_generation_id_reclaim_authority(root.path(), base.generation_id())
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn malformed_coordinator_fails_closed() {
        let root = tempdir().unwrap();
        let coordinator = root.path().join(read_lease::COORDINATOR_FILE);
        fs::write(&coordinator, b"malformed").unwrap();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&coordinator)
            .unwrap();
        restrict_private_file_handle(&file).unwrap();
        assert!(matches!(
            ensure_generation_read_lease_coordinator(root.path()),
            Err(IndexError::InvalidGenerationRetentionLease)
        ));
        assert_eq!(fs::read(coordinator).unwrap(), b"malformed");
    }

    #[cfg(unix)]
    #[test]
    fn forked_child_reacquires_real_locks_after_parent_exits() {
        let root = tempdir().unwrap();
        let old = create_slot(root.path(), 'f');
        publish_active_generation_pointer(
            root.path(),
            &ActiveGenerationPointer::new(old.clone(), None).unwrap(),
        )
        .unwrap();
        let ready = root.path().join("fork-child-ready");
        let release = root.path().join("fork-child-release");
        let released = root.path().join("fork-child-released");
        let status = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("retention::tests::generation_read_lease_fork_parent")
            .arg("--nocapture")
            .arg("--test-threads=1")
            .env(FORK_CHILD_ROOT, root.path())
            .env(FORK_CHILD_GENERATION, old.generation_id())
            .env(FORK_CHILD_READY, &ready)
            .env(FORK_CHILD_RELEASE, &release)
            .env(FORK_CHILD_RELEASED, &released)
            .status()
            .unwrap();
        assert!(status.success(), "fork parent failed: {status}");
        wait_for_file(&ready, "forked child did not reacquire its lease");

        let previous = create_slot(root.path(), 'a');
        let active = create_slot(root.path(), 'b');
        let pointer = ActiveGenerationPointer::new(active.clone(), Some(previous.clone())).unwrap();
        publish_active_generation_pointer(root.path(), &pointer).unwrap();
        let retained = vec![
            active.generation_id().to_owned(),
            previous.generation_id().to_owned(),
        ];
        reclaim_inactive_generation_directories(root.path(), Some(&pointer), None).unwrap();
        reclaim_unreferenced_manifests(root.path(), &retained).unwrap();
        reclaim_unreferenced_certifications(root.path(), Some(&pointer), None).unwrap();
        assert!(slot_path(root.path(), &old).is_dir());
        assert!(manifest_path(root.path(), old.generation_id()).is_file());
        assert!(certification_path(root.path(), &old).is_file());

        fs::write(&release, b"release").unwrap();
        wait_for_file(&released, "forked child did not release its lease");
        reclaim_inactive_generation_directories(root.path(), Some(&pointer), None).unwrap();
        reclaim_unreferenced_manifests(root.path(), &retained).unwrap();
        reclaim_unreferenced_certifications(root.path(), Some(&pointer), None).unwrap();
        assert!(!slot_path(root.path(), &old).exists());
        assert!(!manifest_path(root.path(), old.generation_id()).exists());
        assert!(!certification_path(root.path(), &old).exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_and_replaced_coordinator_state_fails_closed() {
        use std::os::unix::fs::symlink;

        let symlink_root = tempdir().unwrap();
        let outside = symlink_root.path().join("outside");
        fs::write(&outside, read_lease::COORDINATOR_MAGIC).unwrap();
        let outside_file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&outside)
            .unwrap();
        restrict_private_file_handle(&outside_file).unwrap();
        symlink(
            &outside,
            symlink_root.path().join(read_lease::COORDINATOR_FILE),
        )
        .unwrap();
        assert!(matches!(
            ensure_generation_read_lease_coordinator(symlink_root.path()),
            Err(IndexError::InvalidGenerationRetentionLease)
        ));

        let root = tempdir().unwrap();
        let slot = create_slot(root.path(), 'e');
        publish_active_generation_pointer(
            root.path(),
            &ActiveGenerationPointer::new(slot.clone(), None).unwrap(),
        )
        .unwrap();
        let lease = acquire_generation_read_lease(root.path(), slot.generation_id()).unwrap();
        let coordinator = root.path().join(read_lease::COORDINATOR_FILE);
        fs::rename(&coordinator, root.path().join("replaced-coordinator")).unwrap();
        fs::write(&coordinator, read_lease::COORDINATOR_MAGIC).unwrap();
        let replacement = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&coordinator)
            .unwrap();
        restrict_private_file_handle(&replacement).unwrap();
        assert!(matches!(
            try_generation_id_reclaim_authority(root.path(), slot.generation_id()),
            Err(IndexError::InvalidGenerationRetentionLease)
        ));
        drop(lease);
    }

    #[test]
    fn cross_process_lease_survives_publication_and_gc_then_crash_is_reconciled() {
        let root = tempdir().unwrap();
        let old = create_slot(root.path(), 'a');
        publish_active_generation_pointer(
            root.path(),
            &ActiveGenerationPointer::new(old.clone(), None).unwrap(),
        )
        .unwrap();

        let spawn_child = |marker: &Path| {
            Command::new(std::env::current_exe().unwrap())
                .arg("--exact")
                .arg("retention::tests::generation_read_lease_crash_child")
                .arg("--nocapture")
                .env(CHILD_ROOT, root.path())
                .env(CHILD_GENERATION, old.generation_id())
                .env(CHILD_MARKER, marker)
                .spawn()
                .unwrap()
        };
        let markers = [
            root.path().join("child-ready-1"),
            root.path().join("child-ready-2"),
        ];
        let mut children = [spawn_child(&markers[0]), spawn_child(&markers[1])];
        for _ in 0..250 {
            if markers.iter().all(|marker| marker.is_file()) {
                break;
            }
            for child in &mut children {
                assert!(
                    child.try_wait().unwrap().is_none(),
                    "lease child exited early"
                );
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            markers.iter().all(|marker| marker.is_file()),
            "lease children did not acquire both shared locks"
        );

        let previous = create_slot(root.path(), 'b');
        let active = create_slot(root.path(), 'c');
        let pointer = ActiveGenerationPointer::new(active.clone(), Some(previous.clone())).unwrap();
        publish_active_generation_pointer(root.path(), &pointer).unwrap();
        let retained = vec![
            active.generation_id().to_owned(),
            previous.generation_id().to_owned(),
        ];
        reclaim_inactive_generation_directories(root.path(), Some(&pointer), None).unwrap();
        reclaim_unreferenced_manifests(root.path(), &retained).unwrap();
        reclaim_unreferenced_certifications(root.path(), Some(&pointer), None).unwrap();
        assert!(slot_path(root.path(), &old).is_dir());
        assert!(manifest_path(root.path(), old.generation_id()).is_file());
        assert!(certification_path(root.path(), &old).is_file());

        children[0].kill().unwrap();
        children[0].wait().unwrap();
        reclaim_inactive_generation_directories(root.path(), Some(&pointer), None).unwrap();
        reclaim_unreferenced_manifests(root.path(), &retained).unwrap();
        reclaim_unreferenced_certifications(root.path(), Some(&pointer), None).unwrap();
        assert!(slot_path(root.path(), &old).is_dir());
        assert!(manifest_path(root.path(), old.generation_id()).is_file());
        assert!(certification_path(root.path(), &old).is_file());

        children[1].kill().unwrap();
        children[1].wait().unwrap();
        reclaim_inactive_generation_directories(root.path(), Some(&pointer), None).unwrap();
        reclaim_unreferenced_manifests(root.path(), &retained).unwrap();
        reclaim_unreferenced_certifications(root.path(), Some(&pointer), None).unwrap();
        assert!(!slot_path(root.path(), &old).exists());
        assert!(!manifest_path(root.path(), old.generation_id()).exists());
        assert!(!certification_path(root.path(), &old).exists());
        assert!(root.path().join(read_lease::COORDINATOR_FILE).is_file());
    }
}
