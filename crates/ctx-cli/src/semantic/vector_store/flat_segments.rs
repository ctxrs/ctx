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

use fs2::FileExt as _;
use memmap2::{Mmap, MmapOptions};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

const STORE_FORMAT: &str = "ctx-flat-f32";
const MANIFEST_ENVELOPE_VERSION: u32 = 1;
const MANIFEST_SCHEMA_VERSION: u32 = 1;
const SEGMENT_FORMAT_VERSION: u32 = 1;
const HEADER_BYTES: usize = 4_096;
const HEADER_BYTES_U64: u64 = HEADER_BYTES as u64;
const METADATA_RECORD_BYTES: usize = 72;
const MUTATION_RECORD_BYTES: usize = 24;
const VECTOR_ALIGNMENT: usize = 64;
const MAX_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
const MAX_DIMENSIONS: u32 = 65_536;
const MAX_CONTRACT_FIELD_BYTES: usize = 1_024;
const NORMALIZED_NORM_SQUARED_TOLERANCE: f64 = 1.0e-3;

const MANIFESTS_DIRECTORY: &str = "flat_manifests";
const SEGMENTS_DIRECTORY: &str = "flat_segments";
const WRITER_LOCK_FILE: &str = "flat_writer.lock";
const MANIFEST_PREFIX: &str = "flat-manifest-";
const SEGMENT_PREFIX: &str = "flat-segment-";
const TEMP_PREFIX: &str = ".flat-tmp-";

const VECTOR_MAGIC: [u8; 8] = *b"CTXF32V\0";
const METADATA_MAGIC: [u8; 8] = *b"CTXF32M\0";
const MUTATION_MAGIC: [u8; 8] = *b"CTXF32T\0";

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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
        let mut store = Self {
            root,
            contract,
            mode: StoreMode::ReadWrite,
            validated: Mutex::new(None),
            pinned: Mutex::new(None),
            recovery: FlatRecoveryReport::default(),
        };
        store.recovery = store.recover_internal()?;
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
            recovery: FlatRecoveryReport::default(),
        };
        let _guard = store.lock_shared()?;
        if let Some(selected) = select_manifest(&store.root, &store.contract)? {
            let _ = store.load_pinned(&selected)?;
        }
        Ok(store)
    }

    pub(in crate::semantic) fn root(&self) -> &Path {
        &self.root
    }

    pub(in crate::semantic) fn recovery_report(&self) -> &FlatRecoveryReport {
        &self.recovery
    }

    pub(in crate::semantic) fn recover(&mut self) -> FlatResult<FlatRecoveryReport> {
        self.require_writable()?;
        let report = self.recover_internal()?;
        self.recovery = report.clone();
        Ok(report)
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

    pub(in crate::semantic) fn compact(&self) -> FlatResult<FlatPublishOutcome> {
        self.require_writable()?;
        let _guard = self.lock_exclusive()?;
        let Some(current) = self.load_current_locked()? else {
            return Ok(FlatPublishOutcome {
                published: false,
                generation: 0,
                generation_hash: None,
                replaced_events: 0,
                deleted_events: 0,
            });
        };
        if current.envelope.manifest.segments.len() == 1
            && current.envelope.manifest.segments[0].kind == SegmentKind::Base
        {
            return Ok(noop_outcome(Some(&current)));
        }

        let pinned = self.load_pinned(&current)?;
        let generation = next_generation(Some(&current))?;
        let staged = write_compacted_segment(&self.root, &self.contract, generation, &pinned)?;
        let replaced_events = pinned.active_events().len();
        sync_directory(&segments_directory(&self.root))?;
        validate_staged_segment(&self.root, &self.contract, &staged.descriptor)?;

        let mut manifest = Manifest::new(self.contract.clone());
        manifest.generation = generation;
        manifest.created_unix_millis = unix_millis();
        manifest.segments.push(staged.descriptor);
        let selected = publish_manifest(&self.root, manifest)?;
        self.remember_validated(&selected)?;
        drop(pinned);
        self.clear_pinned()?;
        let _ = cleanup_obsolete_locked(&self.root, &selected);
        Ok(FlatPublishOutcome {
            published: true,
            generation,
            generation_hash: Some(selected.generation_hash),
            replaced_events,
            deleted_events: 0,
        })
    }

    fn recover_internal(&self) -> FlatResult<FlatRecoveryReport> {
        let _guard = self.lock_exclusive()?;
        let mut report = remove_temporary_files(&self.root)?;
        let selected = select_manifest_any(&self.root)?;
        if let Some(selected) = selected {
            if selected.envelope.manifest.model == self.contract {
                let _ = self.load_pinned(&selected)?;
                merge_recovery_reports(
                    &mut report,
                    cleanup_obsolete_locked(&self.root, &selected)?,
                );
            } else {
                // A prior reset may have reached immutable segment rename but
                // not manifest publication. Retire only artifacts not named by
                // the still-active old manifest before retrying its generation.
                merge_recovery_reports(
                    &mut report,
                    cleanup_obsolete_locked(&self.root, &selected)?,
                );
                let generation = next_generation(Some(&selected))?;
                let staged = write_empty_base_segment(&self.root, &self.contract, generation)?;
                sync_directory(&segments_directory(&self.root))?;
                validate_staged_segment(&self.root, &self.contract, &staged.descriptor)?;
                let mut manifest = Manifest::new(self.contract.clone());
                manifest.generation = generation;
                manifest.created_unix_millis = unix_millis();
                manifest.segments.push(staged.descriptor);
                let reset = publish_manifest(&self.root, manifest)?;
                self.remember_validated(&reset)?;
                self.clear_pinned()?;
                report.model_contract_reset = true;
                merge_recovery_reports(&mut report, cleanup_obsolete_locked(&self.root, &reset)?);
            }
        } else {
            merge_recovery_reports(&mut report, cleanup_without_manifest(&self.root)?);
            self.clear_validated()?;
            self.clear_pinned()?;
        }
        Ok(report)
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
        file.lock_shared()
            .map_err(|source| io_error("lock shared", path, source))?;
        Ok(Self { file })
    }

    fn exclusive(path: &Path) -> FlatResult<Self> {
        let file = open_lock(path, true)?;
        file.lock_exclusive()
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

#[derive(Clone)]
pub(in crate::semantic) struct PinnedFlatGeneration {
    inner: Arc<PinnedGenerationInner>,
}

struct PinnedGenerationInner {
    generation: u64,
    generation_hash: String,
    contract: FlatModelContract,
    scan_segments: Vec<PinnedScanSegment>,
    active_events: Vec<FlatActiveEvent>,
    stats: FlatActiveStats,
}

impl PinnedFlatGeneration {
    pub(in crate::semantic) fn generation(&self) -> u64 {
        self.inner.generation
    }

    pub(in crate::semantic) fn generation_hash(&self) -> &str {
        &self.inner.generation_hash
    }

    pub(in crate::semantic) fn model_contract(&self) -> &FlatModelContract {
        &self.inner.contract
    }

    pub(in crate::semantic) fn scan_segments(&self) -> &[PinnedScanSegment] {
        &self.inner.scan_segments
    }

    pub(in crate::semantic) fn active_events(&self) -> &[FlatActiveEvent] {
        &self.inner.active_events
    }

    pub(in crate::semantic) fn stats(&self) -> &FlatActiveStats {
        &self.inner.stats
    }
}

#[derive(Clone)]
pub(in crate::semantic) struct PinnedScanSegment {
    inner: Arc<PinnedScanSegmentInner>,
}

struct PinnedScanSegmentInner {
    generation: u64,
    vector_count: usize,
    dimensions: usize,
    stride_bytes: usize,
    vectors: Mmap,
    metadata: Mmap,
    active_bits: Vec<u64>,
}

impl PinnedScanSegment {
    pub(in crate::semantic) fn generation(&self) -> u64 {
        self.inner.generation
    }

    pub(in crate::semantic) fn vector_count(&self) -> usize {
        self.inner.vector_count
    }

    pub(in crate::semantic) fn dimensions(&self) -> usize {
        self.inner.dimensions
    }

    pub(in crate::semantic) fn active_chunk_count(&self) -> usize {
        self.inner
            .active_bits
            .iter()
            .map(|word| word.count_ones() as usize)
            .sum()
    }

    pub(in crate::semantic) fn chunks(&self) -> FlatScanChunkIter<'_> {
        FlatScanChunkIter {
            segment: self,
            ordinal: 0,
        }
    }

    fn is_active(&self, ordinal: usize) -> bool {
        let word = ordinal / 64;
        let bit = ordinal % 64;
        self.inner
            .active_bits
            .get(word)
            .is_some_and(|value| value & (1_u64 << bit) != 0)
    }

    fn metadata(&self, ordinal: usize) -> FlatChunkMetadata {
        let start = HEADER_BYTES + ordinal * METADATA_RECORD_BYTES;
        decode_metadata_record(&self.inner.metadata[start..start + METADATA_RECORD_BYTES])
    }

    fn vector(&self, ordinal: usize) -> &[f32] {
        let start = HEADER_BYTES + ordinal * self.inner.stride_bytes;
        let pointer = self.inner.vectors[start..].as_ptr().cast::<f32>();
        // The format fixes the payload at a page-aligned offset and every row
        // at a 64-byte stride. Opening rejects non-little-endian targets and
        // validates the complete mapped byte range before this slice is built.
        unsafe { std::slice::from_raw_parts(pointer, self.inner.dimensions) }
    }
}

pub(in crate::semantic) struct FlatScanChunkIter<'a> {
    segment: &'a PinnedScanSegment,
    ordinal: usize,
}

impl<'a> Iterator for FlatScanChunkIter<'a> {
    type Item = FlatScanChunkRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        while self.ordinal < self.segment.vector_count() {
            let ordinal = self.ordinal;
            self.ordinal += 1;
            if !self.segment.is_active(ordinal) {
                continue;
            }
            let metadata = self.segment.metadata(ordinal);
            return Some(FlatScanChunkRef {
                ordinal,
                event_id: metadata.event_id,
                seq: metadata.seq,
                source_text_hash: metadata.source_text_hash,
                chunk_index: metadata.chunk_index,
                start_char: metadata.start_char,
                end_char: metadata.end_char,
                vector: self.segment.vector(ordinal),
            });
        }
        None
    }
}

