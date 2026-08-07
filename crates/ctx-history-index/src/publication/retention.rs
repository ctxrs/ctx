use std::{
    fs::{self, File, OpenOptions},
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
};

use ctx_history_core::platform_security::{restrict_private_file_handle, verify_private_file};
use serde::{Deserialize, Serialize};
use tantivy::directory::Lock;

use crate::{
    durable_atomic_replace_file, durable_directory::DurableMmapDirectory, is_generation_id,
    sync_directory, writer_support::acquire_generation_writer_lock_with_retry, GenerationSlot,
    IndexError, Result,
};

pub(crate) const GENERATION_WRITER_LOCK_FILE: &str = ".ctx-generation-writer.lock";
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

    pub(crate) fn target(&self) -> &GenerationSlot {
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

    let pointer = super::load_active_generation_pointer(&root)?
        .ok_or(IndexError::MissingActiveGenerationPointer)?;
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
    let target = super::slot_path(root.as_ref(), lease.target());
    let target_metadata =
        fs::symlink_metadata(target).map_err(|_| IndexError::InvalidGenerationRetentionLease)?;
    if !target_metadata.is_dir() || target_metadata.file_type().is_symlink() {
        return Err(IndexError::InvalidGenerationRetentionLease);
    }
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
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staged)?;
    restrict_private_file_handle(&file)?;
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

fn remove_lease_file(root: &Path) -> Result<()> {
    match fs::remove_file(lease_path(root)) {
        Ok(()) => sync_directory(root).map_err(Into::into),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
