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
    #[cfg(test)]
    recovery: FlatRecoveryReport,
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
            #[cfg(test)]
            recovery: FlatRecoveryReport::default(),
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
            #[cfg(test)]
            recovery: FlatRecoveryReport::default(),
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

    #[cfg(test)]
    pub(in crate::semantic) fn active_hash(&self) -> FlatResult<Option<String>> {
        Ok(self
            .pin_generation()?
            .map(|pinned| pinned.generation_hash().to_owned()))
    }

    pub(in crate::semantic) fn active_events(&self) -> FlatResult<Vec<FlatActiveEvent>> {
        Ok(self
            .pin_generation()?
            .map(|pinned| pinned.active_events().to_vec())
            .unwrap_or_default())
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

    pub(in crate::semantic) fn delete_events(
        &self,
        event_ids: &[Uuid],
    ) -> FlatResult<FlatPublishOutcome> {
        self.publish_replacement_event_chunks(&[], event_ids)
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
        let pinned = load_pinned_generation(&self.root, selected)?;
        self.remember_validated(selected)?;
        let mut guard = self.pinned.lock().map_err(|_| {
            FlatStoreError::Corrupt("flat pinned generation cache lock is poisoned".to_owned())
        })?;
        *guard = Some(pinned.clone());
        Ok(pinned)
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
