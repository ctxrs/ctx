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
#[cfg(any(test, feature = "test-support"))]
mod legacy_fixture;
mod manifest;
mod pinned;
mod recovery;
mod source_compaction;
mod source_reconciliation;
mod source_stage_log;
mod source_staging;
mod validation;
#[cfg(any(test, feature = "test-support"))]
pub(crate) use legacy_fixture::seed_filter_unaware_manifest;

use artifacts::*;
use catalog::*;
use manifest::*;
use pinned::load_pinned_generation;
pub use pinned::PinnedFlatGeneration;
#[cfg(test)]
pub(crate) use pinned::PinnedScanSegment;
use recovery::*;
use source_compaction::*;
use source_stage_log::*;
use source_staging::*;
use validation::*;

pub(crate) type FlatResult<T> = std::result::Result<T, FlatStoreError>;

const COMPACT_SEGMENT_THRESHOLD: usize = 16;
const FLAT_SOURCE_RECEIPT_DOMAIN: &[u8] = b"ctx-flat-source-receipt-v1\0";
pub(crate) const MODEL_CONTRACT_RESET_PENDING_FILE: &str = "flat-model-contract-reset-pending-v1";

#[derive(Debug, Error)]
pub(crate) enum FlatStoreError {
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
pub(crate) struct FlatModelContract {
    pub(crate) contract_version: u32,
    pub(crate) model_id: String,
    pub(crate) model_revision: String,
    pub(crate) tokenizer: String,
    pub(crate) pooling: String,
    pub(crate) dimensions: u32,
    pub(crate) normalization: String,
}

impl FlatModelContract {
    pub(crate) fn validate(&self) -> FlatResult<()> {
        validate_model_contract(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct FlatSourceHash([u8; 32]);

impl FlatSourceHash {
    pub(crate) fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub(crate) fn parse_hex(value: &str) -> FlatResult<Self> {
        let bytes = decode_sha256(value).ok_or_else(|| {
            FlatStoreError::InvalidInput("source text hash must be lowercase SHA-256".to_owned())
        })?;
        Ok(Self(bytes))
    }

    pub(crate) fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub(crate) fn to_hex(self) -> String {
        encode_hex(&self.0)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct FlatChunk {
    pub(crate) chunk_index: u32,
    pub(crate) start_char: u32,
    pub(crate) end_char: u32,
    pub(crate) vector: Vec<f32>,
}

#[derive(Debug, Clone)]
pub(crate) struct FlatEventReplacement {
    pub(crate) event_id: Uuid,
    pub(crate) seq: u64,
    pub(crate) source_text_hash: FlatSourceHash,
    pub(crate) chunks: Vec<FlatChunk>,
}

#[derive(Debug, Clone)]
pub(crate) struct FlatEventMetadataUpdate {
    pub(crate) event_id: Uuid,
    pub(crate) seq: u64,
    pub(crate) source_text_hash: FlatSourceHash,
    pub(crate) stable_identity_hash: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FlatSourceReceipt {
    pub(crate) source_identity_digest: String,
    pub(crate) source_reconciliation_id: String,
    pub(crate) indexed_documents: u64,
    pub(crate) semantic_eligible_documents: u64,
    pub(crate) core_record_accumulator: String,
    pub(crate) contract_fingerprint: String,
    pub(crate) semantic_policy_fingerprint: String,
    pub(crate) owned_event_count: u64,
    #[serde(default)]
    pub(crate) filtered_event_count: u64,
    pub(crate) owned_event_ids_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FlatSourceReceiptInput {
    pub(crate) source_identity_digest: String,
    pub(crate) source_reconciliation_id: String,
    pub(crate) indexed_documents: u64,
    pub(crate) semantic_eligible_documents: u64,
    pub(crate) core_record_accumulator: String,
    pub(crate) contract_fingerprint: String,
    pub(crate) semantic_policy_fingerprint: String,
    pub(crate) filtered_event_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FlatSourceState {
    pub(crate) source_identity_digest: String,
    pub(crate) receipt: Option<FlatSourceReceipt>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FlatPublicationToken {
    pub(crate) generation: u64,
    pub(crate) generation_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FlatActiveEvent {
    pub(crate) event_id: Uuid,
    pub(crate) seq: u64,
    pub(crate) source_text_hash: FlatSourceHash,
    pub(crate) chunk_count: u32,
    pub(crate) source_identity_digest: String,
    pub(crate) source_reconciliation_id: String,
    pub(crate) stable_identity_hash: [u8; 32],
    vector_generation: u64,
    first_vector_ordinal: u64,
}

/// Read-only event lookup bound to one exact flat generation.
///
/// Generation loading stores active event summaries in UUID order, so callers
/// can probe this shared pin without cloning or linearly scanning the active
/// corpus for every event.
#[derive(Clone)]
pub(crate) struct FlatActiveEventLookup {
    events: Arc<Vec<FlatActiveEvent>>,
}

impl FlatActiveEventLookup {
    pub(crate) fn from_events(mut events: Vec<FlatActiveEvent>) -> Self {
        events.sort_by_key(|event| event.event_id);
        Self {
            events: Arc::new(events),
        }
    }

    pub(crate) fn event(&self, event_id: Uuid) -> Option<&FlatActiveEvent> {
        self.events
            .binary_search_by_key(&event_id, |event| event.event_id)
            .ok()
            .map(|index| &self.events[index])
    }

    pub(crate) fn events(&self) -> &[FlatActiveEvent] {
        &self.events
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FlatActiveStats {
    pub(crate) generation: u64,
    pub(crate) generation_hash: Option<String>,
    pub(crate) segment_count: usize,
    pub(crate) active_events: usize,
    pub(crate) active_chunks: usize,
    pub(crate) active_vector_bytes: u64,
    pub(crate) stored_chunks: u64,
    pub(crate) stored_vector_bytes: u64,
    pub(crate) deleted_events: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FlatRecoveryReport {
    pub(crate) model_contract_reset: bool,
    pub(crate) removed_temporary_files: usize,
    pub(crate) removed_obsolete_manifests: usize,
    pub(crate) removed_orphan_segments: usize,
    pub(crate) retained_busy_files: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FlatPublishOutcome {
    pub(crate) published: bool,
    pub(crate) generation: u64,
    pub(crate) generation_hash: Option<String>,
    pub(crate) replaced_events: usize,
    pub(crate) deleted_events: usize,
}

pub(crate) struct FlatSourceFinalization {
    pub(crate) publication: FlatPublishOutcome,
    pub(crate) receipt: Option<FlatSourceReceipt>,
    pub(crate) deleted_chunks: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FlatSourceStagingToken {
    pub(crate) source_reconciliation_id: String,
    pub(crate) page_sequence: u64,
    pub(crate) page_hash: String,
}

pub(crate) struct FlatSourcePageOutcome {
    pub(crate) staging: FlatSourceStagingToken,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FlatSourceStageResume {
    Ready,
    Restarted,
}

impl FlatPublishOutcome {
    pub(crate) fn token(&self) -> FlatPublicationToken {
        FlatPublicationToken {
            generation: self.generation,
            generation_hash: self.generation_hash.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct FlatWorkStats {
    pub(crate) vectors_touched: u64,
    pub(crate) vector_bytes_touched: u64,
    pub(crate) metadata_records_touched: u64,
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

pub(crate) struct FlatSegmentStore {
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

/// Coordinates one semantic control/flat snapshot with source-backed writers.
/// The lock file is part of every initialized flat store; passive callers open
/// it read-only and therefore cannot create storage artifacts.
pub(crate) struct FlatStoreCoordinationGuard {
    _lock: FileLock,
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
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn open(root: impl AsRef<Path>, contract: FlatModelContract) -> FlatResult<Self> {
        let (store, coordination) = Self::prepare_writable_open(root, contract)?;
        let store = store.finish_writable_open()?;
        drop(coordination);
        Ok(store)
    }

    pub(crate) fn prepare_writable_open(
        root: impl AsRef<Path>,
        contract: FlatModelContract,
    ) -> FlatResult<(Self, FlatStoreCoordinationGuard)> {
        ensure_little_endian()?;
        validate_model_contract(&contract)?;
        let root = root.as_ref().to_path_buf();
        ensure_store_directories(&root)?;
        let coordination = FlatStoreCoordinationGuard::lock_control_writer(&root)?;
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
        Ok((store, coordination))
    }

    pub(crate) fn finish_writable_open(self) -> FlatResult<Self> {
        let recovery = self.recover_internal_coordinated()?;
        #[cfg(test)]
        {
            let mut store = self;
            store.recovery = recovery;
            Ok(store)
        }
        #[cfg(not(test))]
        {
            let _ = recovery;
            Ok(self)
        }
    }

    pub(crate) fn open_read_only(
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
    pub(crate) fn recovery_report(&self) -> &FlatRecoveryReport {
        &self.recovery
    }
    pub(crate) fn pin_generation(&self) -> FlatResult<Option<PinnedFlatGeneration>> {
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

    pub(crate) fn active_stats(&self) -> FlatResult<FlatActiveStats> {
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

    pub(crate) fn active_publication_token(&self) -> FlatResult<FlatPublicationToken> {
        let stats = self.active_stats()?;
        Ok(FlatPublicationToken {
            generation: stats.generation,
            generation_hash: stats.generation_hash,
        })
    }

    pub(crate) fn source_states(&self) -> FlatResult<Vec<FlatSourceState>> {
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
    pub(crate) fn active_hash(&self) -> FlatResult<Option<String>> {
        let _guard = self.lock_shared()?;
        Ok(select_manifest(&self.root, &self.contract)?.map(|selected| selected.generation_hash))
    }

    #[cfg(test)]
    pub(crate) fn reset_active_event_snapshot_count(&self) {
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
    pub(crate) fn active_event_snapshot_count(&self) -> u64 {
        self.active_event_snapshot_count.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn active_generation_load_count(&self) -> u64 {
        self.active_generation_load_count.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn source_catalog_load_count(&self) -> u64 {
        self.source_catalog_load_count.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn source_catalog_records_replayed(&self) -> u64 {
        self.source_catalog_records_replayed.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn source_publication_count(&self) -> u64 {
        self.source_publication_count.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn global_manifest_parse_count(&self) -> u64 {
        self.global_manifest_parse_count.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn global_manifest_serialization_count(&self) -> u64 {
        self.global_manifest_serialization_count
            .load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn global_segment_directory_scan_count(&self) -> u64 {
        self.global_segment_directory_scan_count
            .load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn staging_peak_event_records(&self) -> u64 {
        self.staging_peak_event_records.load(Ordering::Relaxed)
    }

    pub(crate) fn begin_source_generation_view(&self) -> FlatResult<()> {
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

    pub(crate) fn end_source_generation_view(&self) -> FlatResult<()> {
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
    pub(crate) fn fail_after_source_frontier_commit_once(&self) {
        self.fail_after_source_frontier_commit
            .store(true, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn take_source_frontier_commit_failure(&self) -> bool {
        self.fail_after_source_frontier_commit
            .swap(false, Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn fail_after_source_finalization_once(&self) {
        self.fail_after_source_finalization
            .store(true, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn take_source_finalization_failure(&self) -> bool {
        self.fail_after_source_finalization
            .swap(false, Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn fail_after_source_acknowledgement_once(&self) {
        self.fail_after_source_acknowledgement
            .store(true, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn take_source_acknowledgement_failure(&self) -> bool {
        self.fail_after_source_acknowledgement
            .swap(false, Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn rollback_active_manifest(&self) -> FlatResult<FlatPublicationToken> {
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

    pub(crate) fn publish_replacement_event_chunks(
        &self,
        replacements: &[FlatEventReplacement],
        tombstones: &[Uuid],
    ) -> FlatResult<FlatPublishOutcome> {
        let _transaction = self.lock_transaction()?;
        self.publish_replacement_event_chunks_coordinated(replacements, tombstones)
    }

    /// Publishes while the caller retains `flat_transaction.lock` across a
    /// larger Flat/control handoff.
    pub(crate) fn publish_replacement_event_chunks_coordinated(
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
    pub(crate) fn source_event_lookup(
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

    pub(crate) fn work_stats(&self) -> FlatWorkStats {
        FlatWorkStats {
            vectors_touched: self.vectors_touched.load(Ordering::Relaxed),
            vector_bytes_touched: self.vector_bytes_touched.load(Ordering::Relaxed),
            metadata_records_touched: self.metadata_records_touched.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn work_since(&self, earlier: FlatWorkStats) -> FlatWorkStats {
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

impl FlatStoreCoordinationGuard {
    pub(crate) fn lock_passive_snapshot(root: &Path) -> FlatResult<Self> {
        Ok(Self {
            _lock: FileLock::shared(&transaction_lock_path(root))?,
        })
    }

    pub(crate) fn lock_control_writer(root: &Path) -> FlatResult<Self> {
        Ok(Self {
            _lock: FileLock::exclusive(&transaction_lock_path(root))?,
        })
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
