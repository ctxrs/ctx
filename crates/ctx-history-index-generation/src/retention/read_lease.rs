use std::{
    collections::{HashMap, HashSet},
    io::{Read as _, Seek as _},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc, Mutex, OnceLock, Weak,
    },
};

use serde::Deserialize;
use sha2::{Digest as _, Sha256};

use super::{load_generation_retention_lease_from_read_root, GenerationRetentionLease};
use crate::{
    is_generation_id, read_root::DirectoryIdentity, sha256_hex, GenerationError as IndexError,
    GenerationReadRoot, GenerationSlot, Result, INDEX_GENERATIONS_DIRECTORY, MANIFEST_DIRECTORY,
};

mod platform;

pub(super) const COORDINATOR_FILE: &str = ".ctx-generation-read-leases-v2.lock";
pub(super) const COORDINATOR_INITIALIZATION_FILE: &str =
    ".ctx-generation-lease-coordinator-init-v2.lock";
pub(super) const COORDINATOR_MAGIC: &[u8] = b"ctx-generation-read-leases-v2\n";
#[cfg(test)]
pub(super) const COORDINATOR_CREATION_CRASH_ENV: &str =
    "CTX_GENERATION_READ_LEASE_COORDINATOR_CREATION_CRASH";
#[cfg(test)]
pub(super) const COORDINATOR_CREATION_CRASH_EXIT_CODE: i32 = 86;
const KEY_DOMAIN_BIT: u64 = 1 << 62;
const KEY_VALUE_MASK: u64 = KEY_DOMAIN_BIT - 1;

/// A process-scoped shared hold on one exact immutable generation.
///
/// The OS releases the underlying byte-range locks when the process exits.
/// The single bounded coordinator file is initialized at most once per index
/// root and is never rewritten as readers come and go.
#[derive(Debug)]
pub struct GenerationReadLease {
    process_id: u32,
    root: PathBuf,
    target: GenerationSlot,
    _guards: Vec<RangeLeaseGuard>,
    _retained_authority: Option<Arc<crate::read_root::RetainedReadAuthority>>,
    _read_root: GenerationReadRoot,
}

impl GenerationReadLease {
    pub fn generation_id(&self) -> &str {
        self.target.generation_id()
    }

    #[doc(hidden)]
    pub fn root(&self) -> &Path {
        if self.process_id == std::process::id() {
            &self.root
        } else {
            Path::new("")
        }
    }

    #[doc(hidden)]
    pub fn target(&self) -> &GenerationSlot {
        &self.target
    }

    #[doc(hidden)]
    pub fn with_root_access<T>(&self, access: impl FnOnce(&Path) -> T) -> Result<T> {
        if self.process_id != std::process::id() {
            return Err(IndexError::InvalidGenerationRetentionLease);
        }
        Ok(crate::read_root::with_registered_read_root(
            &self._read_root,
            || access(&self.root),
        ))
    }
}

/// Acquires shared cross-process authority for exactly one currently retained
/// immutable generation.
///
/// Readers never take the global publication lock. Independent generations use
/// independent byte ranges, while overlapping readers of the same generation
/// share the same ranges. The requested id is resolved only against the
/// active/previous pointer pair; there is no generation-selection fallback.
pub fn acquire_generation_read_lease(
    root: impl AsRef<Path>,
    generation_id: &str,
) -> Result<GenerationReadLease> {
    let root = GenerationReadRoot::open_index_root(root)?;
    acquire_generation_read_lease_from_root(root, generation_id)
}

/// Acquires a read lease below an already opened and validated lexical root.
pub fn acquire_generation_read_lease_from_root(
    root: GenerationReadRoot,
    generation_id: &str,
) -> Result<GenerationReadLease> {
    if !is_generation_id(generation_id) {
        return Err(IndexError::InvalidGenerationId);
    }
    let pointer = crate::generation::load_active_generation_pointer_from_read_root(&root)?
        .ok_or(IndexError::MissingActiveGenerationPointer)?;
    let target = if pointer.active().generation_id() == generation_id {
        pointer.active().clone()
    } else if let Some(previous) = pointer
        .previous()
        .filter(|slot| slot.generation_id() == generation_id)
    {
        previous.clone()
    } else {
        return Err(IndexError::GenerationRetentionLeaseTargetNotRetained {
            requested_generation_id: generation_id.to_owned(),
        });
    };
    acquire_target_read_lease(root, target, None)
}