pub(in crate::semantic) struct FlatScanChunkRef<'a> {
    pub(in crate::semantic) ordinal: usize,
    pub(in crate::semantic) event_id: Uuid,
    pub(in crate::semantic) seq: u64,
    pub(in crate::semantic) source_text_hash: FlatSourceHash,
    pub(in crate::semantic) chunk_index: u32,
    pub(in crate::semantic) start_char: u32,
    pub(in crate::semantic) end_char: u32,
    pub(in crate::semantic) vector: &'a [f32],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestEnvelope {
    format: String,
    envelope_version: u32,
    manifest: Manifest,
    manifest_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    generation: u64,
    created_unix_millis: u64,
    model: FlatModelContract,
    segments: Vec<SegmentDescriptor>,
}

impl Manifest {
    fn new(model: FlatModelContract) -> Self {
        Self {
            schema_version: MANIFEST_SCHEMA_VERSION,
            generation: 0,
            created_unix_millis: 0,
            model,
            segments: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SegmentKind {
    Base,
    Delta,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SegmentDescriptor {
    format_version: u32,
    generation: u64,
    kind: SegmentKind,
    vector_count: u64,
    mutation_count: u64,
    vectors: ArtifactDescriptor,
    metadata: ArtifactDescriptor,
    mutations: ArtifactDescriptor,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactDescriptor {
    file: String,
    file_bytes: u64,
    payload_sha256: String,
}

struct SelectedManifest {
    envelope: ManifestEnvelope,
    generation_hash: String,
    path: PathBuf,
}

struct StagedSegment {
    descriptor: SegmentDescriptor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MutationKind {
    Replace = 1,
    Delete = 2,
}

#[derive(Debug, Clone, Copy)]
struct EventMutation {
    event_id: Uuid,
    kind: MutationKind,
}

#[derive(Debug, Clone, Copy)]
struct FlatChunkMetadata {
    event_id: Uuid,
    seq: u64,
    source_text_hash: FlatSourceHash,
    chunk_index: u32,
    start_char: u32,
    end_char: u32,
}

#[derive(Debug, Clone, Copy)]
struct EventVersion {
    generation: u64,
    kind: MutationKind,
}

struct LoadedSegment {
    descriptor: SegmentDescriptor,
    vectors: Mmap,
    metadata: Mmap,
    mutations: Vec<EventMutation>,
    stride_bytes: usize,
}

fn load_pinned_generation(
    root: &Path,
    selected: &SelectedManifest,
) -> FlatResult<PinnedFlatGeneration> {
    let manifest = &selected.envelope.manifest;
    let mut loaded = Vec::with_capacity(manifest.segments.len());
    let mut versions = HashMap::<Uuid, EventVersion>::new();
    let mut stored_chunks = 0_u64;
    let mut stored_vector_bytes = 0_u64;

    for descriptor in &manifest.segments {
        let segment = load_and_validate_segment(root, &manifest.model, descriptor)?;
        stored_chunks = stored_chunks
            .checked_add(descriptor.vector_count)
            .ok_or_else(|| FlatStoreError::Corrupt("stored chunk count overflow".to_owned()))?;
        stored_vector_bytes = stored_vector_bytes
            .checked_add(
                descriptor
                    .vector_count
                    .checked_mul(u64::from(manifest.model.dimensions))
                    .and_then(|value| value.checked_mul(4))
                    .ok_or_else(|| {
                        FlatStoreError::Corrupt("stored vector byte count overflow".to_owned())
                    })?,
            )
            .ok_or_else(|| FlatStoreError::Corrupt("stored vector bytes overflow".to_owned()))?;
        for mutation in &segment.mutations {
            versions.insert(
                mutation.event_id,
                EventVersion {
                    generation: descriptor.generation,
                    kind: mutation.kind,
                },
            );
        }
        loaded.push(segment);
    }

    let deleted_events = versions
        .values()
        .filter(|version| version.kind == MutationKind::Delete)
        .count();
    let mut summaries = BTreeMap::<Uuid, FlatActiveEvent>::new();
    let mut scan_segments = Vec::with_capacity(loaded.len());
    let mut active_chunks = 0_usize;
    for segment in loaded {
        let vector_count = usize_from_u64(segment.descriptor.vector_count, "vector count")?;
        let mut active_bits = vec![0_u64; vector_count.div_ceil(64)];
        for ordinal in 0..vector_count {
            let metadata = metadata_at(&segment.metadata, ordinal);
            let active = versions.get(&metadata.event_id).is_some_and(|version| {
                version.kind == MutationKind::Replace
                    && version.generation == segment.descriptor.generation
            });
            if !active {
                continue;
            }
            active_bits[ordinal / 64] |= 1_u64 << (ordinal % 64);
            active_chunks = active_chunks
                .checked_add(1)
                .ok_or_else(|| FlatStoreError::Corrupt("active chunk count overflow".to_owned()))?;
            let entry = summaries
                .entry(metadata.event_id)
                .or_insert(FlatActiveEvent {
                    event_id: metadata.event_id,
                    seq: metadata.seq,
                    source_text_hash: metadata.source_text_hash,
                    chunk_count: 0,
                });
            if entry.seq != metadata.seq || entry.source_text_hash != metadata.source_text_hash {
                return Err(FlatStoreError::Corrupt(format!(
                    "active event {} has inconsistent sequence or source hash",
                    metadata.event_id
                )));
            }
            entry.chunk_count = entry.chunk_count.checked_add(1).ok_or_else(|| {
                FlatStoreError::Corrupt(format!(
                    "active event {} has too many chunks",
                    metadata.event_id
                ))
            })?;
        }
        scan_segments.push(PinnedScanSegment {
            inner: Arc::new(PinnedScanSegmentInner {
                generation: segment.descriptor.generation,
                vector_count,
                dimensions: usize_from_u32(manifest.model.dimensions, "dimensions")?,
                stride_bytes: segment.stride_bytes,
                vectors: segment.vectors,
                metadata: segment.metadata,
                active_bits,
            }),
        });
    }

    let active_vector_bytes = u64::try_from(active_chunks)
        .ok()
        .and_then(|count| count.checked_mul(u64::from(manifest.model.dimensions)))
        .and_then(|count| count.checked_mul(4))
        .ok_or_else(|| FlatStoreError::Corrupt("active vector byte count overflow".to_owned()))?;
    let active_events = summaries.into_values().collect::<Vec<_>>();
    let stats = FlatActiveStats {
        generation: manifest.generation,
        generation_hash: Some(selected.generation_hash.clone()),
        segment_count: manifest.segments.len(),
        active_events: active_events.len(),
        active_chunks,
        active_vector_bytes,
        stored_chunks,
        stored_vector_bytes,
        deleted_events,
    };
    Ok(PinnedFlatGeneration {
        inner: Arc::new(PinnedGenerationInner {
            generation: manifest.generation,
            generation_hash: selected.generation_hash.clone(),
            contract: manifest.model.clone(),
            scan_segments,
            active_events,
            stats,
        }),
    })
}

fn load_and_validate_segment(
    root: &Path,
    contract: &FlatModelContract,
    descriptor: &SegmentDescriptor,
) -> FlatResult<LoadedSegment> {
    let vectors = map_artifact(
        root,
        descriptor,
        &descriptor.vectors,
        ArtifactRole::Vectors,
        contract,
    )?;
    let metadata = map_artifact(
        root,
        descriptor,
        &descriptor.metadata,
        ArtifactRole::Metadata,
        contract,
    )?;
    let mutation_map = map_artifact(
        root,
        descriptor,
        &descriptor.mutations,
        ArtifactRole::Mutations,
        contract,
    )?;
    let vector_header = decode_header(&vectors)?;
    let metadata_header = decode_header(&metadata)?;
    let mutation_header = decode_header(&mutation_map)?;
    let stride_bytes = usize_from_u32(vector_header.record_bytes, "vector stride")?;

    validate_vector_payload(&vectors, &vector_header, contract)?;
    let mutations = validate_mutation_payload(
        &mutation_map,
        &mutation_header,
        descriptor.kind,
        descriptor.generation,
    )?;
    validate_metadata_payload(
        &metadata,
        &metadata_header,
        &mutations,
        descriptor.generation,
    )?;
    Ok(LoadedSegment {
        descriptor: descriptor.clone(),
        vectors,
        metadata,
        mutations,
        stride_bytes,
    })
}

fn validate_staged_segment(
    root: &Path,
    contract: &FlatModelContract,
    descriptor: &SegmentDescriptor,
) -> FlatResult<()> {
    let _ = load_and_validate_segment(root, contract, descriptor)?;
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum ArtifactRole {
    Vectors,
    Metadata,
    Mutations,
}

impl ArtifactRole {
    fn magic(self) -> [u8; 8] {
        match self {
            Self::Vectors => VECTOR_MAGIC,
            Self::Metadata => METADATA_MAGIC,
            Self::Mutations => MUTATION_MAGIC,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Vectors => "vectors",
            Self::Metadata => "metadata",
            Self::Mutations => "mutations",
        }
    }
}

#[derive(Debug)]
struct SegmentHeader {
    magic: [u8; 8],
    format_version: u32,
    header_bytes: u32,
    generation: u64,
    record_count: u64,
    record_bytes: u32,
    dimensions: u32,
    payload_bytes: u64,
    payload_sha256: [u8; 32],
}

fn map_artifact(
    root: &Path,
    segment: &SegmentDescriptor,
    artifact: &ArtifactDescriptor,
    role: ArtifactRole,
    contract: &FlatModelContract,
) -> FlatResult<Mmap> {
    validate_artifact_name(&artifact.file, segment.generation, role)?;
    let path = segments_directory(root).join(&artifact.file);
    let metadata = symlink_metadata_file(&path)?;
    if metadata.len() != artifact.file_bytes {
        return Err(FlatStoreError::Corrupt(format!(
            "{} has {} bytes, manifest requires {}",
            artifact.file,
            metadata.len(),
            artifact.file_bytes
        )));
    }
    if metadata.len() < HEADER_BYTES_U64 {
        return Err(FlatStoreError::Corrupt(format!(
            "{} is shorter than its header",
            artifact.file
        )));
    }
    let file = File::open(&path).map_err(|source| io_error("open flat segment", &path, source))?;
    // The map is read-only and the file length/type were checked immediately
    // before mapping. All offsets are checked again before typed access.
    let mapping = unsafe {
        MmapOptions::new()
            .map(&file)
            .map_err(|source| io_error("mmap flat segment", &path, source))?
    };
    let header = decode_header(&mapping)?;
    validate_header(&header, segment, artifact, role, contract)?;
    let payload = &mapping[HEADER_BYTES..];
    let actual = Sha256::digest(payload);
    if actual.as_slice() != header.payload_sha256
        || encode_hex(actual.as_slice()) != artifact.payload_sha256
    {
        return Err(FlatStoreError::Corrupt(format!(
            "{} payload checksum mismatch",
            artifact.file
        )));
    }
    Ok(mapping)
}

fn validate_header(
    header: &SegmentHeader,
    segment: &SegmentDescriptor,
    artifact: &ArtifactDescriptor,
    role: ArtifactRole,
    contract: &FlatModelContract,
) -> FlatResult<()> {
    if header.magic != role.magic()
        || header.format_version != SEGMENT_FORMAT_VERSION
        || header.header_bytes != HEADER_BYTES as u32
        || header.generation != segment.generation
    {
        return Err(FlatStoreError::Corrupt(format!(
            "{} has an incompatible {} header",
            artifact.file,
            role.name()
        )));
    }
    let expected_count = match role {
        ArtifactRole::Vectors | ArtifactRole::Metadata => segment.vector_count,
        ArtifactRole::Mutations => segment.mutation_count,
    };
    if header.record_count != expected_count {
        return Err(FlatStoreError::Corrupt(format!(
            "{} record count does not match its manifest",
            artifact.file
        )));
    }
    let expected_record_bytes = match role {
        ArtifactRole::Vectors => vector_stride(contract.dimensions)?,
        ArtifactRole::Metadata => METADATA_RECORD_BYTES as u32,
        ArtifactRole::Mutations => MUTATION_RECORD_BYTES as u32,
    };
    let expected_dimensions = match role {
        ArtifactRole::Vectors => contract.dimensions,
        ArtifactRole::Metadata | ArtifactRole::Mutations => 0,
    };
    if header.record_bytes != expected_record_bytes || header.dimensions != expected_dimensions {
        return Err(FlatStoreError::Corrupt(format!(
            "{} record layout does not match the model contract",
            artifact.file
        )));
    }
    let expected_payload = header
        .record_count
        .checked_mul(u64::from(header.record_bytes))
        .ok_or_else(|| FlatStoreError::Corrupt("segment payload length overflow".to_owned()))?;
    let expected_file_bytes = HEADER_BYTES_U64
        .checked_add(expected_payload)
        .ok_or_else(|| FlatStoreError::Corrupt("segment file length overflow".to_owned()))?;
    if header.payload_bytes != expected_payload || artifact.file_bytes != expected_file_bytes {
        return Err(FlatStoreError::Corrupt(format!(
            "{} payload length does not match its header",
            artifact.file
        )));
    }
    let expected_digest = decode_sha256(&artifact.payload_sha256).ok_or_else(|| {
        FlatStoreError::Corrupt(format!(
            "{} has an invalid manifest checksum",
            artifact.file
        ))
    })?;
    if header.payload_sha256 != expected_digest {
        return Err(FlatStoreError::Corrupt(format!(
            "{} header checksum does not match its manifest",
            artifact.file
        )));
    }
    Ok(())
}

fn validate_vector_payload(
    mapping: &Mmap,
    header: &SegmentHeader,
    contract: &FlatModelContract,
) -> FlatResult<()> {
    let count = usize_from_u64(header.record_count, "vector count")?;
    let dimensions = usize_from_u32(contract.dimensions, "dimensions")?;
    let stride = usize_from_u32(header.record_bytes, "vector stride")?;
    let vector_bytes = dimensions
        .checked_mul(4)
        .ok_or_else(|| FlatStoreError::Corrupt("vector byte length overflow".to_owned()))?;
    for ordinal in 0..count {
        let start = HEADER_BYTES
            .checked_add(ordinal.checked_mul(stride).ok_or_else(|| {
                FlatStoreError::Corrupt("vector payload offset overflow".to_owned())
            })?)
            .ok_or_else(|| FlatStoreError::Corrupt("vector payload offset overflow".to_owned()))?;
        let row = mapping.get(start..start + stride).ok_or_else(|| {
            FlatStoreError::Corrupt("vector payload is shorter than declared".to_owned())
        })?;
        let mut norm_squared = 0.0_f64;
        for value in row[..vector_bytes].chunks_exact(4) {
            let value = f32::from_le_bytes([value[0], value[1], value[2], value[3]]);
            if !value.is_finite() {
                return Err(FlatStoreError::Corrupt(format!(
                    "vector {ordinal} contains a non-finite component"
                )));
            }
            norm_squared += f64::from(value) * f64::from(value);
        }
        if (norm_squared - 1.0).abs() > NORMALIZED_NORM_SQUARED_TOLERANCE {
            return Err(FlatStoreError::Corrupt(format!(
                "vector {ordinal} is not L2-normalized (norm squared {norm_squared})"
            )));
        }
        if row[vector_bytes..].iter().any(|byte| *byte != 0) {
            return Err(FlatStoreError::Corrupt(format!(
                "vector {ordinal} has non-zero alignment padding"
            )));
        }
    }
    Ok(())
}

fn validate_mutation_payload(
    mapping: &Mmap,
    header: &SegmentHeader,
    kind: SegmentKind,
    generation: u64,
) -> FlatResult<Vec<EventMutation>> {
    let count = usize_from_u64(header.record_count, "mutation count")?;
    let mut mutations = Vec::with_capacity(count);
    let mut previous = None::<Uuid>;
    for ordinal in 0..count {
        let start = HEADER_BYTES + ordinal * MUTATION_RECORD_BYTES;
        let record = mapping
            .get(start..start + MUTATION_RECORD_BYTES)
            .ok_or_else(|| {
                FlatStoreError::Corrupt("mutation payload is shorter than declared".to_owned())
            })?;
        let mutation = decode_mutation_record(record)?;
        if previous.is_some_and(|value| value >= mutation.event_id) {
            return Err(FlatStoreError::Corrupt(format!(
                "segment generation {generation} mutations are not uniquely sorted"
            )));
        }
        if kind == SegmentKind::Base && mutation.kind != MutationKind::Replace {
            return Err(FlatStoreError::Corrupt(format!(
                "base segment generation {generation} contains a deletion"
            )));
        }
        previous = Some(mutation.event_id);
        mutations.push(mutation);
    }
    Ok(mutations)
}

fn validate_metadata_payload(
    mapping: &Mmap,
    header: &SegmentHeader,
    mutations: &[EventMutation],
    generation: u64,
) -> FlatResult<()> {
    let mutation_kinds = mutations
        .iter()
        .map(|mutation| (mutation.event_id, mutation.kind))
        .collect::<HashMap<_, _>>();
    let count = usize_from_u64(header.record_count, "metadata count")?;
    let mut current_event = None::<Uuid>;
    let mut previous_chunk_index = None::<u32>;
    let mut completed_events = HashSet::<Uuid>::new();
    let mut event_evidence = HashMap::<Uuid, (u64, FlatSourceHash)>::new();
    let mut metadata_events = HashSet::<Uuid>::new();
    for ordinal in 0..count {
        let start = HEADER_BYTES + ordinal * METADATA_RECORD_BYTES;
        let record = mapping
            .get(start..start + METADATA_RECORD_BYTES)
            .ok_or_else(|| {
                FlatStoreError::Corrupt("metadata payload is shorter than declared".to_owned())
            })?;
        let metadata = decode_metadata_record_checked(record)?;
        if metadata.start_char > metadata.end_char {
            return Err(FlatStoreError::Corrupt(format!(
                "segment generation {generation} has an inverted character range"
            )));
        }
        if current_event == Some(metadata.event_id) {
            if previous_chunk_index.is_some_and(|index| index >= metadata.chunk_index) {
                return Err(FlatStoreError::Corrupt(format!(
                    "segment generation {generation} repeats or reorders an event chunk"
                )));
            }
        } else {
            if let Some(event_id) = current_event {
                completed_events.insert(event_id);
            }
            if completed_events.contains(&metadata.event_id) {
                return Err(FlatStoreError::Corrupt(format!(
                    "segment generation {generation} splits one event across metadata ranges"
                )));
            }
            current_event = Some(metadata.event_id);
        }
        if mutation_kinds.get(&metadata.event_id) != Some(&MutationKind::Replace) {
            return Err(FlatStoreError::Corrupt(format!(
                "segment generation {generation} metadata has no replacement mutation"
            )));
        }
        if event_evidence
            .insert(metadata.event_id, (metadata.seq, metadata.source_text_hash))
            .is_some_and(|evidence| evidence != (metadata.seq, metadata.source_text_hash))
        {
            return Err(FlatStoreError::Corrupt(format!(
                "segment generation {generation} has inconsistent event sequence or hash"
            )));
        }
        metadata_events.insert(metadata.event_id);
        previous_chunk_index = Some(metadata.chunk_index);
    }
    for mutation in mutations {
        if mutation.kind == MutationKind::Replace && !metadata_events.contains(&mutation.event_id) {
            return Err(FlatStoreError::Corrupt(format!(
                "segment generation {generation} replacement has no chunks"
            )));
        }
        if mutation.kind == MutationKind::Delete && metadata_events.contains(&mutation.event_id) {
            return Err(FlatStoreError::Corrupt(format!(
                "segment generation {generation} deletion also has chunks"
            )));
        }
    }
    Ok(())
}

fn metadata_at(mapping: &Mmap, ordinal: usize) -> FlatChunkMetadata {
    let start = HEADER_BYTES + ordinal * METADATA_RECORD_BYTES;
    decode_metadata_record(&mapping[start..start + METADATA_RECORD_BYTES])
}

fn select_manifest(
    root: &Path,
    expected_contract: &FlatModelContract,
) -> FlatResult<Option<SelectedManifest>> {
    let selected = select_manifest_any(root)?;
    if selected
        .as_ref()
        .is_some_and(|selected| &selected.envelope.manifest.model != expected_contract)
    {
        return Err(FlatStoreError::Incompatible(
            "manifest model/tokenizer/pooling/dimension/normalization contract changed".to_owned(),
        ));
    }
    Ok(selected)
}

fn select_manifest_any(root: &Path) -> FlatResult<Option<SelectedManifest>> {
    let directory = manifests_directory(root);
    let entries = fs::read_dir(&directory)
        .map_err(|source| io_error("read flat manifest directory", &directory, source))?;
    let mut candidates = Vec::<(u64, String, PathBuf)>::new();
    for entry in entries {
        let entry =
            entry.map_err(|source| io_error("read flat manifest entry", &directory, source))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name.starts_with(TEMP_PREFIX) {
            continue;
        }
        if !name.starts_with(MANIFEST_PREFIX) {
            continue;
        }
        let (generation, digest) = parse_manifest_name(name).ok_or_else(|| {
            FlatStoreError::Corrupt(format!("malformed committed manifest name {name:?}"))
        })?;
        let metadata = entry
            .metadata()
            .map_err(|source| io_error("stat flat manifest", &entry.path(), source))?;
        if !metadata.is_file() {
            return Err(FlatStoreError::Corrupt(format!(
                "committed manifest {name:?} is not a regular file"
            )));
        }
        candidates.push((generation, digest, entry.path()));
    }
    candidates.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    let Some((highest_generation, _, _)) = candidates.last() else {
        return Ok(None);
    };
    if candidates
        .iter()
        .rev()
        .take_while(|candidate| candidate.0 == *highest_generation)
        .count()
        != 1
    {
        return Err(FlatStoreError::Corrupt(format!(
            "multiple manifests claim generation {highest_generation}"
        )));
    }
    let (filename_generation, filename_digest, path) = candidates
        .pop()
        .ok_or_else(|| FlatStoreError::Corrupt("manifest selection failed".to_owned()))?;
    let envelope = read_manifest(&path)?;
    validate_manifest(&envelope, filename_generation, &filename_digest)?;
    Ok(Some(SelectedManifest {
        envelope,
        generation_hash: filename_digest,
        path,
    }))
}

fn read_manifest(path: &Path) -> FlatResult<ManifestEnvelope> {
    let metadata = symlink_metadata_file(path)?;
    if metadata.len() == 0 || metadata.len() > MAX_MANIFEST_BYTES {
        return Err(FlatStoreError::Corrupt(format!(
            "manifest {} has unsafe size {}",
            path.display(),
            metadata.len()
        )));
    }
    let mut file =
        File::open(path).map_err(|source| io_error("open flat manifest", path, source))?;
    let capacity = usize_from_u64(metadata.len(), "manifest size")?;
    let mut bytes = Vec::with_capacity(capacity);
    file.read_to_end(&mut bytes)
        .map_err(|source| io_error("read flat manifest", path, source))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| FlatStoreError::Corrupt(format!("invalid manifest JSON: {error}")))
}

fn validate_manifest(
    envelope: &ManifestEnvelope,
    filename_generation: u64,
    filename_digest: &str,
) -> FlatResult<()> {
    if envelope.format != STORE_FORMAT
        || envelope.envelope_version != MANIFEST_ENVELOPE_VERSION
        || envelope.manifest.schema_version != MANIFEST_SCHEMA_VERSION
    {
        return Err(FlatStoreError::Incompatible(
            "manifest format or schema version is unsupported".to_owned(),
        ));
    }
    validate_model_contract(&envelope.manifest.model).map_err(|error| {
        FlatStoreError::Corrupt(format!("manifest has an invalid model contract: {error}"))
    })?;
    let manifest_bytes = serde_json::to_vec(&envelope.manifest)?;
    let actual_digest = encode_hex(Sha256::digest(&manifest_bytes).as_slice());
    if envelope.manifest_sha256 != actual_digest || filename_digest != actual_digest {
        return Err(FlatStoreError::Corrupt(
            "manifest checksum does not match its payload and filename".to_owned(),
        ));
    }
    if envelope.manifest.generation == 0
        || envelope.manifest.generation != filename_generation
        || envelope.manifest.segments.is_empty()
    {
        return Err(FlatStoreError::Corrupt(
            "manifest generation or segment set is invalid".to_owned(),
        ));
    }
    let mut prior_generation = 0_u64;
    let mut saw_base = false;
    for (index, segment) in envelope.manifest.segments.iter().enumerate() {
        if segment.format_version != SEGMENT_FORMAT_VERSION
            || segment.generation <= prior_generation
            || segment.generation > envelope.manifest.generation
        {
            return Err(FlatStoreError::Corrupt(
                "manifest segment generations are invalid".to_owned(),
            ));
        }
        match segment.kind {
            SegmentKind::Base if index != 0 || saw_base => {
                return Err(FlatStoreError::Corrupt(
                    "base segment must be the first and only base".to_owned(),
                ));
            }
            SegmentKind::Base => saw_base = true,
            SegmentKind::Delta => {}
        }
        validate_artifact_descriptor(segment, &segment.vectors, ArtifactRole::Vectors)?;
        validate_artifact_descriptor(segment, &segment.metadata, ArtifactRole::Metadata)?;
        validate_artifact_descriptor(segment, &segment.mutations, ArtifactRole::Mutations)?;
        prior_generation = segment.generation;
    }
    if prior_generation != envelope.manifest.generation {
        return Err(FlatStoreError::Corrupt(
            "manifest has no segment for its publication generation".to_owned(),
        ));
    }
    Ok(())
}

fn validate_artifact_descriptor(
    segment: &SegmentDescriptor,
    artifact: &ArtifactDescriptor,
    role: ArtifactRole,
) -> FlatResult<()> {
    validate_artifact_name(&artifact.file, segment.generation, role)?;
    if decode_sha256(&artifact.payload_sha256).is_none() {
        return Err(FlatStoreError::Corrupt(format!(
            "{} has a malformed checksum",
            artifact.file
        )));
    }
    let record_count = match role {
        ArtifactRole::Vectors | ArtifactRole::Metadata => segment.vector_count,
        ArtifactRole::Mutations => segment.mutation_count,
    };
    let minimum_record_bytes = match role {
        ArtifactRole::Vectors => 4_u64,
        ArtifactRole::Metadata => METADATA_RECORD_BYTES as u64,
        ArtifactRole::Mutations => MUTATION_RECORD_BYTES as u64,
    };
    let minimum = HEADER_BYTES_U64
        .checked_add(
            record_count
                .checked_mul(minimum_record_bytes)
                .ok_or_else(|| {
                    FlatStoreError::Corrupt("artifact byte length overflow".to_owned())
                })?,
        )
        .ok_or_else(|| FlatStoreError::Corrupt("artifact byte length overflow".to_owned()))?;
    if artifact.file_bytes < minimum {
        return Err(FlatStoreError::Corrupt(format!(
            "{} is too short for its record count",
            artifact.file
        )));
    }
    Ok(())
}

fn publish_manifest(root: &Path, manifest: Manifest) -> FlatResult<SelectedManifest> {
    let manifest_bytes = serde_json::to_vec(&manifest)?;
    let digest = encode_hex(Sha256::digest(&manifest_bytes).as_slice());
    let envelope = ManifestEnvelope {
        format: STORE_FORMAT.to_owned(),
        envelope_version: MANIFEST_ENVELOPE_VERSION,
        manifest,
        manifest_sha256: digest.clone(),
    };
    let bytes = serde_json::to_vec(&envelope)?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(FlatStoreError::InvalidInput(
            "manifest exceeds the safe size limit; compact first".to_owned(),
        ));
    }
    let directory = manifests_directory(root);
    let final_path = directory.join(manifest_name(envelope.manifest.generation, &digest));
    let temporary = unique_temporary_path(&directory, "manifest");
    let mut file = create_new_file(&temporary)?;
    file.write_all(&bytes)
        .map_err(|source| io_error("write flat manifest", &temporary, source))?;
    file.sync_all()
        .map_err(|source| io_error("sync flat manifest", &temporary, source))?;
    drop(file);
    commit_unique_file(&temporary, &final_path)?;
    sync_directory(&directory)?;
    Ok(SelectedManifest {
        envelope,
        generation_hash: digest,
        path: final_path,
    })
}

fn write_replacement_segment(
    root: &Path,
    contract: &FlatModelContract,
    generation: u64,
    replacements: &[FlatEventReplacement],
    tombstones: &[Uuid],
) -> FlatResult<StagedSegment> {
    let mut ordered_replacements = replacements.iter().collect::<Vec<_>>();
    ordered_replacements.sort_by_key(|replacement| replacement.event_id);
    let mut ordered_tombstones = tombstones.to_vec();
    ordered_tombstones.sort_unstable();

    let vector_count = ordered_replacements
        .iter()
        .try_fold(0_u64, |total, replacement| {
            let chunks = u64::try_from(replacement.chunks.len()).map_err(|_| {
                FlatStoreError::InvalidInput("replacement chunk count is too large".to_owned())
            })?;
            total.checked_add(chunks).ok_or_else(|| {
                FlatStoreError::InvalidInput("publication vector count overflow".to_owned())
            })
        })?;
    let mutation_count = u64::try_from(ordered_replacements.len())
        .ok()
        .zip(u64::try_from(ordered_tombstones.len()).ok())
        .and_then(|(replacements, tombstones)| replacements.checked_add(tombstones))
        .ok_or_else(|| FlatStoreError::InvalidInput("mutation count overflow".to_owned()))?;

    let directory = segments_directory(root);
    let stride = usize_from_u32(vector_stride(contract.dimensions)?, "vector stride")?;
    let mut vectors = StagedArtifactWriter::new(&directory, generation, ArtifactRole::Vectors)?;
    let mut metadata = StagedArtifactWriter::new(&directory, generation, ArtifactRole::Metadata)?;
    let mut vector_scratch = vec![0_u8; stride];
    for replacement in ordered_replacements {
        let mut chunks = replacement.chunks.iter().collect::<Vec<_>>();
        chunks.sort_by_key(|chunk| chunk.chunk_index);
        for chunk in chunks {
            encode_vector(&chunk.vector, &mut vector_scratch)?;
            vectors.write_payload(&vector_scratch)?;
            metadata.write_payload(&encode_metadata_record(FlatChunkMetadata {
                event_id: replacement.event_id,
                seq: replacement.seq,
                source_text_hash: replacement.source_text_hash,
                chunk_index: chunk.chunk_index,
                start_char: chunk.start_char,
                end_char: chunk.end_char,
            }))?;
        }
    }

    let mut mutations = StagedArtifactWriter::new(&directory, generation, ArtifactRole::Mutations)?;
    let mut ordered_mutations = replacements
        .iter()
        .map(|replacement| EventMutation {
            event_id: replacement.event_id,
            kind: MutationKind::Replace,
        })
        .chain(
            ordered_tombstones
                .into_iter()
                .map(|event_id| EventMutation {
                    event_id,
                    kind: MutationKind::Delete,
                }),
        )
        .collect::<Vec<_>>();
    ordered_mutations.sort_by_key(|mutation| mutation.event_id);
    for mutation in ordered_mutations {
        mutations.write_payload(&encode_mutation_record(mutation))?;
    }

    let vectors = vectors.finalize(vector_count, stride as u32, contract.dimensions)?;
    let metadata = metadata.finalize(vector_count, METADATA_RECORD_BYTES as u32, 0)?;
    let mutations = mutations.finalize(mutation_count, MUTATION_RECORD_BYTES as u32, 0)?;
    Ok(StagedSegment {
        descriptor: SegmentDescriptor {
            format_version: SEGMENT_FORMAT_VERSION,
            generation,
            kind: SegmentKind::Delta,
            vector_count,
            mutation_count,
            vectors,
            metadata,
            mutations,
        },
    })
}

fn write_compacted_segment(
    root: &Path,
    contract: &FlatModelContract,
    generation: u64,
    pinned: &PinnedFlatGeneration,
) -> FlatResult<StagedSegment> {
    let vector_count = u64::try_from(pinned.stats().active_chunks)
        .map_err(|_| FlatStoreError::Corrupt("active vector count is too large".to_owned()))?;
    let mutation_count = u64::try_from(pinned.active_events().len())
        .map_err(|_| FlatStoreError::Corrupt("active event count is too large".to_owned()))?;
    let directory = segments_directory(root);
    let stride = usize_from_u32(vector_stride(contract.dimensions)?, "vector stride")?;
    let mut vectors = StagedArtifactWriter::new(&directory, generation, ArtifactRole::Vectors)?;
    let mut metadata = StagedArtifactWriter::new(&directory, generation, ArtifactRole::Metadata)?;
    let mut scratch = vec![0_u8; stride];
    for segment in pinned.scan_segments() {
        for chunk in segment.chunks() {
            encode_vector(chunk.vector, &mut scratch)?;
            vectors.write_payload(&scratch)?;
            metadata.write_payload(&encode_metadata_record(FlatChunkMetadata {
                event_id: chunk.event_id,
                seq: chunk.seq,
                source_text_hash: chunk.source_text_hash,
                chunk_index: chunk.chunk_index,
                start_char: chunk.start_char,
                end_char: chunk.end_char,
            }))?;
        }
    }
    let mut mutations = StagedArtifactWriter::new(&directory, generation, ArtifactRole::Mutations)?;
    for event in pinned.active_events() {
        mutations.write_payload(&encode_mutation_record(EventMutation {
            event_id: event.event_id,
            kind: MutationKind::Replace,
        }))?;
    }
    let vectors = vectors.finalize(vector_count, stride as u32, contract.dimensions)?;
    let metadata = metadata.finalize(vector_count, METADATA_RECORD_BYTES as u32, 0)?;
    let mutations = mutations.finalize(mutation_count, MUTATION_RECORD_BYTES as u32, 0)?;
    Ok(StagedSegment {
        descriptor: SegmentDescriptor {
            format_version: SEGMENT_FORMAT_VERSION,
            generation,
            kind: SegmentKind::Base,
            vector_count,
            mutation_count,
            vectors,
            metadata,
            mutations,
        },
    })
}

fn write_empty_base_segment(
    root: &Path,
    contract: &FlatModelContract,
    generation: u64,
) -> FlatResult<StagedSegment> {
    let directory = segments_directory(root);
    let stride = vector_stride(contract.dimensions)?;
    let vectors = StagedArtifactWriter::new(&directory, generation, ArtifactRole::Vectors)?
        .finalize(0, stride, contract.dimensions)?;
    let metadata = StagedArtifactWriter::new(&directory, generation, ArtifactRole::Metadata)?
        .finalize(0, METADATA_RECORD_BYTES as u32, 0)?;
    let mutations = StagedArtifactWriter::new(&directory, generation, ArtifactRole::Mutations)?
        .finalize(0, MUTATION_RECORD_BYTES as u32, 0)?;
    Ok(StagedSegment {
        descriptor: SegmentDescriptor {
            format_version: SEGMENT_FORMAT_VERSION,
            generation,
            kind: SegmentKind::Base,
            vector_count: 0,
            mutation_count: 0,
            vectors,
            metadata,
            mutations,
        },
    })
}

struct StagedArtifactWriter {
    directory: PathBuf,
    temporary: PathBuf,
    generation: u64,
    role: ArtifactRole,
    writer: BufWriter<File>,
    hasher: Sha256,
    payload_bytes: u64,
}

impl StagedArtifactWriter {
    fn new(directory: &Path, generation: u64, role: ArtifactRole) -> FlatResult<Self> {
        let temporary = unique_temporary_path(directory, role.name());
        let file = create_new_file(&temporary)?;
        let mut writer = BufWriter::new(file);
        writer
            .write_all(&[0_u8; HEADER_BYTES])
            .map_err(|source| io_error("write flat segment header", &temporary, source))?;
        Ok(Self {
            directory: directory.to_path_buf(),
            temporary,
            generation,
            role,
            writer,
            hasher: Sha256::new(),
            payload_bytes: 0,
        })
    }

    fn write_payload(&mut self, bytes: &[u8]) -> FlatResult<()> {
        self.writer
            .write_all(bytes)
            .map_err(|source| io_error("write flat segment payload", &self.temporary, source))?;
        self.hasher.update(bytes);
        self.payload_bytes = self
            .payload_bytes
            .checked_add(u64::try_from(bytes.len()).map_err(|_| {
                FlatStoreError::InvalidInput("segment write length is too large".to_owned())
            })?)
            .ok_or_else(|| {
                FlatStoreError::InvalidInput("segment payload length overflow".to_owned())
            })?;
        Ok(())
    }

    fn finalize(
        mut self,
        record_count: u64,
        record_bytes: u32,
        dimensions: u32,
    ) -> FlatResult<ArtifactDescriptor> {
        let expected_payload = record_count
            .checked_mul(u64::from(record_bytes))
            .ok_or_else(|| {
                FlatStoreError::InvalidInput("segment payload length overflow".to_owned())
            })?;
        if self.payload_bytes != expected_payload {
            return Err(FlatStoreError::Corrupt(format!(
                "staged {} payload length does not match its records",
                self.role.name()
            )));
        }
        self.writer
            .flush()
            .map_err(|source| io_error("flush flat segment", &self.temporary, source))?;
        let digest = self.hasher.finalize();
        let digest_bytes: [u8; 32] = digest.into();
        let header = encode_header(SegmentHeader {
            magic: self.role.magic(),
            format_version: SEGMENT_FORMAT_VERSION,
            header_bytes: HEADER_BYTES as u32,
            generation: self.generation,
            record_count,
            record_bytes,
            dimensions,
            payload_bytes: self.payload_bytes,
            payload_sha256: digest_bytes,
        });
        self.writer
            .seek(SeekFrom::Start(0))
            .map_err(|source| io_error("seek flat segment", &self.temporary, source))?;
        self.writer
            .write_all(&header)
            .map_err(|source| io_error("finalize flat segment header", &self.temporary, source))?;
        self.writer
            .flush()
            .map_err(|source| io_error("flush flat segment header", &self.temporary, source))?;
        let file = self.writer.into_inner().map_err(|error| {
            io_error("finish flat segment", &self.temporary, error.into_error())
        })?;
        file.sync_all()
            .map_err(|source| io_error("sync flat segment", &self.temporary, source))?;
        drop(file);
        let digest_hex = encode_hex(&digest_bytes);
        let final_name = segment_name(self.generation, self.role, &digest_hex);
        let final_path = self.directory.join(&final_name);
        commit_unique_file(&self.temporary, &final_path)?;
        Ok(ArtifactDescriptor {
            file: final_name,
            file_bytes: HEADER_BYTES_U64 + self.payload_bytes,
            payload_sha256: digest_hex,
        })
    }
}

fn validate_publication_input(
    contract: &FlatModelContract,
    replacements: &[FlatEventReplacement],
    tombstones: &[Uuid],
) -> FlatResult<()> {
    let dimensions = usize_from_u32(contract.dimensions, "dimensions")?;
    let mut event_ids = HashSet::with_capacity(replacements.len() + tombstones.len());
    for replacement in replacements {
        if replacement.event_id.is_nil() {
            return Err(FlatStoreError::InvalidInput(
                "replacement event id must not be nil".to_owned(),
            ));
        }
        if !event_ids.insert(replacement.event_id) {
            return Err(FlatStoreError::InvalidInput(format!(
                "event {} appears more than once in one publication",
                replacement.event_id
            )));
        }
        if replacement.chunks.is_empty() {
            return Err(FlatStoreError::InvalidInput(format!(
                "replacement event {} has no chunks; use a tombstone",
                replacement.event_id
            )));
        }
        let mut chunk_indexes = HashSet::with_capacity(replacement.chunks.len());
        for chunk in &replacement.chunks {
            if !chunk_indexes.insert(chunk.chunk_index) {
                return Err(FlatStoreError::InvalidInput(format!(
                    "event {} repeats chunk index {}",
                    replacement.event_id, chunk.chunk_index
                )));
            }
            if chunk.start_char > chunk.end_char {
                return Err(FlatStoreError::InvalidInput(format!(
                    "event {} chunk {} has an inverted character range",
                    replacement.event_id, chunk.chunk_index
                )));
            }
            validate_vector(&chunk.vector, dimensions)?;
        }
    }
    for event_id in tombstones {
        if event_id.is_nil() {
            return Err(FlatStoreError::InvalidInput(
                "tombstone event id must not be nil".to_owned(),
            ));
        }
        if !event_ids.insert(*event_id) {
            return Err(FlatStoreError::InvalidInput(format!(
                "event {event_id} is both replaced and tombstoned"
            )));
        }
    }
    Ok(())
}

fn validate_vector(vector: &[f32], dimensions: usize) -> FlatResult<()> {
    if vector.len() != dimensions {
        return Err(FlatStoreError::InvalidInput(format!(
            "vector has {} dimensions, expected {dimensions}",
            vector.len()
        )));
    }
    let mut norm_squared = 0.0_f64;
    for value in vector {
        if !value.is_finite() {
            return Err(FlatStoreError::InvalidInput(
                "vector contains a non-finite component".to_owned(),
            ));
        }
        norm_squared += f64::from(*value) * f64::from(*value);
    }
    if (norm_squared - 1.0).abs() > NORMALIZED_NORM_SQUARED_TOLERANCE {
        return Err(FlatStoreError::InvalidInput(format!(
            "vector is not L2-normalized (norm squared {norm_squared})"
        )));
    }
    Ok(())
}

fn encode_vector(vector: &[f32], scratch: &mut [u8]) -> FlatResult<()> {
    let vector_bytes = vector
        .len()
        .checked_mul(4)
        .ok_or_else(|| FlatStoreError::InvalidInput("vector byte length overflow".to_owned()))?;
    if vector_bytes > scratch.len() {
        return Err(FlatStoreError::InvalidInput(
            "vector exceeds its aligned row".to_owned(),
        ));
    }
    scratch.fill(0);
    for (value, destination) in vector
        .iter()
        .zip(scratch[..vector_bytes].chunks_exact_mut(4))
    {
        destination.copy_from_slice(&value.to_le_bytes());
    }
    Ok(())
}

fn encode_metadata_record(metadata: FlatChunkMetadata) -> [u8; METADATA_RECORD_BYTES] {
    let mut record = [0_u8; METADATA_RECORD_BYTES];
    record[..16].copy_from_slice(metadata.event_id.as_bytes());
    record[16..48].copy_from_slice(metadata.source_text_hash.as_bytes());
    record[48..56].copy_from_slice(&metadata.seq.to_le_bytes());
    record[56..60].copy_from_slice(&metadata.chunk_index.to_le_bytes());
    record[60..64].copy_from_slice(&metadata.start_char.to_le_bytes());
    record[64..68].copy_from_slice(&metadata.end_char.to_le_bytes());
    record
}

fn decode_metadata_record(record: &[u8]) -> FlatChunkMetadata {
    let mut event_id = [0_u8; 16];
    event_id.copy_from_slice(&record[..16]);
    let mut hash = [0_u8; 32];
    hash.copy_from_slice(&record[16..48]);
    FlatChunkMetadata {
        event_id: Uuid::from_bytes(event_id),
        source_text_hash: FlatSourceHash::from_bytes(hash),
        seq: u64::from_le_bytes([
            record[48], record[49], record[50], record[51], record[52], record[53], record[54],
            record[55],
        ]),
        chunk_index: u32::from_le_bytes([record[56], record[57], record[58], record[59]]),
        start_char: u32::from_le_bytes([record[60], record[61], record[62], record[63]]),
        end_char: u32::from_le_bytes([record[64], record[65], record[66], record[67]]),
    }
}

fn decode_metadata_record_checked(record: &[u8]) -> FlatResult<FlatChunkMetadata> {
    let metadata = decode_metadata_record(record);
    if metadata.event_id.is_nil() || record[68..].iter().any(|byte| *byte != 0) {
        return Err(FlatStoreError::Corrupt(
            "metadata record has invalid identity or reserved bytes".to_owned(),
        ));
    }
    Ok(metadata)
}

fn encode_mutation_record(mutation: EventMutation) -> [u8; MUTATION_RECORD_BYTES] {
    let mut record = [0_u8; MUTATION_RECORD_BYTES];
    record[..16].copy_from_slice(mutation.event_id.as_bytes());
    record[16] = mutation.kind as u8;
    record
}

fn decode_mutation_record(record: &[u8]) -> FlatResult<EventMutation> {
    let mut event_id = [0_u8; 16];
    event_id.copy_from_slice(&record[..16]);
    let event_id = Uuid::from_bytes(event_id);
    if event_id.is_nil() || record[17..].iter().any(|byte| *byte != 0) {
        return Err(FlatStoreError::Corrupt(
            "mutation record has invalid identity or reserved bytes".to_owned(),
        ));
    }
    let kind = match record[16] {
        1 => MutationKind::Replace,
        2 => MutationKind::Delete,
        value => {
            return Err(FlatStoreError::Corrupt(format!(
                "mutation record has unknown kind {value}"
            )));
        }
    };
    Ok(EventMutation { event_id, kind })
}

fn encode_header(header: SegmentHeader) -> [u8; HEADER_BYTES] {
    let mut bytes = [0_u8; HEADER_BYTES];
    bytes[..8].copy_from_slice(&header.magic);
    bytes[8..12].copy_from_slice(&header.format_version.to_le_bytes());
    bytes[12..16].copy_from_slice(&header.header_bytes.to_le_bytes());
    bytes[16..24].copy_from_slice(&header.generation.to_le_bytes());
    bytes[24..32].copy_from_slice(&header.record_count.to_le_bytes());
    bytes[32..36].copy_from_slice(&header.record_bytes.to_le_bytes());
    bytes[36..40].copy_from_slice(&header.dimensions.to_le_bytes());
    bytes[40..48].copy_from_slice(&header.payload_bytes.to_le_bytes());
    bytes[48..80].copy_from_slice(&header.payload_sha256);
    bytes
}

fn decode_header(mapping: &[u8]) -> FlatResult<SegmentHeader> {
    let bytes = mapping.get(..HEADER_BYTES).ok_or_else(|| {
        FlatStoreError::Corrupt("segment is shorter than its fixed header".to_owned())
    })?;
    if bytes[80..].iter().any(|byte| *byte != 0) {
        return Err(FlatStoreError::Corrupt(
            "segment header has non-zero reserved bytes".to_owned(),
        ));
    }
    let mut magic = [0_u8; 8];
    magic.copy_from_slice(&bytes[..8]);
    let mut digest = [0_u8; 32];
    digest.copy_from_slice(&bytes[48..80]);
    Ok(SegmentHeader {
        magic,
        format_version: read_u32(bytes, 8),
        header_bytes: read_u32(bytes, 12),
        generation: read_u64(bytes, 16),
        record_count: read_u64(bytes, 24),
        record_bytes: read_u32(bytes, 32),
        dimensions: read_u32(bytes, 36),
        payload_bytes: read_u64(bytes, 40),
        payload_sha256: digest,
    })
}

fn read_u32(bytes: &[u8], start: usize) -> u32 {
    u32::from_le_bytes([
        bytes[start],
        bytes[start + 1],
        bytes[start + 2],
        bytes[start + 3],
    ])
}

fn read_u64(bytes: &[u8], start: usize) -> u64 {
    u64::from_le_bytes([
        bytes[start],
        bytes[start + 1],
        bytes[start + 2],
        bytes[start + 3],
        bytes[start + 4],
        bytes[start + 5],
        bytes[start + 6],
        bytes[start + 7],
    ])
}

fn validate_model_contract(contract: &FlatModelContract) -> FlatResult<()> {
    if contract.contract_version == 0
        || contract.dimensions == 0
        || contract.dimensions > MAX_DIMENSIONS
        || !contract.normalization.eq_ignore_ascii_case("l2")
    {
        return Err(FlatStoreError::InvalidInput(
            "model contract version/dimensions/normalization are invalid".to_owned(),
        ));
    }
    for (name, value) in [
        ("model id", &contract.model_id),
        ("model revision", &contract.model_revision),
        ("tokenizer", &contract.tokenizer),
        ("pooling", &contract.pooling),
        ("normalization", &contract.normalization),
    ] {
        if value.is_empty()
            || value.len() > MAX_CONTRACT_FIELD_BYTES
            || value.chars().any(char::is_control)
        {
            return Err(FlatStoreError::InvalidInput(format!(
                "{name} is empty, oversized, or contains control characters"
            )));
        }
    }
    let _ = vector_stride(contract.dimensions)?;
    Ok(())
}

fn vector_stride(dimensions: u32) -> FlatResult<u32> {
    let bytes = dimensions.checked_mul(4).ok_or_else(|| {
        FlatStoreError::InvalidInput("model vector byte length overflow".to_owned())
    })?;
    let alignment = VECTOR_ALIGNMENT as u32;
    bytes
        .checked_add(alignment - 1)
        .map(|value| value / alignment * alignment)
        .ok_or_else(|| FlatStoreError::InvalidInput("vector stride overflow".to_owned()))
}

fn validate_artifact_name(name: &str, generation: u64, role: ArtifactRole) -> FlatResult<()> {
    if Path::new(name).file_name().and_then(|value| value.to_str()) != Some(name)
        || name.contains('/')
        || name.contains('\\')
    {
        return Err(FlatStoreError::Corrupt(
            "segment artifact name is not a safe leaf name".to_owned(),
        ));
    }
    let prefix = format!("{SEGMENT_PREFIX}{generation:020}-{}-", role.name());
    let Some(digest) = name
        .strip_prefix(&prefix)
        .and_then(|value| value.strip_suffix(".bin"))
    else {
        return Err(FlatStoreError::Corrupt(format!(
            "segment artifact {name:?} does not match its generation/role"
        )));
    };
    if decode_sha256(digest).is_none() {
        return Err(FlatStoreError::Corrupt(format!(
            "segment artifact {name:?} has an invalid checksum suffix"
        )));
    }
    Ok(())
}

fn segment_name(generation: u64, role: ArtifactRole, digest: &str) -> String {
    format!(
        "{SEGMENT_PREFIX}{generation:020}-{}-{digest}.bin",
        role.name()
    )
}

fn manifest_name(generation: u64, digest: &str) -> String {
    format!("{MANIFEST_PREFIX}{generation:020}-{digest}.json")
}

fn parse_manifest_name(name: &str) -> Option<(u64, String)> {
    let body = name.strip_prefix(MANIFEST_PREFIX)?.strip_suffix(".json")?;
    let (generation, digest) = body.split_once('-')?;
    if generation.len() != 20 || decode_sha256(digest).is_none() {
        return None;
    }
    Some((generation.parse().ok()?, digest.to_owned()))
}

fn ensure_store_directories(root: &Path) -> FlatResult<()> {
    fs::create_dir_all(root).map_err(|source| io_error("create flat store root", root, source))?;
    ensure_real_directory(root)?;
    for directory in [manifests_directory(root), segments_directory(root)] {
        fs::create_dir_all(&directory)
            .map_err(|source| io_error("create flat store directory", &directory, source))?;
        ensure_real_directory(&directory)?;
    }
    let lock = lock_path(root);
    let file = open_lock(&lock, true)?;
    file.sync_all()
        .map_err(|source| io_error("sync flat writer lock", &lock, source))?;
    sync_directory(root)
}

fn validate_existing_store_directories(root: &Path) -> FlatResult<()> {
    ensure_real_directory(root)?;
    ensure_real_directory(&manifests_directory(root))?;
    ensure_real_directory(&segments_directory(root))?;
    let lock = lock_path(root);
    let _ = symlink_metadata_file(&lock)?;
    Ok(())
}

fn ensure_real_directory(path: &Path) -> FlatResult<()> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| io_error("stat flat store directory", path, source))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(FlatStoreError::Corrupt(format!(
            "{} is not a real directory",
            path.display()
        )));
    }
    Ok(())
}

fn symlink_metadata_file(path: &Path) -> FlatResult<fs::Metadata> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| io_error("stat flat store file", path, source))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(FlatStoreError::Corrupt(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    Ok(metadata)
}

fn manifests_directory(root: &Path) -> PathBuf {
    root.join(MANIFESTS_DIRECTORY)
}

fn segments_directory(root: &Path) -> PathBuf {
    root.join(SEGMENTS_DIRECTORY)
}

fn lock_path(root: &Path) -> PathBuf {
    root.join(WRITER_LOCK_FILE)
}

fn create_new_file(path: &Path) -> FlatResult<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| io_error("create flat temporary file", path, source))
}

