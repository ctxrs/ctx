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
mod catalog;
mod manifest;
mod pinned;
mod recovery;
mod validation;

use artifacts::*;
use catalog::*;
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
    #[error("legacy flat F32 manifest schema {0} requires rebuild")]
    LegacySchema(u32),
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

#[derive(Debug, Clone)]
pub(in crate::semantic) struct FlatEventMetadataUpdate {
    pub(in crate::semantic) event_id: Uuid,
    pub(in crate::semantic) seq: u64,
    pub(in crate::semantic) source_text_hash: FlatSourceHash,
    pub(in crate::semantic) stable_identity_hash: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::semantic) struct FlatActiveEvent {
    pub(in crate::semantic) event_id: Uuid,
    pub(in crate::semantic) seq: u64,
    pub(in crate::semantic) source_text_hash: FlatSourceHash,
    pub(in crate::semantic) chunk_count: u32,
    pub(in crate::semantic) source_identity_digest: String,
    pub(in crate::semantic) source_reconciliation_id: String,
    pub(in crate::semantic) stable_identity_hash: [u8; 32],
    vector_generation: u64,
    first_vector_ordinal: u64,
}

/// Read-only event lookup bound to one exact flat generation.
///
/// Generation loading stores active event summaries in UUID order, so callers
/// can probe this shared pin without cloning or linearly scanning the active
/// corpus for every event.
#[derive(Clone)]
pub(in crate::semantic) struct FlatActiveEventLookup {
    events: Arc<Vec<FlatActiveEvent>>,
}

impl FlatActiveEventLookup {
    pub(in crate::semantic) fn event(&self, event_id: Uuid) -> Option<&FlatActiveEvent> {
        self.events
            .binary_search_by_key(&event_id, |event| event.event_id)
            .ok()
            .map(|index| &self.events[index])
    }

    pub(in crate::semantic) fn events(&self) -> &[FlatActiveEvent] {
        &self.events
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(in crate::semantic) struct FlatWorkStats {
    pub(in crate::semantic) vectors_touched: u64,
    pub(in crate::semantic) vector_bytes_touched: u64,
    pub(in crate::semantic) metadata_records_touched: u64,
}

impl FlatWorkStats {
    fn saturating_delta(self, earlier: Self) -> Self {
        Self {
            vectors_touched: self.vectors_touched.saturating_sub(earlier.vectors_touched),
            vector_bytes_touched: self
                .vector_bytes_touched
                .saturating_sub(earlier.vector_bytes_touched),
            metadata_records_touched: self
                .metadata_records_touched
                .saturating_sub(earlier.metadata_records_touched),
        }
    }
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
    pinned: Mutex<Option<PinnedFlatGeneration>>,
    reconciliation_view: Mutex<Option<FlatReconciliationView>>,
    vectors_touched: AtomicU64,
    vector_bytes_touched: AtomicU64,
    metadata_records_touched: AtomicU64,
    #[cfg(test)]
    recovery: FlatRecoveryReport,
    #[cfg(test)]
    active_event_snapshot_count: AtomicU64,
    #[cfg(test)]
    active_generation_load_count: AtomicU64,
}

struct FlatReconciliationView {
    id: String,
    source: Option<FlatSourceScope>,
    lookup: FlatActiveEventLookup,
    after_event_id: Option<Uuid>,
    pending_event_page: Option<FlatReconciliationEventPage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FlatSourceScope {
    source_identity_digest: String,
    source_reconciliation_id: String,
}

struct FlatReconciliationEventPage {
    event_ids: Vec<Uuid>,
    after_event_id: Uuid,
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
            pinned: Mutex::new(None),
            reconciliation_view: Mutex::new(None),
            vectors_touched: AtomicU64::new(0),
            vector_bytes_touched: AtomicU64::new(0),
            metadata_records_touched: AtomicU64::new(0),
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
            pinned: Mutex::new(None),
            reconciliation_view: Mutex::new(None),
            vectors_touched: AtomicU64::new(0),
            vector_bytes_touched: AtomicU64::new(0),
            metadata_records_touched: AtomicU64::new(0),
            #[cfg(test)]
            recovery: FlatRecoveryReport::default(),
            #[cfg(test)]
            active_event_snapshot_count: AtomicU64::new(0),
            #[cfg(test)]
            active_generation_load_count: AtomicU64::new(0),
        };
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
        Ok(Some(pinned))
    }

    pub(in crate::semantic) fn active_stats(&self) -> FlatResult<FlatActiveStats> {
        let _guard = self.lock_shared()?;
        Ok(select_manifest(&self.root, &self.contract)?
            .as_ref()
            .map(manifest_stats)
            .unwrap_or_default())
    }

    pub(in crate::semantic) fn active_event_lookup(&self) -> FlatResult<FlatActiveEventLookup> {
        if let Some(lookup) = self.reconciliation_lookup()? {
            return Ok(lookup);
        }
        let _guard = self.lock_shared()?;
        let events = match select_manifest(&self.root, &self.contract)? {
            Some(selected) => {
                let (events, touched) = load_active_events(
                    &self.root,
                    &self.contract,
                    &selected.envelope.manifest,
                    None,
                )?;
                self.touch_metadata(touched);
                events
            }
            None => Arc::new(Vec::new()),
        };
        self.record_active_event_snapshot();
        Ok(FlatActiveEventLookup { events })
    }

    /// Retains one immutable flat generation for a complete reconciliation.
    ///
    /// The source projection publishes at most one delta per bounded Core
    /// page and persists its frontier after each page. Consequently a crash
    /// or restart can retain no more durable deltas than the pages in that one
    /// reconciliation; releasing the view runs exact threshold compaction.
    pub(in crate::semantic) fn begin_reconciliation_view(&self, id: &str) -> FlatResult<()> {
        self.begin_reconciliation_view_inner(id, None)
    }

    pub(in crate::semantic) fn begin_source_reconciliation_view(
        &self,
        source_identity_digest: &str,
        source_reconciliation_id: &str,
    ) -> FlatResult<()> {
        let source = FlatSourceScope {
            source_identity_digest: source_identity_digest.to_owned(),
            source_reconciliation_id: source_reconciliation_id.to_owned(),
        };
        self.begin_reconciliation_view_inner(source_reconciliation_id, Some(source))
    }

    fn begin_reconciliation_view_inner(
        &self,
        id: &str,
        source: Option<FlatSourceScope>,
    ) -> FlatResult<()> {
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
                Some(view) if view.id == id && view.source == source => return Ok(()),
                Some(_) => true,
                None => false,
            }
        };
        if replace_existing {
            self.finish_reconciliation_view()?;
        }