/// Acquires a process-scoped read lease for the exact target named by the
/// currently installed durable retention authority.
///
/// The target is derived only from the validated authority. It is never
/// selected from a caller-supplied path or generation id, and the on-disk
/// authority must still match before the process-scoped lease is returned.
pub fn acquire_retained_generation_read_lease(
    root: impl AsRef<Path>,
    authority: &GenerationRetentionLease,
) -> Result<GenerationReadLease> {
    let root = GenerationReadRoot::open_index_root(root)?;
    acquire_retained_generation_read_lease_from_root(root, authority)
}

/// Acquires an exact durable-target read lease below an already validated root.
pub fn acquire_retained_generation_read_lease_from_root(
    root: GenerationReadRoot,
    authority: &GenerationRetentionLease,
) -> Result<GenerationReadLease> {
    authority.validate()?;
    acquire_target_read_lease(root, authority.target().clone(), Some(authority))
}

fn acquire_target_read_lease(
    root: GenerationReadRoot,
    target: GenerationSlot,
    durable_authority: Option<&GenerationRetentionLease>,
) -> Result<GenerationReadLease> {
    let generation_id = target.generation_id().to_owned();
    let coordinator = coordinator(&root, true)?;
    let primary_guard =
        RangeLeaseGuard::try_shared(Arc::clone(&coordinator), slot_primary_keys(&target)?)?
            .ok_or_else(|| IndexError::GenerationRetentionLeaseConflict {
                retained_generation_id: generation_id.clone(),
                owner_kind: "generation_reclamation".to_owned(),
            })?;
    if let Some(expected) = durable_authority {
        let observed = load_generation_retention_lease_from_read_root(&root)?;
        if observed.as_ref() != Some(expected) {
            return Err(IndexError::GenerationRetentionLeaseOwnerMismatch);
        }
    }
    validate_leased_target(&root, &target)?;

    // Discover a flat-delta dependency only after the target manifest itself
    // is pinned. Otherwise concurrent GC could unlink the target between
    // dependency discovery and acquisition of its ranges.
    let mut guards = vec![primary_guard];
    if let Some(base_generation_id) = referenced_base_generation_id(&root, target.generation_id())?
    {
        let base_guard =
            RangeLeaseGuard::try_shared(coordinator, generation_keys(&base_generation_id)?)?
                .ok_or_else(|| IndexError::GenerationRetentionLeaseConflict {
                    retained_generation_id: generation_id.clone(),
                    owner_kind: "generation_reclamation".to_owned(),
                })?;
        validate_leased_manifest(&root, &base_generation_id)?;
        guards.push(base_guard);
    }

    Ok(GenerationReadLease {
        process_id: std::process::id(),
        root: root.path().to_path_buf(),
        target,
        _guards: guards,
        _retained_authority: durable_authority
            .map(|_| crate::read_root::register_retained_read_authority(&root, &generation_id))
            .transpose()?,
        _read_root: root,
    })
}

pub(crate) fn ensure_generation_read_lease_coordinator(root: &Path) -> Result<()> {
    let root = GenerationReadRoot::open_index_root(root)?;
    coordinator(&root, true)?.verify_binding()
}

pub(crate) fn try_generation_id_reclaim_authority(
    root: &Path,
    generation_id: &str,
) -> Result<Option<GenerationReclaimAuthority>> {
    if !is_generation_id(generation_id) {
        return Err(IndexError::InvalidGenerationId);
    }
    let root = GenerationReadRoot::open_index_root(root)?;
    let coordinator = coordinator(&root, true)?;
    RangeLeaseGuard::try_exclusive(coordinator, generation_keys(generation_id)?)
        .map(|guard| guard.map(|guard| GenerationReclaimAuthority { _guard: guard }))
}

pub(crate) fn try_generation_directory_reclaim_authority(
    root: &Path,
    directory: &str,
) -> Result<Option<GenerationReclaimAuthority>> {
    if !GenerationSlot::names_are_valid(&"0".repeat(64), directory) {
        return Err(IndexError::InvalidActiveGenerationPointer);
    }
    let root = GenerationReadRoot::open_index_root(root)?;
    let coordinator = coordinator(&root, true)?;
    RangeLeaseGuard::try_exclusive(coordinator, directory_keys(directory))
        .map(|guard| guard.map(|guard| GenerationReclaimAuthority { _guard: guard }))
}

