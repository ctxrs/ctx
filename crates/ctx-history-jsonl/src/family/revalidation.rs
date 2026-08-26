#[cfg(any(test, feature = "test-support"))]
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};
use std::{fs::File, io::Read, path::Path};

use sha2::{Digest, Sha256};

use super::{identity::observe_metadata, new_prefix_hasher, JsonlFileObservation};
use super::{JsonlFamilyError, JsonlResult, JsonlResumableSha256, OpenedProviderSourceFile};

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static PREFIX_HASH_BYTES: Cell<u64> = const { Cell::new(0) };
    static AFTER_PREFIX_HASH_HOOK: RefCell<Option<Box<dyn FnOnce()>>> =
        const { RefCell::new(None) };
    static AFTER_SECOND_PREFIX_HASH_HOOK: RefCell<Option<Box<dyn FnOnce()>>> =
        const { RefCell::new(None) };
    static AFTER_FINAL_PREFIX_HASH_HOOK: RefCell<Option<Box<dyn FnOnce()>>> =
        const { RefCell::new(None) };
}

#[cfg(any(test, feature = "test-support"))]
struct AppendObservationHook {
    source_path: PathBuf,
    hook: Box<dyn FnOnce() + Send>,
}

#[cfg(any(test, feature = "test-support"))]
struct SemanticPreflightHook {
    source_path: PathBuf,
    hook: Box<dyn FnOnce() + Send>,
}

#[cfg(any(test, feature = "test-support"))]
struct TrackedPrefixHashBytes {
    source_path: PathBuf,
    bytes: Arc<AtomicU64>,
}

/// Process-global, source-scoped prefix-hash accounting for worker-thread
/// assertions. Hook dispatch and the legacy aggregate remain thread-local.
#[cfg(any(test, feature = "test-support"))]
pub struct JsonlPrefixHashBytesGuard {
    source_path: PathBuf,
    bytes: Arc<AtomicU64>,
}

#[cfg(any(test, feature = "test-support"))]
impl JsonlPrefixHashBytesGuard {
    pub fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::SeqCst)
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for JsonlPrefixHashBytesGuard {
    fn drop(&mut self) {
        let mut tracked = TRACKED_PREFIX_HASH_BYTES
            .lock()
            .expect("JSONL tracked prefix-hash byte lock was poisoned");
        let index = tracked
            .iter()
            .position(|entry| {
                entry.source_path == self.source_path && Arc::ptr_eq(&entry.bytes, &self.bytes)
            })
            .expect("JSONL tracked prefix-hash byte guard was not registered");
        tracked.remove(index);
    }
}

#[cfg(any(test, feature = "test-support"))]
static AFTER_APPEND_OBSERVATION_ROUTE_BINDING_HOOKS: Mutex<Vec<AppendObservationHook>> =
    Mutex::new(Vec::new());

#[cfg(any(test, feature = "test-support"))]
static AFTER_SEMANTIC_PREFLIGHT_HOOKS: Mutex<Vec<SemanticPreflightHook>> = Mutex::new(Vec::new());

#[cfg(any(test, feature = "test-support"))]
static TRACKED_PREFIX_HASH_BYTES: Mutex<Vec<TrackedPrefixHashBytes>> = Mutex::new(Vec::new());

#[cfg(any(test, feature = "test-support"))]
pub fn track_jsonl_prefix_hash_bytes(source_path: PathBuf) -> JsonlPrefixHashBytesGuard {
    let bytes = Arc::new(AtomicU64::new(0));
    TRACKED_PREFIX_HASH_BYTES
        .lock()
        .expect("JSONL tracked prefix-hash byte lock was poisoned")
        .push(TrackedPrefixHashBytes {
            source_path: source_path.clone(),
            bytes: Arc::clone(&bytes),
        });
    JsonlPrefixHashBytesGuard { source_path, bytes }
}

#[cfg(any(test, feature = "test-support"))]
pub fn reset_jsonl_prefix_hash_bytes() {
    PREFIX_HASH_BYTES.set(0);
}

#[cfg(any(test, feature = "test-support"))]
pub fn jsonl_prefix_hash_bytes() -> u64 {
    PREFIX_HASH_BYTES.get()
}

