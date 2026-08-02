use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write},
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
mod source_compaction;
mod source_reconciliation;
mod source_stage_log;
mod source_staging;
mod validation;

use artifacts::*;
use catalog::*;
use manifest::*;
use pinned::load_pinned_generation;
pub(in crate::semantic) use pinned::PinnedFlatGeneration;
#[cfg(test)]
pub(in crate::semantic) use pinned::PinnedScanSegment;
use recovery::*;
use source_compaction::*;
use source_stage_log::*;
use source_staging::*;
use validation::*;

pub(in crate::semantic) type FlatResult<T> = std::result::Result<T, FlatStoreError>;

const COMPACT_SEGMENT_THRESHOLD: usize = 16;
const FLAT_SOURCE_RECEIPT_DOMAIN: &[u8] = b"ctx-flat-source-receipt-v1\0";

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::semantic) struct FlatSourceReceipt {
    pub(in crate::semantic) source_identity_digest: String,
    pub(in crate::semantic) source_reconciliation_id: String,
    pub(in crate::semantic) indexed_documents: u64,
    pub(in crate::semantic) semantic_eligible_documents: u64,
    pub(in crate::semantic) core_record_accumulator: String,
    pub(in crate::semantic) contract_fingerprint: String,
    pub(in crate::semantic) semantic_policy_fingerprint: String,
    pub(in crate::semantic) owned_event_count: u64,
    pub(in crate::semantic) owned_event_ids_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::semantic) struct FlatSourceReceiptInput {
    pub(in crate::semantic) source_identity_digest: String,
    pub(in crate::semantic) source_reconciliation_id: String,
    pub(in crate::semantic) indexed_documents: u64,
    pub(in crate::semantic) semantic_eligible_documents: u64,
    pub(in crate::semantic) core_record_accumulator: String,
    pub(in crate::semantic) contract_fingerprint: String,
    pub(in crate::semantic) semantic_policy_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::semantic) struct FlatSourceState {
    pub(in crate::semantic) source_identity_digest: String,
    pub(in crate::semantic) receipt: Option<FlatSourceReceipt>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::semantic) struct FlatPublicationToken {
    pub(in crate::semantic) generation: u64,
    pub(in crate::semantic) generation_hash: Option<String>,
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
    pub(in crate::semantic) fn from_events(mut events: Vec<FlatActiveEvent>) -> Self {
        events.sort_by_key(|event| event.event_id);
        Self {
            events: Arc::new(events),
        }
    }

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

pub(in crate::semantic) struct FlatSourceFinalization {
    pub(in crate::semantic) publication: FlatPublishOutcome,
    pub(in crate::semantic) receipt: Option<FlatSourceReceipt>,
    pub(in crate::semantic) deleted_chunks: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::semantic) struct FlatSourceStagingToken {
    pub(in crate::semantic) source_reconciliation_id: String,
    pub(in crate::semantic) page_sequence: u64,
    pub(in crate::semantic) page_hash: String,
}

pub(in crate::semantic) struct FlatSourcePageOutcome {
    pub(in crate::semantic) staging: FlatSourceStagingToken,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::semantic) enum FlatSourceStageResume {
    Ready,
    Restarted,
}

impl FlatPublishOutcome {
    pub(in crate::semantic) fn token(&self) -> FlatPublicationToken {
        FlatPublicationToken {
            generation: self.generation,
            generation_hash: self.generation_hash.clone(),
        }
    }
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
    source_generation: Mutex<Option<FlatSourceGenerationView>>,
    source_stage: Mutex<Option<FlatSourceStage>>,
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
    #[cfg(test)]
    source_catalog_load_count: AtomicU64,
    #[cfg(test)]
    source_catalog_records_replayed: AtomicU64,
    #[cfg(test)]
    source_publication_count: AtomicU64,
    #[cfg(test)]
    global_manifest_parse_count: AtomicU64,
    #[cfg(test)]
    global_manifest_serialization_count: AtomicU64,
    #[cfg(test)]
    global_segment_directory_scan_count: AtomicU64,
    #[cfg(test)]
    staging_peak_event_records: AtomicU64,
    #[cfg(test)]
    fail_after_source_frontier_commit: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    fail_after_source_finalization: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    fail_after_source_publication_commit: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    fail_after_source_acknowledgement: std::sync::atomic::AtomicBool,
}

struct FlatSourceGenerationView {
    _transaction_lock: FileLock,
    selected: Option<SelectedManifest>,
}

#[derive(Clone)]
struct FlatReconciliationView {
    id: String,
    source: Option<FlatSourceScope>,
    lookup: FlatActiveEventLookup,
    updates: HashMap<Uuid, Option<FlatActiveEvent>>,
    after_event_id: Option<Uuid>,
    pending_event_page: Option<FlatReconciliationEventPage>,
    retirement_event_ids: Option<Vec<Uuid>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FlatSourceScope {
    source_identity_digest: String,
    source_reconciliation_id: String,
}

#[derive(Clone)]
struct FlatReconciliationEventPage {
    event_ids: Vec<Uuid>,
    after_event_id: Uuid,
}

impl FlatReconciliationView {
    fn current_events(&self) -> Vec<FlatActiveEvent> {
        let mut events = self
            .lookup
            .events()
            .iter()
            .filter(|event| !self.updates.contains_key(&event.event_id))
            .cloned()
            .chain(self.updates.values().filter_map(Clone::clone))
            .collect::<Vec<_>>();
        events.sort_by_key(|event| event.event_id);
        events
    }

    fn apply_publication(&mut self, staged: &StagedSegment) {
        for mutation in &staged.mutations {
            match mutation.kind {
                MutationKind::Delete => {
                    self.updates.insert(mutation.event_id, None);
                }
                MutationKind::Replace => {
                    self.updates.insert(
                        mutation.event_id,
                        Some(FlatActiveEvent {
                            event_id: mutation.event_id,
                            seq: mutation.seq,
                            source_text_hash: mutation.source_text_hash,
                            chunk_count: mutation.chunk_count,
                            source_identity_digest: staged
                                .descriptor
                                .source_identity_digest
                                .clone(),
                            source_reconciliation_id: staged
                                .descriptor
                                .source_reconciliation_id
                                .clone(),
                            stable_identity_hash: mutation.stable_identity_hash,
                            vector_generation: mutation.vector_generation,
                            first_vector_ordinal: mutation.first_vector_ordinal,
                        }),
                    );
                }
            }
        }
    }
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
            source_generation: Mutex::new(None),
            source_stage: Mutex::new(None),
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
            #[cfg(test)]
            source_catalog_load_count: AtomicU64::new(0),
            #[cfg(test)]
            source_catalog_records_replayed: AtomicU64::new(0),
            #[cfg(test)]
            source_publication_count: AtomicU64::new(0),
            #[cfg(test)]
            global_manifest_parse_count: AtomicU64::new(0),
            #[cfg(test)]
            global_manifest_serialization_count: AtomicU64::new(0),
            #[cfg(test)]
            global_segment_directory_scan_count: AtomicU64::new(0),
            #[cfg(test)]
            staging_peak_event_records: AtomicU64::new(0),
            #[cfg(test)]
            fail_after_source_frontier_commit: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_after_source_finalization: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_after_source_publication_commit: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_after_source_acknowledgement: std::sync::atomic::AtomicBool::new(false),
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
            source_generation: Mutex::new(None),
            source_stage: Mutex::new(None),
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
            #[cfg(test)]
            source_catalog_load_count: AtomicU64::new(0),
            #[cfg(test)]
            source_catalog_records_replayed: AtomicU64::new(0),
            #[cfg(test)]
            source_publication_count: AtomicU64::new(0),
            #[cfg(test)]
            global_manifest_parse_count: AtomicU64::new(0),
            #[cfg(test)]
            global_manifest_serialization_count: AtomicU64::new(0),
            #[cfg(test)]
            global_segment_directory_scan_count: AtomicU64::new(0),
            #[cfg(test)]
            staging_peak_event_records: AtomicU64::new(0),
            #[cfg(test)]
            fail_after_source_frontier_commit: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_after_source_finalization: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_after_source_publication_commit: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_after_source_acknowledgement: std::sync::atomic::AtomicBool::new(false),
        };
        Ok(store)
    }

    #[cfg(test)]
    pub(in crate::semantic) fn recovery_report(&self) -> &FlatRecoveryReport {
        &self.recovery
    }
    pub(in crate::semantic) fn pin_generation(&self) -> FlatResult<Option<PinnedFlatGeneration>> {
        let generation = self.source_generation.lock().map_err(|_| {
            FlatStoreError::Corrupt("flat source generation lock is poisoned".to_owned())
        })?;
        if let Some(generation) = generation.as_ref() {
            let Some(selected) = generation.selected.as_ref() else {
                self.clear_pinned()?;
                return Ok(None);
            };
            return self.load_pinned(selected).map(Some);
        }
        drop(generation);
        let _guard = self.lock_shared()?;
        let Some(selected) = select_manifest(&self.root, &self.contract)? else {
            self.clear_pinned()?;
            return Ok(None);
        };
        let pinned = self.load_pinned(&selected)?;
        Ok(Some(pinned))
    }

    pub(in crate::semantic) fn active_stats(&self) -> FlatResult<FlatActiveStats> {
        let generation = self.source_generation.lock().map_err(|_| {
            FlatStoreError::Corrupt("flat source generation lock is poisoned".to_owned())
        })?;
        if let Some(generation) = generation.as_ref() {
            return Ok(generation
                .selected
                .as_ref()
                .map(manifest_stats)
                .unwrap_or_default());
        }
        drop(generation);
        let _guard = self.lock_shared()?;
        Ok(select_manifest(&self.root, &self.contract)?
            .as_ref()
            .map(manifest_stats)
            .unwrap_or_default())
    }

    pub(in crate::semantic) fn active_publication_token(&self) -> FlatResult<FlatPublicationToken> {
        let stats = self.active_stats()?;
        Ok(FlatPublicationToken {
            generation: stats.generation,
            generation_hash: stats.generation_hash,
        })
    }

    pub(in crate::semantic) fn source_states(&self) -> FlatResult<Vec<FlatSourceState>> {
        let generation = self.source_generation.lock().map_err(|_| {
            FlatStoreError::Corrupt("flat source generation lock is poisoned".to_owned())
        })?;
        if let Some(generation) = generation.as_ref() {
            return Ok(generation
                .selected
                .as_ref()
                .map(|selected| manifest_source_states(&selected.envelope.manifest))
                .unwrap_or_default());
        }
        drop(generation);
        let _guard = self.lock_shared()?;
        let Some(selected) = select_manifest(&self.root, &self.contract)? else {
            return Ok(Vec::new());
        };
        Ok(manifest_source_states(&selected.envelope.manifest))
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
        self.source_catalog_load_count.store(0, Ordering::Relaxed);
        self.source_catalog_records_replayed
            .store(0, Ordering::Relaxed);
        self.source_publication_count.store(0, Ordering::Relaxed);
        self.global_manifest_parse_count.store(0, Ordering::Relaxed);
        self.global_manifest_serialization_count
            .store(0, Ordering::Relaxed);
        self.global_segment_directory_scan_count
            .store(0, Ordering::Relaxed);
        self.staging_peak_event_records.store(0, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(in crate::semantic) fn active_event_snapshot_count(&self) -> u64 {
        self.active_event_snapshot_count.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(in crate::semantic) fn active_generation_load_count(&self) -> u64 {
        self.active_generation_load_count.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(in crate::semantic) fn source_catalog_load_count(&self) -> u64 {
        self.source_catalog_load_count.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(in crate::semantic) fn source_catalog_records_replayed(&self) -> u64 {
        self.source_catalog_records_replayed.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(in crate::semantic) fn source_publication_count(&self) -> u64 {
        self.source_publication_count.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(in crate::semantic) fn global_manifest_parse_count(&self) -> u64 {
        self.global_manifest_parse_count.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(in crate::semantic) fn global_manifest_serialization_count(&self) -> u64 {
        self.global_manifest_serialization_count
            .load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(in crate::semantic) fn global_segment_directory_scan_count(&self) -> u64 {
        self.global_segment_directory_scan_count
            .load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(in crate::semantic) fn staging_peak_event_records(&self) -> u64 {
        self.staging_peak_event_records.load(Ordering::Relaxed)
    }

    pub(in crate::semantic) fn begin_source_generation_view(&self) -> FlatResult<()> {
        self.require_writable()?;
        let mut generation = self.source_generation.lock().map_err(|_| {
            FlatStoreError::Corrupt("flat source generation lock is poisoned".to_owned())
        })?;
        if generation.is_some() {
            return Ok(());
        }
        let transaction_lock = self.lock_transaction()?;
        let selected = {
            let _guard = self.lock_shared()?;
            select_manifest(&self.root, &self.contract)?
        };
        #[cfg(test)]
        self.global_manifest_parse_count
            .fetch_add(1, Ordering::Relaxed);
        *generation = Some(FlatSourceGenerationView {
            _transaction_lock: transaction_lock,
            selected,
        });
        Ok(())
    }

    pub(in crate::semantic) fn end_source_generation_view(&self) -> FlatResult<()> {
        let mut stage = self.source_stage.lock().map_err(|_| {
            FlatStoreError::Corrupt("flat source stage lock is poisoned".to_owned())
        })?;
        *stage = None;
        drop(stage);
        let mut generation = self.source_generation.lock().map_err(|_| {
            FlatStoreError::Corrupt("flat source generation lock is poisoned".to_owned())
        })?;
        *generation = None;
        Ok(())
    }

    #[cfg(test)]
    pub(in crate::semantic) fn fail_after_source_frontier_commit_once(&self) {
        self.fail_after_source_frontier_commit
            .store(true, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(in crate::semantic) fn take_source_frontier_commit_failure(&self) -> bool {
        self.fail_after_source_frontier_commit
            .swap(false, Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(in crate::semantic) fn fail_after_source_finalization_once(&self) {
        self.fail_after_source_finalization
            .store(true, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(in crate::semantic) fn take_source_finalization_failure(&self) -> bool {
        self.fail_after_source_finalization
            .swap(false, Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(in crate::semantic) fn fail_after_source_acknowledgement_once(&self) {
        self.fail_after_source_acknowledgement
            .store(true, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(in crate::semantic) fn take_source_acknowledgement_failure(&self) -> bool {
        self.fail_after_source_acknowledgement
            .swap(false, Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(in crate::semantic) fn rollback_active_manifest(&self) -> FlatResult<FlatPublicationToken> {
        self.require_writable()?;
        let _transaction = self.lock_transaction()?;
        let _guard = self.lock_exclusive()?;
        let selected = select_manifest(&self.root, &self.contract)?.ok_or_else(|| {
            FlatStoreError::InvalidInput("flat manifest rollback has no publication".to_owned())
        })?;
        let token = FlatPublicationToken {
            generation: selected.envelope.manifest.generation,
            generation_hash: Some(selected.generation_hash),
        };
        fs::remove_file(&selected.path)
            .map_err(|source| io_error("roll back flat manifest", &selected.path, source))?;
        sync_directory(&manifests_directory(&self.root))?;
        self.clear_pinned()?;
        Ok(token)
    }

    pub(in crate::semantic) fn publish_replacement_event_chunks(
        &self,
        replacements: &[FlatEventReplacement],
        tombstones: &[Uuid],
    ) -> FlatResult<FlatPublishOutcome> {
        self.require_writable()?;
        validate_publication_input(&self.contract, replacements, tombstones)?;
        let _transaction = self.lock_transaction()?;
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
        manifest.segments.push(staged.descriptor.clone());
        let selected = publish_manifest(&self.root, manifest)?;
        self.record_reconciliation_publication(&staged)?;
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

    #[cfg(test)]
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

    fn lock_transaction(&self) -> FlatResult<FileLock> {
        FileLock::exclusive(&transaction_lock_path(&self.root))
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