/// Pins a generation directory only when another process-scoped reader
/// already holds it. This lets publication account for legitimate hard-link
/// aliases without turning an otherwise unretained directory into authority.
pub(crate) fn acquire_existing_generation_directory_read_authority(
    root: &Path,
    directory: &str,
) -> Result<Option<ExistingGenerationDirectoryReadAuthority>> {
    if !GenerationSlot::names_are_valid(&"0".repeat(64), directory) {
        return Err(IndexError::InvalidActiveGenerationPointer);
    }
    let root = GenerationReadRoot::open_index_root(root)?;
    let coordinator = coordinator(&root, true)?;
    let keys = directory_keys(directory);
    if let Some(uncontended) =
        RangeLeaseGuard::try_exclusive(Arc::clone(&coordinator), keys.clone())?
    {
        drop(uncontended);
        return Ok(None);
    }
    let guard = RangeLeaseGuard::try_shared(coordinator, keys)?
        .ok_or(IndexError::ConcurrentGenerationChange)?;
    Ok(Some(ExistingGenerationDirectoryReadAuthority {
        _guard: guard,
    }))
}

#[derive(Debug)]
pub(crate) struct ExistingGenerationDirectoryReadAuthority {
    _guard: RangeLeaseGuard,
}

#[derive(Debug)]
pub(crate) struct GenerationReclaimAuthority {
    _guard: RangeLeaseGuard,
}

fn validate_leased_target(root: &GenerationReadRoot, target: &GenerationSlot) -> Result<()> {
    target.validate()?;
    root.opened()
        .open_directory(&Path::new(INDEX_GENERATIONS_DIRECTORY).join(target.directory()))
        .map_err(|_| IndexError::GenerationRetentionLeaseTargetNotRetained {
            requested_generation_id: target.generation_id().to_owned(),
        })?;
    validate_leased_manifest(root, target.generation_id())?;
    Ok(())
}

fn validate_leased_manifest(root: &GenerationReadRoot, generation_id: &str) -> Result<()> {
    root.open_file(&manifest_relative_path(generation_id))
        .map(|_| ())
        .map_err(|_| IndexError::GenerationRetentionLeaseTargetNotRetained {
            requested_generation_id: generation_id.to_owned(),
        })
}

const MANIFEST_FLAT_DELTA_PREFIX: &[u8] = br#"{"storage_format":"ctx-manifest-flat-delta-v1","#;

#[derive(Deserialize)]
struct ManifestDeltaReference {
    storage_format: String,
    base_generation_id: String,
}

fn referenced_base_generation_id(
    root: &GenerationReadRoot,
    generation_id: &str,
) -> Result<Option<String>> {
    let mut file = root
        .open_file(&manifest_relative_path(generation_id))
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => IndexError::MissingManifest(generation_id.to_owned()),
            _ => IndexError::Io(error),
        })?;
    let mut prefix = [0_u8; 64];
    let prefix_bytes = file.read(&mut prefix)?;
    if !prefix[..prefix_bytes].starts_with(MANIFEST_FLAT_DELTA_PREFIX) {
        return Ok(None);
    }
    file.rewind()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let actual = sha256_hex(&bytes);
    if actual != generation_id {
        return Err(IndexError::ManifestDigestMismatch {
            expected: generation_id.to_owned(),
            actual,
        });
    }
    let reference: ManifestDeltaReference = serde_json::from_slice(&bytes)?;
    if reference.storage_format != "ctx-manifest-flat-delta-v1"
        || !is_generation_id(&reference.base_generation_id)
    {
        return Err(IndexError::InvalidGenerationId);
    }
    Ok(Some(reference.base_generation_id))
}

fn manifest_relative_path(generation_id: &str) -> PathBuf {
    Path::new(MANIFEST_DIRECTORY).join(format!("{generation_id}.json"))
}

fn slot_primary_keys(slot: &GenerationSlot) -> Result<Vec<u64>> {
    let mut keys = generation_keys(slot.generation_id())?;
    keys.extend(directory_keys(slot.directory()));
    keys.sort_unstable();
    keys.dedup();
    Ok(keys)
}

fn generation_keys(generation_id: &str) -> Result<Vec<u64>> {
    if !is_generation_id(generation_id) {
        return Err(IndexError::InvalidGenerationId);
    }
    let mut keys = Vec::with_capacity(4);
    for chunk in generation_id.as_bytes().chunks_exact(16) {
        let chunk = std::str::from_utf8(chunk).map_err(|_| IndexError::InvalidGenerationId)?;
        let value = u64::from_str_radix(chunk, 16).map_err(|_| IndexError::InvalidGenerationId)?;
        keys.push(value & KEY_VALUE_MASK);
    }
    keys.sort_unstable();
    keys.dedup();
    Ok(keys)
}