#[cfg(any(test, feature = "test-support"))]
fn record_jsonl_prefix_hash_bytes(source_path: &Path, bytes: u64) {
    PREFIX_HASH_BYTES.with(|total| {
        total.set(total.get().saturating_add(bytes));
    });
    for tracked in TRACKED_PREFIX_HASH_BYTES
        .lock()
        .expect("JSONL tracked prefix-hash byte lock was poisoned")
        .iter()
        .filter(|tracked| tracked.source_path == source_path)
    {
        tracked.bytes.fetch_add(bytes, Ordering::SeqCst);
    }
}

#[cfg(any(test, feature = "test-support"))]
pub fn set_after_jsonl_prefix_hash_hook(hook: impl FnOnce() + 'static) {
    AFTER_PREFIX_HASH_HOOK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(hook));
    });
}

#[cfg(any(test, feature = "test-support"))]
pub fn set_after_second_jsonl_prefix_hash_hook(hook: impl FnOnce() + 'static) {
    AFTER_SECOND_PREFIX_HASH_HOOK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(hook));
    });
}

#[cfg(any(test, feature = "test-support"))]
pub fn set_after_final_jsonl_prefix_hash_hook(hook: impl FnOnce() + 'static) {
    AFTER_FINAL_PREFIX_HASH_HOOK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(hook));
    });
}

#[cfg(any(test, feature = "test-support"))]
pub fn set_after_jsonl_append_observation_route_binding_hook(
    source_path: PathBuf,
    hook: impl FnOnce() + Send + 'static,
) {
    let mut hooks = AFTER_APPEND_OBSERVATION_ROUTE_BINDING_HOOKS
        .lock()
        .expect("JSONL append-observation hook lock was poisoned");
    assert!(
        hooks
            .iter()
            .all(|pending| pending.source_path != source_path),
        "JSONL append-observation hook is already installed for {source_path:?}"
    );
    hooks.push(AppendObservationHook {
        source_path,
        hook: Box::new(hook),
    });
}

#[cfg(any(test, feature = "test-support"))]
pub fn set_after_jsonl_semantic_preflight_hook(
    source_path: PathBuf,
    hook: impl FnOnce() + Send + 'static,
) {
    let mut hooks = AFTER_SEMANTIC_PREFLIGHT_HOOKS
        .lock()
        .expect("JSONL semantic-preflight hook lock was poisoned");
    assert!(
        hooks
            .iter()
            .all(|pending| pending.source_path != source_path),
        "JSONL semantic-preflight hook is already installed for {source_path:?}"
    );
    hooks.push(SemanticPreflightHook {
        source_path,
        hook: Box::new(hook),
    });
}

#[cfg(any(test, feature = "test-support"))]
fn run_after_jsonl_prefix_hash_hook() {
    AFTER_PREFIX_HASH_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(any(test, feature = "test-support"))]
fn run_after_second_jsonl_prefix_hash_hook() {
    AFTER_SECOND_PREFIX_HASH_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(any(test, feature = "test-support"))]
fn run_after_final_jsonl_prefix_hash_hook() {
    AFTER_FINAL_PREFIX_HASH_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(any(test, feature = "test-support"))]
fn run_after_jsonl_append_observation_route_binding_hook(source_path: &Path) {
    let hook = {
        let mut hooks = AFTER_APPEND_OBSERVATION_ROUTE_BINDING_HOOKS
            .lock()
            .expect("JSONL append-observation hook lock was poisoned");
        hooks
            .iter()
            .position(|pending| pending.source_path == source_path)
            .map(|index| hooks.remove(index).hook)
    };
    if let Some(hook) = hook {
        hook();
    }
}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn run_after_jsonl_semantic_preflight_hook(source_path: &Path) {
    let hook = {
        let mut hooks = AFTER_SEMANTIC_PREFLIGHT_HOOKS
            .lock()
            .expect("JSONL semantic-preflight hook lock was poisoned");
        hooks
            .iter()
            .position(|pending| pending.source_path == source_path)
            .map(|index| hooks.remove(index).hook)
    };
    if let Some(hook) = hook {
        hook();
    }
}