fn unique_temporary_path(directory: &Path, purpose: &str) -> PathBuf {
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    directory.join(format!(
        "{TEMP_PREFIX}{purpose}-{}-{}-{sequence}",
        std::process::id(),
        unix_nanos()
    ))
}

fn commit_unique_file(temporary: &Path, final_path: &Path) -> FlatResult<()> {
    if final_path.exists() {
        return Err(FlatStoreError::Corrupt(format!(
            "immutable flat artifact already exists: {}",
            final_path.display()
        )));
    }
    fs::rename(temporary, final_path)
        .map_err(|source| io_error("publish immutable flat artifact", final_path, source))
}

fn remove_temporary_files(root: &Path) -> FlatResult<FlatRecoveryReport> {
    let mut report = FlatRecoveryReport::default();
    for directory in [manifests_directory(root), segments_directory(root)] {
        let entries = fs::read_dir(&directory)
            .map_err(|source| io_error("read flat recovery directory", &directory, source))?;
        for entry in entries {
            let entry =
                entry.map_err(|source| io_error("read flat recovery entry", &directory, source))?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !name.starts_with(TEMP_PREFIX) {
                continue;
            }
            remove_recoverable_file(
                &entry.path(),
                &mut report.removed_temporary_files,
                &mut report.retained_busy_files,
            )?;
        }
    }
    Ok(report)
}