fn directory_keys(directory: &str) -> Vec<u64> {
    let digest = Sha256::digest(directory.as_bytes());
    let mut keys = digest
        .chunks_exact(8)
        .map(|chunk| {
            let mut bytes = [0_u8; 8];
            bytes.copy_from_slice(chunk);
            KEY_DOMAIN_BIT | (u64::from_be_bytes(bytes) & KEY_VALUE_MASK)
        })
        .collect::<Vec<_>>();
    keys.sort_unstable();
    keys.dedup();
    keys
}

#[derive(Debug, Default)]
struct LocalRangeState {
    shared: HashMap<u64, usize>,
    exclusive: HashSet<u64>,
}

#[derive(Debug)]
struct LeaseCoordinator {
    process_id: u32,
    opened: platform::OpenedCoordinator,
    local: Mutex<LocalRangeState>,
}

impl LeaseCoordinator {
    fn verify_binding(&self) -> Result<()> {
        if self.process_id != std::process::id() {
            return Err(IndexError::InvalidGenerationRetentionLease);
        }
        self.opened
            .verify_binding(COORDINATOR_FILE, COORDINATOR_MAGIC)
            .map_err(|_| IndexError::InvalidGenerationRetentionLease)
    }
}

#[derive(Debug)]
struct CoordinatorRegistry {
    process_id: u32,
    coordinators: HashMap<DirectoryIdentity, Weak<LeaseCoordinator>>,
    inherited: Vec<Arc<LeaseCoordinator>>,
}

impl CoordinatorRegistry {
    fn new(process_id: u32) -> Self {
        Self {
            process_id,
            coordinators: HashMap::new(),
            inherited: Vec::new(),
        }
    }

    fn reset_after_fork(&mut self, process_id: u32) {
        // POSIX record locks are process-associated and are not inherited by a
        // child. Pin every inherited coordinator descriptor before rebuilding
        // the registry: closing one of those descriptors later would otherwise
        // release all locks the child reacquires on the same file.
        self.inherited.extend(
            self.coordinators
                .drain()
                .filter_map(|(_, coordinator)| coordinator.upgrade()),
        );
        self.process_id = process_id;
    }
}

static COORDINATORS: OnceLock<Mutex<CoordinatorRegistry>> = OnceLock::new();
static COORDINATOR_REGISTRY_PROCESS_ID: AtomicU32 = AtomicU32::new(0);