pub fn observe_opened_file<E: JsonlFamilyError>(
    source_path: &Path,
    opened: &OpenedProviderSourceFile<E>,
) -> JsonlResult<JsonlFileObservation, E> {
    opened.revalidate()?;
    let observation = observe_metadata::<E>(source_path, opened.file(), opened.metadata())?;
    opened.revalidate()?;
    Ok(observation)
}

/// Observes current metadata for a retained append-only file while requiring
/// its named route to keep identifying the same ordinary object. Callers must
/// separately prove the exact bytes in their frozen publication prefix.
pub fn observe_opened_file_allow_append<E: JsonlFamilyError>(
    source_path: &Path,
    opened: &OpenedProviderSourceFile<E>,
) -> JsonlResult<JsonlFileObservation, E> {
    opened.revalidate_same_object()?;
    #[cfg(any(test, feature = "test-support"))]
    run_after_jsonl_append_observation_route_binding_hook(source_path);
    let metadata = opened.file().metadata()?;
    let observation = observe_metadata::<E>(source_path, opened.file(), &metadata)?;
    opened.revalidate_same_object()?;
    Ok(observation)
}

pub fn revalidate_frozen_prefix<E: JsonlFamilyError>(
    source_path: &Path,
    source_file: &OpenedProviderSourceFile<E>,
    frozen: &JsonlFileObservation,
    prefix_length: u64,
    expected_prefix_digest: [u8; 32],
) -> JsonlResult<JsonlFileObservation, E> {
    revalidate_frozen_prefix_with_hasher(
        source_path,
        source_file,
        frozen,
        prefix_length,
        expected_prefix_digest,
        new_prefix_hasher(),
        false,
    )
}

pub(crate) fn authenticate_frozen_prefix<E: JsonlFamilyError>(
    source_path: &Path,
    source_file: &OpenedProviderSourceFile<E>,
    frozen: &JsonlFileObservation,
    prefix_length: u64,
    expected_prefix_digest: [u8; 32],
) -> JsonlResult<JsonlFileObservation, E> {
    revalidate_frozen_prefix_with_hasher(
        source_path,
        source_file,
        frozen,
        prefix_length,
        expected_prefix_digest,
        new_prefix_hasher(),
        true,
    )
}

pub(crate) fn revalidate_frozen_prefix_sha256<E: JsonlFamilyError>(
    source_path: &Path,
    source_file: &OpenedProviderSourceFile<E>,
    frozen: &JsonlFileObservation,
    prefix_length: u64,
    expected_prefix_digest: [u8; 32],
) -> JsonlResult<JsonlFileObservation, E> {
    revalidate_frozen_prefix_with_hasher(
        source_path,
        source_file,
        frozen,
        prefix_length,
        expected_prefix_digest,
        Sha256::default(),
        false,
    )
}

pub(crate) fn authenticate_frozen_prefix_sha256<E: JsonlFamilyError>(
    source_path: &Path,
    source_file: &OpenedProviderSourceFile<E>,
    frozen: &JsonlFileObservation,
    prefix_length: u64,
    expected_prefix_digest: [u8; 32],
) -> JsonlResult<JsonlFileObservation, E> {
    revalidate_frozen_prefix_with_hasher(
        source_path,
        source_file,
        frozen,
        prefix_length,
        expected_prefix_digest,
        Sha256::default(),
        true,
    )
}