        let _guard = self.lock_shared()?;
        let events = match select_manifest(&self.root, &self.contract)? {
            Some(selected) => {
                let (events, touched) = load_active_events(
                    &self.root,
                    &self.contract,
                    &selected.envelope.manifest,
                    source
                        .as_ref()
                        .map(|scope| scope.source_identity_digest.as_str()),
                )?;
                self.touch_metadata(touched);
                events
            }
            None => Arc::new(Vec::new()),
        };
        self.record_active_event_snapshot();
        let mut view = self.reconciliation_view.lock().map_err(|_| {
            FlatStoreError::Corrupt("flat reconciliation view lock is poisoned".to_owned())
        })?;
        *view = Some(FlatReconciliationView {
            id: id.to_owned(),
            source,
            lookup: FlatActiveEventLookup { events },
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
        let events = view.lookup.events();
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
        let finish = match retained.source.as_ref() {
            Some(source) => self
                .publish_source_snapshot(source)
                .and_then(|()| self.compact_source_if_needed(source)),
            None => self.compact().map(|_| ()),
        };
        if let Err(error) = finish {
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

    fn full_reconciliation_lookup(&self) -> FlatResult<Option<FlatActiveEventLookup>> {
        let view = self.reconciliation_view.lock().map_err(|_| {
            FlatStoreError::Corrupt("flat reconciliation view lock is poisoned".to_owned())
        })?;
        Ok(view
            .as_ref()
            .filter(|view| view.source.is_none())
            .map(|view| view.lookup.clone()))
    }

    #[cfg(test)]
    pub(in crate::semantic) fn active_hash(&self) -> FlatResult<Option<String>> {
        let _guard = self.lock_shared()?;
        Ok(select_manifest(&self.root, &self.contract)?.map(|selected| selected.generation_hash))
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
        let source = self.current_source_scope()?;
        let (existing, touched) = match self.full_reconciliation_lookup()? {
            Some(lookup) => (lookup, 0),
            None => self.load_source_events(current.as_ref(), &source)?,
        };
        self.touch_metadata(touched);
        let generation = next_generation(current.as_ref())?;
        let staged = write_replacement_segment(
            &self.root,
            &self.contract,
            generation,
            &source,
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
        apply_publication_counts(&mut manifest, &existing, replacements, tombstones)?;
        if source.source_identity_digest != UNSCOPED_SOURCE_IDENTITY {
            manifest.segments.retain(|segment| {
                segment.source_identity_digest != UNSCOPED_SOURCE_IDENTITY
                    || segment.vector_count != 0
                    || segment.mutation_count != 0
            });
        }
        manifest.segments.push(staged.descriptor);
        let selected = publish_manifest(&self.root, manifest)?;
        self.record_reconciliation_publication(tombstones)?;
        self.clear_pinned()?;
        self.touch_vectors(replacements)?;
        self.touch_metadata(
            u64::try_from(replacements.len() + tombstones.len()).unwrap_or(u64::MAX),
        );
        let _ = cleanup_obsolete_locked(&self.root, &selected);
        Ok(FlatPublishOutcome {
            published: true,
            generation,
            generation_hash: Some(selected.generation_hash),
            replaced_events: replacements.len(),
            deleted_events: tombstones.len(),
        })
    }

    pub(in crate::semantic) fn publish_event_metadata_updates(
        &self,
        updates: &[FlatEventMetadataUpdate],
    ) -> FlatResult<FlatPublishOutcome> {
        self.require_writable()?;
        if updates.is_empty() {
            let _guard = self.lock_shared()?;
            let current = select_manifest(&self.root, &self.contract)?;
            return Ok(noop_outcome(current.as_ref()));
        }
        let mut ids = HashSet::with_capacity(updates.len());
        if updates.iter().any(|update| !ids.insert(update.event_id)) {
            return Err(FlatStoreError::InvalidInput(
                "metadata update repeats an event".to_owned(),
            ));
        }
        let _guard = self.lock_exclusive()?;
        let current = self.load_current_locked()?;
        let source = self.current_source_scope()?;
        let (existing, touched) = self.load_source_events(current.as_ref(), &source)?;
        self.touch_metadata(touched);
        let mutations = updates
            .iter()
            .map(|update| {
                let prior = existing.event(update.event_id).ok_or_else(|| {
                    FlatStoreError::InvalidInput(format!(
                        "metadata update references absent event {}",
                        update.event_id
                    ))
                })?;
                if prior.source_text_hash != update.source_text_hash {
                    return Err(FlatStoreError::InvalidInput(format!(
                        "metadata-only update changes source hash for {}",
                        update.event_id
                    )));
                }
                Ok(EventMutation {
                    event_id: update.event_id,
                    kind: MutationKind::Replace,
                    seq: update.seq,
                    source_text_hash: update.source_text_hash,
                    stable_identity_hash: update.stable_identity_hash,
                    vector_generation: prior.vector_generation,
                    first_vector_ordinal: prior.first_vector_ordinal,
                    chunk_count: prior.chunk_count,
                })
            })
            .collect::<FlatResult<Vec<_>>>()?;
        let generation = next_generation(current.as_ref())?;
        let staged = write_catalog_segment(
            &self.root,
            &self.contract,
            generation,
            &source,
            SegmentKind::Delta,
            &mutations,
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
        self.clear_pinned()?;
        self.touch_metadata(u64::try_from(updates.len()).unwrap_or(u64::MAX));
        let _ = cleanup_obsolete_locked(&self.root, &selected);
        Ok(FlatPublishOutcome {
            published: true,
            generation,
            generation_hash: Some(selected.generation_hash),
            replaced_events: updates.len(),
            deleted_events: 0,
        })
    }

    pub(in crate::semantic) fn source_event_lookup(
        &self,
        source_identity_digest: &str,
    ) -> FlatResult<FlatActiveEventLookup> {
        let _guard = self.lock_shared()?;
        let Some(current) = select_manifest(&self.root, &self.contract)? else {
            return Ok(FlatActiveEventLookup {
                events: Arc::new(Vec::new()),
            });
        };
        let (events, touched) = load_active_events(
            &self.root,
            &self.contract,
            &current.envelope.manifest,
            Some(source_identity_digest),
        )?;
        self.touch_metadata(touched);
        Ok(FlatActiveEventLookup { events })
    }

    pub(in crate::semantic) fn source_event_ids_except_reconciliation(
        &self,
        source_identity_digest: &str,
        reconciliation_id: Option<&str>,
        limit: usize,
    ) -> FlatResult<Vec<Uuid>> {
        if limit == 0 {
            return Err(FlatStoreError::InvalidInput(
                "source event page limit cannot be zero".to_owned(),
            ));
        }
        let lookup = self.source_event_lookup(source_identity_digest)?;
        Ok(lookup
            .events()
            .iter()
            .filter(|event| {
                reconciliation_id.is_none_or(|reconciliation_id| {
                    event.source_reconciliation_id != reconciliation_id
                })
            })
            .take(limit)
            .map(|event| event.event_id)
            .collect())
    }

    pub(in crate::semantic) fn work_stats(&self) -> FlatWorkStats {
        FlatWorkStats {
            vectors_touched: self.vectors_touched.load(Ordering::Relaxed),
            vector_bytes_touched: self.vector_bytes_touched.load(Ordering::Relaxed),
            metadata_records_touched: self.metadata_records_touched.load(Ordering::Relaxed),
        }
    }

    pub(in crate::semantic) fn work_since(&self, earlier: FlatWorkStats) -> FlatWorkStats {
        self.work_stats().saturating_delta(earlier)
    }

    fn current_source_scope(&self) -> FlatResult<FlatSourceScope> {
        let view = self.reconciliation_view.lock().map_err(|_| {
            FlatStoreError::Corrupt("flat reconciliation view lock is poisoned".to_owned())
        })?;
        Ok(view
            .as_ref()
            .and_then(|view| view.source.clone())
            .unwrap_or_else(unscoped_source))
    }

    fn load_source_events(
        &self,
        current: Option<&SelectedManifest>,
        source: &FlatSourceScope,
    ) -> FlatResult<(FlatActiveEventLookup, u64)> {
        let Some(current) = current else {
            return Ok((
                FlatActiveEventLookup {
                    events: Arc::new(Vec::new()),
                },
                0,
            ));
        };
        let (events, touched) = load_active_events(
            &self.root,
            &self.contract,
            &current.envelope.manifest,
            Some(&source.source_identity_digest),
        )?;
        Ok((FlatActiveEventLookup { events }, touched))
    }

    fn publish_source_snapshot(&self, source: &FlatSourceScope) -> FlatResult<()> {
        self.require_writable()?;
        let _guard = self.lock_exclusive()?;
        let Some(current) = self.load_current_locked()? else {
            return Ok(());
        };
        let (events, touched) = self.load_source_events(Some(&current), source)?;
        self.touch_metadata(touched);
        let mutations = events
            .events()
            .iter()
            .map(event_mutation)
            .collect::<Vec<_>>();
        let snapshot_source = if events.events().is_empty() {
            unscoped_source()
        } else {
            source.clone()
        };
        let generation = next_generation(Some(&current))?;
        let staged = write_catalog_segment(
            &self.root,
            &self.contract,
            generation,
            &snapshot_source,
            SegmentKind::Base,
            &mutations,
        )?;
        sync_directory(&segments_directory(&self.root))?;
        validate_staged_segment(&self.root, &self.contract, &staged.descriptor)?;
        let mut manifest = current.envelope.manifest.clone();
        manifest.generation = generation;
        manifest.created_unix_millis = unix_millis();
        if events.events().is_empty() {
            manifest.segments.retain(|segment| {
                segment.source_identity_digest != source.source_identity_digest
                    && (segment.source_identity_digest != UNSCOPED_SOURCE_IDENTITY
                        || segment.vector_count != 0
                        || segment.mutation_count != 0)
            });
            remove_source_snapshot(&mut manifest, &source.source_identity_digest);
        } else {
            manifest.segments.retain(|segment| {
                segment.source_identity_digest != source.source_identity_digest
                    || segment.vector_count != 0
            });
        }
        manifest.segments.push(staged.descriptor);
        if !events.events().is_empty() {
            set_source_snapshot(&mut manifest, &source.source_identity_digest, generation);
        }
        let selected = publish_manifest(&self.root, manifest)?;
        self.clear_pinned()?;
        self.touch_metadata(u64::try_from(mutations.len()).unwrap_or(u64::MAX));
        let _ = cleanup_obsolete_locked(&self.root, &selected);
        Ok(())
    }

    fn touch_vectors(&self, replacements: &[FlatEventReplacement]) -> FlatResult<()> {
        let vectors = replacements.iter().try_fold(0_u64, |total, replacement| {
            total
                .checked_add(u64::try_from(replacement.chunks.len()).map_err(|_| {
                    FlatStoreError::InvalidInput("replacement chunk count is too large".to_owned())
                })?)
                .ok_or_else(|| {
                    FlatStoreError::InvalidInput("vector touch count overflow".to_owned())
                })
        })?;
        let bytes = vectors
            .checked_mul(u64::from(self.contract.dimensions))
            .and_then(|value| value.checked_mul(4))
            .ok_or_else(|| {
                FlatStoreError::InvalidInput("vector touch bytes overflow".to_owned())
            })?;
        self.vectors_touched.fetch_add(vectors, Ordering::Relaxed);
        self.vector_bytes_touched
            .fetch_add(bytes, Ordering::Relaxed);
        Ok(())
    }

    fn touch_metadata(&self, records: u64) {
        self.metadata_records_touched
            .fetch_add(records, Ordering::Relaxed);
    }

    fn record_active_event_snapshot(&self) {
        #[cfg(test)]
        self.active_event_snapshot_count
            .fetch_add(1, Ordering::Relaxed);
    }

    fn load_current_locked(&self) -> FlatResult<Option<SelectedManifest>> {
        select_manifest(&self.root, &self.contract)
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
