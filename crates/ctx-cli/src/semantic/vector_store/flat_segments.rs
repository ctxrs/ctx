use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use memmap2::{Mmap, MmapOptions};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

mod artifacts;
mod manifest;
mod pinned;
mod recovery;
mod validation;

use artifacts::*;
use manifest::*;
use pinned::load_pinned_generation;
pub(in crate::semantic) use pinned::PinnedFlatGeneration;
#[cfg(test)]
pub(in crate::semantic) use pinned::PinnedScanSegment;
use recovery::*;
use validation::*;

pub(in crate::semantic) type FlatResult<T> = std::result::Result<T, FlatStoreError>;

const COMPACT_SEGMENT_THRESHOLD: usize = 16;

#[derive(Debug, Error)]
pub(in crate::semantic) enum FlatStoreError {
    #[error("{operation} {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid flat F32 input: {0}")]
    InvalidInput(String),
    #[error("corrupt flat F32 store: {0}")]
    Corrupt(String),
    #[error("incompatible flat F32 store: {0}")]
    Incompatible(String),
    #[error("flat F32 store is read-only")]
    ReadOnly,
    #[error("unsupported flat F32 platform: {0}")]
    Unsupported(String),
    #[error("serialize flat F32 manifest: {0}")]
    Serialize(#[from] serde_json::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::semantic) struct FlatModelContract {
    pub(in crate::semantic) contract_version: u32,
    pub(in crate::semantic) model_id: String,
    pub(in crate::semantic) model_revision: String,
    pub(in crate::semantic) tokenizer: String,
    pub(in crate::semantic) pooling: String,
    pub(in crate::semantic) dimensions: u32,
    pub(in crate::semantic) normalization: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(in crate::semantic) struct FlatSourceHash([u8; 32]);

impl FlatSourceHash {
    pub(in crate::semantic) fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub(in crate::semantic) fn parse_hex(value: &str) -> FlatResult<Self> {
        let bytes = decode_sha256(value).ok_or_else(|| {
            FlatStoreError::InvalidInput("source text hash must be lowercase SHA-256".to_owned())
        })?;
        Ok(Self(bytes))
    }

    pub(in crate::semantic) fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub(in crate::semantic) fn to_hex(self) -> String {
        encode_hex(&self.0)
    }
}

#[derive(Debug, Clone)]
pub(in crate::semantic) struct FlatChunk {
    pub(in crate::semantic) chunk_index: u32,
    pub(in crate::semantic) start_char: u32,
    pub(in crate::semantic) end_char: u32,
    pub(in crate::semantic) vector: Vec<f32>,
}

#[derive(Debug, Clone)]
pub(in crate::semantic) struct FlatEventReplacement {
    pub(in crate::semantic) event_id: Uuid,
    pub(in crate::semantic) seq: u64,
    pub(in crate::semantic) source_text_hash: FlatSourceHash,
    pub(in crate::semantic) chunks: Vec<FlatChunk>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::semantic) struct FlatActiveEvent {
    pub(in crate::semantic) event_id: Uuid,
    pub(in crate::semantic) seq: u64,
    pub(in crate::semantic) source_text_hash: FlatSourceHash,
    pub(in crate::semantic) chunk_count: u32,
}

/// Read-only event lookup bound to one exact flat generation.
///
/// Generation loading stores active event summaries in UUID order, so callers
/// can probe this shared pin without cloning or linearly scanning the active
/// corpus for every event.
#[derive(Clone)]
pub(in crate::semantic) struct FlatActiveEventLookup {
    pinned: Option<PinnedFlatGeneration>,
}

impl FlatActiveEventLookup {
    pub(in crate::semantic) fn event(&self, event_id: Uuid) -> Option<&FlatActiveEvent> {
        let events = self.pinned.as_ref()?.active_events();
        events
            .binary_search_by_key(&event_id, |event| event.event_id)
            .ok()
            .map(|index| &events[index])
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(in crate::semantic) struct FlatActiveStats {
    pub(in crate::semantic) generation: u64,
    pub(in crate::semantic) generation_hash: Option<String>,
    pub(in crate::semantic) segment_count: usize,
    pub(in crate::semantic) active_events: usize,
    pub(in crate::semantic) active_chunks: usize,
    pub(in crate::semantic) active_vector_bytes: u64,
    pub(in crate::semantic) stored_chunks: u64,
    pub(in crate::semantic) stored_vector_bytes: u64,
    pub(in crate::semantic) deleted_events: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(in crate::semantic) struct FlatRecoveryReport {
    pub(in crate::semantic) model_contract_reset: bool,
    pub(in crate::semantic) removed_temporary_files: usize,
    pub(in crate::semantic) removed_obsolete_manifests: usize,
    pub(in crate::semantic) removed_orphan_segments: usize,
    pub(in crate::semantic) retained_busy_files: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::semantic) struct FlatPublishOutcome {
    pub(in crate::semantic) published: bool,
    pub(in crate::semantic) generation: u64,
    pub(in crate::semantic) generation_hash: Option<String>,
    pub(in crate::semantic) replaced_events: usize,
    pub(in crate::semantic) deleted_events: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StoreMode {
    ReadWrite,
    ReadOnly,
}

pub(in crate::semantic) struct FlatSegmentStore {
    root: PathBuf,
    contract: FlatModelContract,
    mode: StoreMode,
    validated: Mutex<Option<ValidatedGeneration>>,
    pinned: Mutex<Option<PinnedFlatGeneration>>,
    reconciliation_view: Mutex<Option<FlatReconciliationView>>,
    #[cfg(test)]
    recovery: FlatRecoveryReport,
    #[cfg(test)]
    active_event_snapshot_count: AtomicU64,
    #[cfg(test)]
    active_generation_load_count: AtomicU64,
}

struct FlatReconciliationView {
    id: String,
    lookup: FlatActiveEventLookup,
    after_event_id: Option<Uuid>,
    pending_event_page: Option<FlatReconciliationEventPage>,
}

struct FlatReconciliationEventPage {
    event_ids: Vec<Uuid>,
    after_event_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ValidatedGeneration {
    generation: u64,
    generation_hash: String,
}

impl FlatSegmentStore {
    pub(in crate::semantic) fn open(
        root: impl AsRef<Path>,
        contract: FlatModelContract,
    ) -> FlatResult<Self> {
        ensure_little_endian()?;
        validate_model_contract(&contract)?;
        let root = root.as_ref().to_path_buf();
        ensure_store_directories(&root)?;
        let store = Self {
            root,
            contract,
            mode: StoreMode::ReadWrite,
            validated: Mutex::new(None),
            pinned: Mutex::new(None),
            reconciliation_view: Mutex::new(None),
            #[cfg(test)]
            recovery: FlatRecoveryReport::default(),
            #[cfg(test)]
            active_event_snapshot_count: AtomicU64::new(0),
            #[cfg(test)]
            active_generation_load_count: AtomicU64::new(0),
        };
        let recovery = store.recover_internal()?;
        #[cfg(test)]
        let store = {
            let mut store = store;
            store.recovery = recovery;
            store
        };
        #[cfg(not(test))]
        let _ = recovery;
        Ok(store)
    }

    pub(in crate::semantic) fn open_read_only(
        root: impl AsRef<Path>,
        contract: FlatModelContract,
    ) -> FlatResult<Self> {
        ensure_little_endian()?;
        validate_model_contract(&contract)?;
        let root = root.as_ref().to_path_buf();
        validate_existing_store_directories(&root)?;
        let store = Self {
            root,
            contract,
            mode: StoreMode::ReadOnly,
            validated: Mutex::new(None),
            pinned: Mutex::new(None),
            reconciliation_view: Mutex::new(None),
            #[cfg(test)]
            recovery: FlatRecoveryReport::default(),
            #[cfg(test)]
            active_event_snapshot_count: AtomicU64::new(0),
            #[cfg(test)]
            active_generation_load_count: AtomicU64::new(0),
        };
        let _guard = store.lock_shared()?;
        if let Some(selected) = select_manifest(&store.root, &store.contract)? {
            let _ = store.load_pinned(&selected)?;
        }
        Ok(store)
    }

    #[cfg(test)]
    pub(in crate::semantic) fn recovery_report(&self) -> &FlatRecoveryReport {
        &self.recovery
    }
    pub(in crate::semantic) fn pin_generation(&self) -> FlatResult<Option<PinnedFlatGeneration>> {
        let _guard = self.lock_shared()?;
        let Some(selected) = select_manifest(&self.root, &self.contract)? else {
            self.clear_pinned()?;
            return Ok(None);
        };
        let pinned = self.load_pinned(&selected)?;
        self.remember_validated(&selected)?;
        Ok(Some(pinned))
    }

    pub(in crate::semantic) fn active_stats(&self) -> FlatResult<FlatActiveStats> {
        Ok(self
            .pin_generation()?
            .map(|pinned| pinned.stats().clone())
            .unwrap_or_default())
    }

    pub(in crate::semantic) fn active_event_lookup(&self) -> FlatResult<FlatActiveEventLookup> {
        if let Some(lookup) = self.reconciliation_lookup()? {
            return Ok(lookup);
        }
        #[cfg(test)]
        self.active_event_snapshot_count
            .fetch_add(1, Ordering::Relaxed);
        Ok(FlatActiveEventLookup {
            pinned: self.pin_generation()?,
        })
    }

    /// Retains one immutable flat generation for a complete reconciliation.
    ///
    /// The source projection publishes at most one delta per bounded Core
    /// page and persists its frontier after each page. Consequently a crash
    /// or restart can retain no more durable deltas than the pages in that one
    /// reconciliation; releasing the view runs exact threshold compaction.
    pub(in crate::semantic) fn begin_reconciliation_view(&self, id: &str) -> FlatResult<()> {
        if id.is_empty() {
            return Err(FlatStoreError::InvalidInput(
                "flat reconciliation view id cannot be empty".to_owned(),
            ));
        }
        let replace_existing = {
            let view = self.reconciliation_view.lock().map_err(|_| {
                FlatStoreError::Corrupt("flat reconciliation view lock is poisoned".to_owned())
            })?;
            match view.as_ref() {
                Some(view) if view.id == id => return Ok(()),
                Some(_) => true,
                None => false,
            }
        };
        if replace_existing {
            self.finish_reconciliation_view()?;
        }

        let pinned = self.pin_generation()?;
        let lookup = FlatActiveEventLookup { pinned };
        #[cfg(test)]
        self.active_event_snapshot_count
            .fetch_add(1, Ordering::Relaxed);
        let mut view = self.reconciliation_view.lock().map_err(|_| {
            FlatStoreError::Corrupt("flat reconciliation view lock is poisoned".to_owned())
        })?;
        *view = Some(FlatReconciliationView {
            id: id.to_owned(),
            lookup,
            after_event_id: None,
            pending_event_page: None,
        });
        Ok(())
    }

    pub(in crate::semantic) fn reconciliation_event_ids(
        &self,
        id: &str,
        limit: usize,
    ) -> FlatResult<Vec<Uuid>> {
        if limit == 0 {
            return Err(FlatStoreError::InvalidInput(
                "flat reconciliation event page limit cannot be zero".to_owned(),
            ));
        }
        let mut current = self.reconciliation_view.lock().map_err(|_| {
            FlatStoreError::Corrupt("flat reconciliation view lock is poisoned".to_owned())
        })?;
        let view = current
            .as_mut()
            .filter(|view| view.id == id)
            .ok_or_else(|| {
                FlatStoreError::InvalidInput(
                    "flat reconciliation event page has no matching view".to_owned(),
                )
            })?;
        if let Some(pending) = view.pending_event_page.as_ref() {
            return Ok(pending.event_ids.clone());
        }
        let Some(pinned) = view.lookup.pinned.as_ref() else {
            return Ok(Vec::new());
        };
        let events = pinned.active_events();
        let start = view.after_event_id.map_or(0, |after| {
            events.partition_point(|event| event.event_id <= after)
        });
        let event_ids = events[start..]
            .iter()
            .take(limit)
            .map(|event| event.event_id)
            .collect::<Vec<_>>();
        if let Some(after_event_id) = event_ids.last().copied() {
            view.pending_event_page = Some(FlatReconciliationEventPage {
                event_ids: event_ids.clone(),
                after_event_id,
            });
        }
        Ok(event_ids)
    }

    pub(in crate::semantic) fn finish_reconciliation_view(&self) -> FlatResult<()> {
        let retained = {
            let mut current = self.reconciliation_view.lock().map_err(|_| {
                FlatStoreError::Corrupt("flat reconciliation view lock is poisoned".to_owned())
            })?;
            current.take()
        };
        let Some(retained) = retained else {
            return Ok(());
        };
        if let Err(error) = self.compact_if_needed() {
            let mut current = self.reconciliation_view.lock().map_err(|_| {
                FlatStoreError::Corrupt("flat reconciliation view lock is poisoned".to_owned())
            })?;
            if current.is_none() {
                *current = Some(retained);
            }
            return Err(error);
        }
        Ok(())
    }

    pub(in crate::semantic) fn compact_if_needed(&self) -> FlatResult<()> {
        let stats = self.active_stats()?;
        if stats.segment_count >= COMPACT_SEGMENT_THRESHOLD
            || (stats.active_chunks > 0
                && stats.stored_chunks > (stats.active_chunks as u64).saturating_mul(2))
        {
            let _ = self.compact()?;
        }
        Ok(())
    }

    pub(in crate::semantic) fn reconciliation_active(&self) -> FlatResult<bool> {
        let view = self.reconciliation_view.lock().map_err(|_| {
            FlatStoreError::Corrupt("flat reconciliation view lock is poisoned".to_owned())
        })?;
        Ok(view.is_some())
    }

    fn reconciliation_lookup(&self) -> FlatResult<Option<FlatActiveEventLookup>> {
        let view = self.reconciliation_view.lock().map_err(|_| {
            FlatStoreError::Corrupt("flat reconciliation view lock is poisoned".to_owned())
        })?;
        Ok(view.as_ref().map(|view| view.lookup.clone()))
    }

    #[cfg(test)]
    pub(in crate::semantic) fn active_hash(&self) -> FlatResult<Option<String>> {
        Ok(self
            .pin_generation()?
            .map(|pinned| pinned.generation_hash().to_owned()))
    }

    #[cfg(test)]
    pub(in crate::semantic) fn reset_active_event_snapshot_count(&self) {
        self.active_event_snapshot_count.store(0, Ordering::Relaxed);
        self.active_generation_load_count
            .store(0, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(in crate::semantic) fn active_event_snapshot_count(&self) -> u64 {
        self.active_event_snapshot_count.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(in crate::semantic) fn active_generation_load_count(&self) -> u64 {
        self.active_generation_load_count.load(Ordering::Relaxed)
    }

    pub(in crate::semantic) fn publish_replacement_event_chunks(
        &self,
        replacements: &[FlatEventReplacement],
        tombstones: &[Uuid],
    ) -> FlatResult<FlatPublishOutcome> {
        self.require_writable()?;
        validate_publication_input(&self.contract, replacements, tombstones)?;
        let _guard = self.lock_exclusive()?;
        let current = self.load_current_locked()?;
        if replacements.is_empty() && tombstones.is_empty() {
            return Ok(noop_outcome(current.as_ref()));
        }
        let generation = next_generation(current.as_ref())?;
        let staged = write_replacement_segment(
            &self.root,
            &self.contract,
            generation,
            replacements,
            tombstones,
        )?;
        sync_directory(&segments_directory(&self.root))?;
        validate_staged_segment(&self.root, &self.contract, &staged.descriptor)?;

        let mut manifest = current
            .as_ref()
            .map(|selected| selected.envelope.manifest.clone())
            .unwrap_or_else(|| Manifest::new(self.contract.clone()));
        manifest.generation = generation;
        manifest.created_unix_millis = unix_millis();
        manifest.segments.push(staged.descriptor);
        let selected = publish_manifest(&self.root, manifest)?;
        self.remember_validated(&selected)?;
        self.record_reconciliation_publication(tombstones)?;
        self.clear_pinned()?;
        let _ = cleanup_obsolete_locked(&self.root, &selected);
        Ok(FlatPublishOutcome {
            published: true,
            generation,
            generation_hash: Some(selected.generation_hash),
            replaced_events: replacements.len(),
            deleted_events: tombstones.len(),
        })
    }

    fn load_current_locked(&self) -> FlatResult<Option<SelectedManifest>> {
        let selected = select_manifest(&self.root, &self.contract)?;
        if let Some(selected) = &selected {
            if !self.is_validated(selected)? {
                let _ = self.load_pinned(selected)?;
            }
        }
        Ok(selected)
    }

    fn load_pinned(&self, selected: &SelectedManifest) -> FlatResult<PinnedFlatGeneration> {
        {
            let guard = self.pinned.lock().map_err(|_| {
                FlatStoreError::Corrupt("flat pinned generation cache lock is poisoned".to_owned())
            })?;
            if let Some(pinned) = guard.as_ref().filter(|pinned| {
                pinned.generation() == selected.envelope.manifest.generation
                    && pinned.generation_hash() == selected.generation_hash
            }) {
                return Ok(pinned.clone());
            }
        }
        #[cfg(test)]
        self.active_generation_load_count
            .fetch_add(1, Ordering::Relaxed);
        let pinned = load_pinned_generation(&self.root, selected)?;
        self.remember_validated(selected)?;
        let mut guard = self.pinned.lock().map_err(|_| {
            FlatStoreError::Corrupt("flat pinned generation cache lock is poisoned".to_owned())
        })?;
        *guard = Some(pinned.clone());
        Ok(pinned)
    }

    fn record_reconciliation_publication(&self, tombstones: &[Uuid]) -> FlatResult<()> {
        let mut current = self.reconciliation_view.lock().map_err(|_| {
            FlatStoreError::Corrupt("flat reconciliation view lock is poisoned".to_owned())
        })?;
        let Some(view) = current.as_mut() else {
            return Ok(());
        };
        let tombstones = tombstones.iter().copied().collect::<HashSet<_>>();
        if view.pending_event_page.as_ref().is_some_and(|pending| {
            pending.event_ids.len() == tombstones.len()
                && pending
                    .event_ids
                    .iter()
                    .all(|event_id| tombstones.contains(event_id))
        }) {
            let pending = view.pending_event_page.take().ok_or_else(|| {
                FlatStoreError::Corrupt("flat reconciliation event page was lost".to_owned())
            })?;
            view.after_event_id = Some(pending.after_event_id);
        }
        Ok(())
    }

    fn is_validated(&self, selected: &SelectedManifest) -> FlatResult<bool> {
        let guard = self.validated.lock().map_err(|_| {
            FlatStoreError::Corrupt("flat validation cache lock is poisoned".to_owned())
        })?;
        Ok(guard.as_ref().is_some_and(|validated| {
            validated.generation == selected.envelope.manifest.generation
                && validated.generation_hash == selected.generation_hash
        }))
    }

    fn remember_validated(&self, selected: &SelectedManifest) -> FlatResult<()> {
        let mut guard = self.validated.lock().map_err(|_| {
            FlatStoreError::Corrupt("flat validation cache lock is poisoned".to_owned())
        })?;
        *guard = Some(ValidatedGeneration {
            generation: selected.envelope.manifest.generation,
            generation_hash: selected.generation_hash.clone(),
        });
        Ok(())
    }

    fn clear_validated(&self) -> FlatResult<()> {
        let mut guard = self.validated.lock().map_err(|_| {
            FlatStoreError::Corrupt("flat validation cache lock is poisoned".to_owned())
        })?;
        *guard = None;
        Ok(())
    }

    fn clear_pinned(&self) -> FlatResult<()> {
        let mut guard = self.pinned.lock().map_err(|_| {
            FlatStoreError::Corrupt("flat pinned generation cache lock is poisoned".to_owned())
        })?;
        *guard = None;
        Ok(())
    }

    fn require_writable(&self) -> FlatResult<()> {
        if self.mode == StoreMode::ReadOnly {
            return Err(FlatStoreError::ReadOnly);
        }
        Ok(())
    }

    fn lock_shared(&self) -> FlatResult<FileLock> {
        FileLock::shared(&lock_path(&self.root))
    }

    fn lock_exclusive(&self) -> FlatResult<FileLock> {
        FileLock::exclusive(&lock_path(&self.root))
    }
}

struct FileLock {
    file: File,
}

impl FileLock {
    fn shared(path: &Path) -> FlatResult<Self> {
        let file = open_lock(path, false)?;
        fs2::FileExt::lock_shared(&file).map_err(|source| io_error("lock shared", path, source))?;
        Ok(Self { file })
    }

    fn exclusive(path: &Path) -> FlatResult<Self> {
        let file = open_lock(path, true)?;
        fs2::FileExt::lock_exclusive(&file)
            .map_err(|source| io_error("lock exclusive", path, source))?;
        Ok(Self { file })
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

fn open_lock(path: &Path, create: bool) -> FlatResult<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    if create {
        options.write(true).create(true);
    }
    options
        .open(path)
        .map_err(|source| io_error("open flat writer lock", path, source))
}

#[cfg(test)]
mod tests;