fn cleanup_obsolete_locked(
    root: &Path,
    selected: &SelectedManifest,
) -> FlatResult<FlatRecoveryReport> {
    let mut report = FlatRecoveryReport::default();
    let active_segments = selected
        .envelope
        .manifest
        .segments
        .iter()
        .flat_map(|segment| {
            [
                segment.vectors.file.as_str(),
                segment.metadata.file.as_str(),
                segment.mutations.file.as_str(),
            ]
        })
        .collect::<HashSet<_>>();

    let manifest_directory = manifests_directory(root);
    for entry in fs::read_dir(&manifest_directory).map_err(|source| {
        io_error(
            "read flat manifest cleanup directory",
            &manifest_directory,
            source,
        )
    })? {
        let entry = entry.map_err(|source| {
            io_error(
                "read flat manifest cleanup entry",
                &manifest_directory,
                source,
            )
        })?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !name.starts_with(MANIFEST_PREFIX) || entry.path() == selected.path {
            continue;
        }
        if parse_manifest_name(&name).is_none() {
            continue;
        }
        remove_recoverable_file(
            &entry.path(),
            &mut report.removed_obsolete_manifests,
            &mut report.retained_busy_files,
        )?;
    }

    let segment_directory = segments_directory(root);
    for entry in fs::read_dir(&segment_directory).map_err(|source| {
        io_error(
            "read flat segment cleanup directory",
            &segment_directory,
            source,
        )
    })? {
        let entry = entry.map_err(|source| {
            io_error(
                "read flat segment cleanup entry",
                &segment_directory,
                source,
            )
        })?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !name.starts_with(SEGMENT_PREFIX) || active_segments.contains(name.as_str()) {
            continue;
        }
        remove_recoverable_file(
            &entry.path(),
            &mut report.removed_orphan_segments,
            &mut report.retained_busy_files,
        )?;
    }
    sync_directory(&manifest_directory)?;
    sync_directory(&segment_directory)?;
    Ok(report)
}