fn revalidate_frozen_prefix_with_hasher<E: JsonlFamilyError>(
    source_path: &Path,
    source_file: &OpenedProviderSourceFile<E>,
    frozen: &JsonlFileObservation,
    prefix_length: u64,
    expected_prefix_digest: [u8; 32],
    prefix_hasher: impl JsonlPrefixHasher,
    force_authentication: bool,
) -> JsonlResult<JsonlFileObservation, E> {
    if prefix_length > frozen.length() {
        return Err(E::source_changed());
    }
    let before = observe_metadata::<E>(
        source_path,
        source_file.file(),
        &source_file.file().metadata()?,
    )?;
    if !frozen.admits_frozen_prefix_in(&before) {
        return Err(E::source_changed());
    }
    source_file.revalidate_same_object()?;

    // An unchanged strong observation already binds the retained bytes. This
    // keeps an exact no-op metadata-only instead of rehashing the whole corpus.
    if &before == frozen && !force_authentication {
        return Ok(before);
    }

    verify_prefix_digest(
        source_path,
        source_file,
        prefix_length,
        expected_prefix_digest,
        prefix_hasher.clone(),
    )?;
    #[cfg(any(test, feature = "test-support"))]
    run_after_jsonl_prefix_hash_hook();
    let middle = observe_metadata::<E>(
        source_path,
        source_file.file(),
        &source_file.file().metadata()?,
    )?;
    source_file.revalidate_same_object()?;
    if !before.admits_frozen_prefix_in(&middle) {
        return Err(E::source_changed());
    }

    // Exact prefix equality plus monotonic same-object observations admits a
    // continuously growing append log. Requiring metadata to stop changing
    // would make an active terminal source impossible to certify.
    verify_prefix_digest(
        source_path,
        source_file,
        prefix_length,
        expected_prefix_digest,
        prefix_hasher.clone(),
    )?;
    #[cfg(any(test, feature = "test-support"))]
    run_after_second_jsonl_prefix_hash_hook();
    let after = observe_metadata::<E>(
        source_path,
        source_file.file(),
        &source_file.file().metadata()?,
    )?;
    source_file.revalidate_same_object()?;
    if !middle.admits_frozen_prefix_in(&after) {
        return Err(E::source_changed());
    }

    // End on content proof so rewrite-plus-append after the preceding metadata
    // observation cannot be mistaken for deferred growth.
    verify_prefix_digest(
        source_path,
        source_file,
        prefix_length,
        expected_prefix_digest,
        prefix_hasher,
    )?;
    #[cfg(any(test, feature = "test-support"))]
    run_after_final_jsonl_prefix_hash_hook();
    // Bind the final content proof to both the retained object that was hashed
    // and the authority-relative directory entry that currently names it.
    // Append growth remains permitted because this compares object identity,
    // while replacement, ancestor swaps, and route retargeting fail closed.
    source_file.revalidate_same_object()?;
    Ok(after)
}

pub(super) trait JsonlPrefixHasher: Clone {
    fn update_bytes(&mut self, bytes: &[u8]);
    fn finish_digest(&self) -> [u8; 32];
}

impl JsonlPrefixHasher for JsonlResumableSha256 {
    fn update_bytes(&mut self, bytes: &[u8]) {
        self.update(bytes);
    }

    fn finish_digest(&self) -> [u8; 32] {
        self.digest()
    }
}

impl JsonlPrefixHasher for Sha256 {
    fn update_bytes(&mut self, bytes: &[u8]) {
        Digest::update(self, bytes);
    }

    fn finish_digest(&self) -> [u8; 32] {
        self.clone().finalize().into()
    }
}

fn verify_prefix_digest<E: JsonlFamilyError, H: JsonlPrefixHasher>(
    source_path: &Path,
    source_file: &OpenedProviderSourceFile<E>,
    prefix_length: u64,
    expected_prefix_digest: [u8; 32],
    prefix_hasher: H,
) -> JsonlResult<(), E> {
    let observed = hash_prefix::<E, _>(
        source_path,
        &mut source_file.reopen_same_object()?,
        prefix_length,
        prefix_hasher,
    )?;
    if observed.finish_digest() != expected_prefix_digest {
        return Err(E::source_changed());
    }
    Ok(())
}

pub(super) fn hash_prefix<E: JsonlFamilyError, H: JsonlPrefixHasher>(
    source_path: &Path,
    file: &mut File,
    length: u64,
    mut hasher: H,
) -> JsonlResult<H, E> {
    use std::io::{Seek, SeekFrom};

    #[cfg(not(any(test, feature = "test-support")))]
    let _ = source_path;

    file.seek(SeekFrom::Start(0))?;
    let mut remaining = length;
    let mut buffer = [0_u8; 64 * 1024];
    while remaining > 0 {
        let requested = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| E::system_invariant("JSONL prefix length exceeds usize"))?;
        let read = file.read(&mut buffer[..requested])?;
        if read == 0 {
            return Err(E::source_changed());
        }
        #[cfg(any(test, feature = "test-support"))]
        record_jsonl_prefix_hash_bytes(source_path, read as u64);
        hasher.update_bytes(&buffer[..read]);
        remaining = remaining.saturating_sub(read as u64);
    }
    Ok(hasher)
}