fn coordinator(root: &GenerationReadRoot, create: bool) -> Result<Arc<LeaseCoordinator>> {
    let process_id = std::process::id();
    let registry = COORDINATORS.get_or_init(|| Mutex::new(CoordinatorRegistry::new(process_id)));
    let registered_process_id = COORDINATOR_REGISTRY_PROCESS_ID.load(Ordering::Acquire);
    let mut registry = if registered_process_id != 0 && registered_process_id != process_id {
        // A mutex copied while another parent thread owned it cannot safely be
        // recovered after fork. Fail closed instead of waiting forever.
        registry
            .try_lock()
            .map_err(|_| IndexError::InvalidGenerationRetentionLease)?
    } else {
        registry
            .lock()
            .map_err(|_| IndexError::InvalidGenerationRetentionLease)?
    };
    if registry.process_id != process_id {
        registry.reset_after_fork(process_id);
    }
    COORDINATOR_REGISTRY_PROCESS_ID.store(process_id, Ordering::Release);

    if let Some(existing) = registry
        .coordinators
        .get(&root.identity())
        .and_then(Weak::upgrade)
    {
        drop(registry);
        existing.verify_binding()?;
        return Ok(existing);
    }

    let opened = platform::OpenedCoordinator::open(
        root.opened(),
        COORDINATOR_FILE,
        COORDINATOR_INITIALIZATION_FILE,
        COORDINATOR_MAGIC,
        create,
    )
    .map_err(|_| IndexError::InvalidGenerationRetentionLease)?;
    let coordinator = Arc::new(LeaseCoordinator {
        process_id,
        opened,
        local: Mutex::new(LocalRangeState::default()),
    });
    registry
        .coordinators
        .insert(root.identity(), Arc::downgrade(&coordinator));
    drop(registry);
    coordinator.verify_binding()?;
    Ok(coordinator)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RangeLockKind {
    Shared,
    Exclusive,
}

#[derive(Debug)]
struct RangeLeaseGuard {
    coordinator: Arc<LeaseCoordinator>,
    keys: Vec<u64>,
    kind: RangeLockKind,
}

impl RangeLeaseGuard {
    fn try_shared(coordinator: Arc<LeaseCoordinator>, keys: Vec<u64>) -> Result<Option<Self>> {
        coordinator.verify_binding()?;
        let mut local = coordinator
            .local
            .lock()
            .map_err(|_| IndexError::InvalidGenerationRetentionLease)?;
        if keys.iter().any(|key| local.exclusive.contains(key)) {
            return Ok(None);
        }
        let next_counts = keys
            .iter()
            .map(|key| {
                local
                    .shared
                    .get(key)
                    .copied()
                    .unwrap_or_default()
                    .checked_add(1)
                    .map(|count| (*key, count))
                    .ok_or(IndexError::CountOverflow)
            })
            .collect::<Result<Vec<_>>>()?;

        let mut newly_locked = Vec::new();
        for key in &keys {
            if local.shared.contains_key(key) {
                continue;
            }
            match coordinator
                .opened
                .try_lock(*key, platform::LockKind::Shared)
            {
                Ok(true) => newly_locked.push(*key),
                Ok(false) => {
                    rollback_locks(&coordinator, &newly_locked)?;
                    return Ok(None);
                }
                Err(error) => {
                    rollback_locks(&coordinator, &newly_locked)?;
                    return Err(IndexError::Io(error));
                }
            }
        }
        local.shared.extend(next_counts);
        drop(local);
        let guard = Self {
            coordinator,
            keys,
            kind: RangeLockKind::Shared,
        };
        if let Err(error) = guard.coordinator.verify_binding() {
            drop(guard);
            return Err(error);
        }
        Ok(Some(guard))
    }

    fn try_exclusive(coordinator: Arc<LeaseCoordinator>, keys: Vec<u64>) -> Result<Option<Self>> {
        coordinator.verify_binding()?;
        let mut local = coordinator
            .local
            .lock()
            .map_err(|_| IndexError::InvalidGenerationRetentionLease)?;
        if keys
            .iter()
            .any(|key| local.exclusive.contains(key) || local.shared.contains_key(key))
        {
            return Ok(None);
        }

        let mut newly_locked = Vec::new();
        for key in &keys {
            match coordinator
                .opened
                .try_lock(*key, platform::LockKind::Exclusive)
            {
                Ok(true) => newly_locked.push(*key),
                Ok(false) => {
                    rollback_locks(&coordinator, &newly_locked)?;
                    return Ok(None);
                }
                Err(error) => {
                    rollback_locks(&coordinator, &newly_locked)?;
                    return Err(IndexError::Io(error));
                }
            }
        }
        local.exclusive.extend(keys.iter().copied());
        drop(local);
        let guard = Self {
            coordinator,
            keys,
            kind: RangeLockKind::Exclusive,
        };
        if let Err(error) = guard.coordinator.verify_binding() {
            drop(guard);
            return Err(error);
        }
        Ok(Some(guard))
    }
}

impl Drop for RangeLeaseGuard {
    fn drop(&mut self) {
        if self.coordinator.process_id != std::process::id() {
            return;
        }
        let Ok(mut local) = self.coordinator.local.lock() else {
            return;
        };
        for key in &self.keys {
            let should_unlock = match self.kind {
                RangeLockKind::Shared => match local.shared.get_mut(key) {
                    Some(count) if *count > 1 => {
                        *count -= 1;
                        false
                    }
                    Some(_) => {
                        local.shared.remove(key);
                        true
                    }
                    None => false,
                },
                RangeLockKind::Exclusive => local.exclusive.remove(key),
            };
            if should_unlock {
                let _ = self.coordinator.opened.unlock(*key);
            }
        }
    }
}

#[cfg(test)]
fn coordinator_creation_crash_prefix_len(magic_len: usize) -> Option<usize> {
    match std::env::var(COORDINATOR_CREATION_CRASH_ENV).ok()?.as_str() {
        "zero" => Some(0),
        "partial" => Some(magic_len / 2),
        _ => None,
    }
}

fn rollback_locks(coordinator: &LeaseCoordinator, keys: &[u64]) -> Result<()> {
    for key in keys.iter().rev() {
        coordinator.opened.unlock(*key).map_err(IndexError::Io)?;
    }
    Ok(())
}