fn cleanup_without_manifest(root: &Path) -> FlatResult<FlatRecoveryReport> {
    let mut report = FlatRecoveryReport::default();
    let directory = segments_directory(root);
    for entry in fs::read_dir(&directory)
        .map_err(|source| io_error("read orphan flat segment directory", &directory, source))?
    {
        let entry = entry
            .map_err(|source| io_error("read orphan flat segment entry", &directory, source))?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !name.starts_with(SEGMENT_PREFIX) {
            continue;
        }
        remove_recoverable_file(
            &entry.path(),
            &mut report.removed_orphan_segments,
            &mut report.retained_busy_files,
        )?;
    }
    sync_directory(&directory)?;
    Ok(report)
}

fn remove_recoverable_file(
    path: &Path,
    removed: &mut usize,
    retained_busy: &mut usize,
) -> FlatResult<()> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| io_error("stat recoverable flat file", path, source))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(FlatStoreError::Corrupt(format!(
            "recoverable flat path {} is not a regular file",
            path.display()
        )));
    }
    match fs::remove_file(path) {
        Ok(()) => {
            *removed = removed.saturating_add(1);
            Ok(())
        }
        Err(source)
            if matches!(
                source.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::WouldBlock
            ) =>
        {
            *retained_busy = retained_busy.saturating_add(1);
            Ok(())
        }
        Err(source) => Err(io_error("remove recoverable flat file", path, source)),
    }
}

fn merge_recovery_reports(target: &mut FlatRecoveryReport, other: FlatRecoveryReport) {
    target.model_contract_reset |= other.model_contract_reset;
    target.removed_temporary_files = target
        .removed_temporary_files
        .saturating_add(other.removed_temporary_files);
    target.removed_obsolete_manifests = target
        .removed_obsolete_manifests
        .saturating_add(other.removed_obsolete_manifests);
    target.removed_orphan_segments = target
        .removed_orphan_segments
        .saturating_add(other.removed_orphan_segments);
    target.retained_busy_files = target
        .retained_busy_files
        .saturating_add(other.retained_busy_files);
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> FlatResult<()> {
    File::open(path)
        .map_err(|source| io_error("open flat directory for sync", path, source))?
        .sync_all()
        .map_err(|source| io_error("sync flat directory", path, source))
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> FlatResult<()> {
    Ok(())
}

fn next_generation(current: Option<&SelectedManifest>) -> FlatResult<u64> {
    current
        .map(|selected| selected.envelope.manifest.generation)
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| FlatStoreError::Corrupt("manifest generation overflow".to_owned()))
}

fn noop_outcome(current: Option<&SelectedManifest>) -> FlatPublishOutcome {
    FlatPublishOutcome {
        published: false,
        generation: current
            .map(|selected| selected.envelope.manifest.generation)
            .unwrap_or(0),
        generation_hash: current.map(|selected| selected.generation_hash.clone()),
        replaced_events: 0,
        deleted_events: 0,
    }
}

fn ensure_little_endian() -> FlatResult<()> {
    if cfg!(target_endian = "little") {
        Ok(())
    } else {
        Err(FlatStoreError::Unsupported(
            "memory-mapped f32 slices require a little-endian target".to_owned(),
        ))
    }
}

fn usize_from_u64(value: u64, name: &'static str) -> FlatResult<usize> {
    usize::try_from(value)
        .map_err(|_| FlatStoreError::Corrupt(format!("{name} does not fit this platform")))
}

fn usize_from_u32(value: u32, name: &'static str) -> FlatResult<usize> {
    usize::try_from(value)
        .map_err(|_| FlatStoreError::Corrupt(format!("{name} does not fit this platform")))
}

fn decode_sha256(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    let mut bytes = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_nibble(pair[0])?;
        let low = hex_nibble(pair[1])?;
        bytes[index] = high << 4 | low;
    }
    Some(bytes)
}

fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn unix_millis() -> u64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
        Err(_) => 0,
    }
}

fn unix_nanos() -> u128 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_nanos(),
        Err(_) => 0,
    }
}

fn io_error(operation: &'static str, path: &Path, source: std::io::Error) -> FlatStoreError {
    FlatStoreError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contract() -> FlatModelContract {
        FlatModelContract {
            contract_version: 2,
            model_id: "test/e5".to_owned(),
            model_revision: "revision-1".to_owned(),
            tokenizer: "tokenizer-sha256".to_owned(),
            pooling: "attention-mask-mean".to_owned(),
            dimensions: 4,
            normalization: "l2".to_owned(),
        }
    }

    fn hash(byte: u8) -> FlatSourceHash {
        FlatSourceHash::from_bytes([byte; 32])
    }

    fn chunk(index: u32, vector: [f32; 4]) -> FlatChunk {
        FlatChunk {
            chunk_index: index,
            start_char: index * 10,
            end_char: index * 10 + 9,
            vector: vector.to_vec(),
        }
    }

    fn replacement(
        event_id: Uuid,
        seq: u64,
        hash_byte: u8,
        chunks: Vec<FlatChunk>,
    ) -> FlatEventReplacement {
        FlatEventReplacement {
            event_id,
            seq,
            source_text_hash: hash(hash_byte),
            chunks,
        }
    }

    fn visible_chunks(pinned: &PinnedFlatGeneration) -> Vec<(Uuid, u64, u32, Vec<f32>)> {
        pinned
            .scan_segments()
            .iter()
            .flat_map(PinnedScanSegment::chunks)
            .map(|chunk| {
                (
                    chunk.event_id,
                    chunk.seq,
                    chunk.chunk_index,
                    chunk.vector.to_vec(),
                )
            })
            .collect()
    }

    #[test]
    fn replacement_tombstone_and_read_only_enumeration_are_exact() -> FlatResult<()> {
        let temporary = tempfile::tempdir()
            .map_err(|source| io_error("create test directory", Path::new("."), source))?;
        let store = FlatSegmentStore::open(temporary.path(), contract())?;
        let first = Uuid::from_u128(1);
        let second = Uuid::from_u128(2);
        store.publish_replacement_event_chunks(
            &[
                replacement(
                    first,
                    10,
                    1,
                    vec![
                        chunk(0, [1.0, 0.0, 0.0, 0.0]),
                        chunk(1, [0.0, 1.0, 0.0, 0.0]),
                    ],
                ),
                replacement(second, 20, 2, vec![chunk(0, [0.0, 0.0, 1.0, 0.0])]),
            ],
            &[],
        )?;
        store.publish_replacement_event_chunks(
            &[replacement(
                first,
                30,
                3,
                vec![chunk(7, [0.0, 0.0, 0.0, 1.0])],
            )],
            &[second],
        )?;

        let read_only = FlatSegmentStore::open_read_only(temporary.path(), contract())?;
        let pinned = read_only
            .pin_generation()?
            .ok_or_else(|| FlatStoreError::Corrupt("expected a published generation".to_owned()))?;
        assert_eq!(pinned.generation(), 2);
        assert_eq!(pinned.stats().active_events, 1);
        assert_eq!(pinned.stats().active_chunks, 1);
        assert_eq!(pinned.stats().deleted_events, 1);
        assert_eq!(
            pinned.active_events(),
            &[FlatActiveEvent {
                event_id: first,
                seq: 30,
                source_text_hash: hash(3),
                chunk_count: 1,
            }]
        );
        assert_eq!(
            visible_chunks(&pinned),
            vec![(first, 30, 7, vec![0.0, 0.0, 0.0, 1.0])]
        );
        let active_vector = pinned
            .scan_segments()
            .iter()
            .flat_map(PinnedScanSegment::chunks)
            .next()
            .ok_or_else(|| FlatStoreError::Corrupt("expected active vector".to_owned()))?
            .vector;
        assert_eq!(active_vector.as_ptr() as usize % VECTOR_ALIGNMENT, 0);
        assert!(matches!(
            read_only.delete_events(&[first]),
            Err(FlatStoreError::ReadOnly)
        ));
        Ok(())
    }

    #[test]
    fn compaction_is_sequential_and_old_pin_remains_readable() -> FlatResult<()> {
        let temporary = tempfile::tempdir()
            .map_err(|source| io_error("create test directory", Path::new("."), source))?;
        let store = FlatSegmentStore::open(temporary.path(), contract())?;
        // Deliberately publish descending event IDs across generations. A
        // sequential compactor must not need a corpus-wide vector reorder.
        let first = Uuid::from_u128(12);
        let second = Uuid::from_u128(11);
        store.publish_replacement_event_chunks(
            &[replacement(
                first,
                120,
                1,
                vec![chunk(0, [1.0, 0.0, 0.0, 0.0])],
            )],
            &[],
        )?;
        let old_pin = store
            .pin_generation()?
            .ok_or_else(|| FlatStoreError::Corrupt("expected first generation".to_owned()))?;
        store.publish_replacement_event_chunks(
            &[replacement(
                second,
                110,
                2,
                vec![chunk(0, [0.0, 1.0, 0.0, 0.0])],
            )],
            &[],
        )?;
        let before = visible_chunks(
            &store
                .pin_generation()?
                .ok_or_else(|| FlatStoreError::Corrupt("expected generation".to_owned()))?,
        );
        let compacted = store.compact()?;
        assert!(compacted.published);
        let current = store
            .pin_generation()?
            .ok_or_else(|| FlatStoreError::Corrupt("expected compacted generation".to_owned()))?;
        assert_eq!(current.scan_segments().len(), 1);
        assert_eq!(visible_chunks(&current), before);
        assert_eq!(
            visible_chunks(&old_pin),
            vec![(first, 120, 0, vec![1.0, 0.0, 0.0, 0.0])]
        );
        Ok(())
    }

    #[test]
    fn restart_removes_only_owned_temporary_and_orphan_files() -> FlatResult<()> {
        let temporary = tempfile::tempdir()
            .map_err(|source| io_error("create test directory", Path::new("."), source))?;
        let root = temporary.path();
        let store = FlatSegmentStore::open(root, contract())?;
        let event = Uuid::from_u128(21);
        store.publish_replacement_event_chunks(
            &[replacement(
                event,
                210,
                1,
                vec![chunk(0, [1.0, 0.0, 0.0, 0.0])],
            )],
            &[],
        )?;
        let state_path = root.join("state.sqlite");
        fs::write(&state_path, b"parent-owned")
            .map_err(|source| io_error("write parent state fixture", &state_path, source))?;
        let temporary_segment = segments_directory(root).join(format!("{TEMP_PREFIX}crash"));
        fs::write(&temporary_segment, b"partial")
            .map_err(|source| io_error("write temporary fixture", &temporary_segment, source))?;
        let unknown = segments_directory(root).join("parent-owned.file");
        fs::write(&unknown, b"keep")
            .map_err(|source| io_error("write unknown fixture", &unknown, source))?;

        let reopened = FlatSegmentStore::open(root, contract())?;
        assert_eq!(reopened.recovery_report().removed_temporary_files, 1);
        assert_eq!(
            fs::read(&state_path).map_err(|source| io_error(
                "read parent state fixture",
                &state_path,
                source
            ))?,
            b"parent-owned"
        );
        assert!(unknown.exists());
        assert!(!temporary_segment.exists());
        assert_eq!(reopened.active_stats()?.active_events, 1);
        Ok(())
    }

    #[test]
    fn interrupted_segment_commit_keeps_previous_manifest_active() -> FlatResult<()> {
        let temporary = tempfile::tempdir()
            .map_err(|source| io_error("create test directory", Path::new("."), source))?;
        let root = temporary.path();
        let store = FlatSegmentStore::open(root, contract())?;
        let first = Uuid::from_u128(25);
        store.publish_replacement_event_chunks(
            &[replacement(
                first,
                250,
                1,
                vec![chunk(0, [1.0, 0.0, 0.0, 0.0])],
            )],
            &[],
        )?;

        let orphan = Uuid::from_u128(26);
        let _staged = write_replacement_segment(
            root,
            &contract(),
            2,
            &[replacement(
                orphan,
                260,
                2,
                vec![chunk(0, [0.0, 1.0, 0.0, 0.0])],
            )],
            &[],
        )?;
        sync_directory(&segments_directory(root))?;
        drop(store);

        let reopened = FlatSegmentStore::open(root, contract())?;
        let pinned = reopened
            .pin_generation()?
            .ok_or_else(|| FlatStoreError::Corrupt("expected prior generation".to_owned()))?;
        assert_eq!(pinned.generation(), 1);
        assert_eq!(
            visible_chunks(&pinned),
            vec![(first, 250, 0, vec![1.0, 0.0, 0.0, 0.0])]
        );
        assert_eq!(reopened.recovery_report().removed_orphan_segments, 3);
        Ok(())
    }

    #[test]
    fn corruption_is_rejected_and_model_change_atomically_resets_empty() -> FlatResult<()> {
        let temporary = tempfile::tempdir()
            .map_err(|source| io_error("create test directory", Path::new("."), source))?;
        let root = temporary.path();
        let store = FlatSegmentStore::open(root, contract())?;
        let event = Uuid::from_u128(31);
        store.publish_replacement_event_chunks(
            &[replacement(
                event,
                310,
                1,
                vec![chunk(0, [1.0, 0.0, 0.0, 0.0])],
            )],
            &[],
        )?;
        let selected = select_manifest(root, &contract())?
            .ok_or_else(|| FlatStoreError::Corrupt("expected manifest fixture".to_owned()))?;
        let vector_path =
            segments_directory(root).join(&selected.envelope.manifest.segments[0].vectors.file);
        let mut file = OpenOptions::new()
            .write(true)
            .open(&vector_path)
            .map_err(|source| io_error("open corrupt vector fixture", &vector_path, source))?;
        file.seek(SeekFrom::Start(HEADER_BYTES_U64))
            .map_err(|source| io_error("seek corrupt vector fixture", &vector_path, source))?;
        file.write_all(&[1])
            .map_err(|source| io_error("write corrupt vector fixture", &vector_path, source))?;
        file.sync_all()
            .map_err(|source| io_error("sync corrupt vector fixture", &vector_path, source))?;
        drop(file);
        assert!(matches!(
            FlatSegmentStore::open_read_only(root, contract()),
            Err(FlatStoreError::Corrupt(_))
        ));

        let other = tempfile::tempdir()
            .map_err(|source| io_error("create second test directory", Path::new("."), source))?;
        let store = FlatSegmentStore::open(other.path(), contract())?;
        store.publish_replacement_event_chunks(
            &[replacement(
                event,
                310,
                1,
                vec![chunk(0, [1.0, 0.0, 0.0, 0.0])],
            )],
            &[],
        )?;
        let old_pin = store
            .pin_generation()?
            .ok_or_else(|| FlatStoreError::Corrupt("expected old model generation".to_owned()))?;
        let mut changed = contract();
        changed.model_revision = "revision-2".to_owned();
        assert!(matches!(
            FlatSegmentStore::open_read_only(other.path(), changed.clone()),
            Err(FlatStoreError::Incompatible(_))
        ));
        let _interrupted_reset =
            write_empty_base_segment(other.path(), &changed, old_pin.generation() + 1)?;
        sync_directory(&segments_directory(other.path()))?;
        let reset = FlatSegmentStore::open(other.path(), changed.clone())?;
        assert!(reset.recovery_report().model_contract_reset);
        assert!(reset.recovery_report().removed_orphan_segments >= 3);
        assert_eq!(reset.active_stats()?.active_events, 0);
        let reset_pin = reset
            .pin_generation()?
            .ok_or_else(|| FlatStoreError::Corrupt("expected empty reset generation".to_owned()))?;
        assert_eq!(reset_pin.generation(), 2);
        assert!(visible_chunks(&reset_pin).is_empty());
        assert_eq!(
            visible_chunks(&old_pin),
            vec![(event, 310, 0, vec![1.0, 0.0, 0.0, 0.0])]
        );
        assert!(matches!(
            store.pin_generation(),
            Err(FlatStoreError::Incompatible(_))
        ));
        let read_only = FlatSegmentStore::open_read_only(other.path(), changed)?;
        assert_eq!(read_only.active_stats()?.active_events, 0);
        Ok(())
    }

    #[test]
    fn manifest_checksum_corruption_is_rejected() -> FlatResult<()> {
        let temporary = tempfile::tempdir()
            .map_err(|source| io_error("create test directory", Path::new("."), source))?;
        let root = temporary.path();
        let store = FlatSegmentStore::open(root, contract())?;
        let event = Uuid::from_u128(35);
        store.publish_replacement_event_chunks(
            &[replacement(
                event,
                350,
                1,
                vec![chunk(0, [1.0, 0.0, 0.0, 0.0])],
            )],
            &[],
        )?;
        let selected = select_manifest(root, &contract())?
            .ok_or_else(|| FlatStoreError::Corrupt("expected manifest fixture".to_owned()))?;
        let mut envelope = read_manifest(&selected.path)?;
        envelope.manifest.created_unix_millis =
            envelope.manifest.created_unix_millis.saturating_add(1);
        let bytes = serde_json::to_vec(&envelope)?;
        fs::write(&selected.path, bytes)
            .map_err(|source| io_error("corrupt manifest fixture", &selected.path, source))?;
        assert!(matches!(
            FlatSegmentStore::open_read_only(root, contract()),
            Err(FlatStoreError::Corrupt(_))
        ));
        Ok(())
    }

    #[test]
    fn invalid_vectors_and_ambiguous_mutations_never_publish() -> FlatResult<()> {
        let temporary = tempfile::tempdir()
            .map_err(|source| io_error("create test directory", Path::new("."), source))?;
        let store = FlatSegmentStore::open(temporary.path(), contract())?;
        let event = Uuid::from_u128(41);
        let invalid = replacement(event, 410, 1, vec![chunk(0, [1.0, 1.0, 0.0, 0.0])]);
        assert!(matches!(
            store.publish_replacement_event_chunks(&[invalid], &[]),
            Err(FlatStoreError::InvalidInput(_))
        ));
        let valid = replacement(event, 410, 1, vec![chunk(0, [1.0, 0.0, 0.0, 0.0])]);
        assert!(matches!(
            store.publish_replacement_event_chunks(&[valid], &[event]),
            Err(FlatStoreError::InvalidInput(_))
        ));
        assert_eq!(store.active_hash()?, None);
        Ok(())
    }
}
