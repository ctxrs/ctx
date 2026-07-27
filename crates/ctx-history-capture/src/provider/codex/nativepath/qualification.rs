use std::{cell::RefCell, path::Path};

use ctx_history_store::{JournalCheckpoint, NativePathGroupReceipt, Store};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::{
    producer::CodexProducerStats, root::import_codex_native_session_root, CodexCatalogSource,
};
use crate::{CaptureError, CodexSessionImportOptions, ProviderImportSummary, Result};

const EVIDENCE_SCHEMA_VERSION: u32 = 1;
const INPUT_FINGERPRINT_DOMAIN: &[u8] = b"ctx-codex-nativepath-qualification-input-v1";
const IMPORTER_FINGERPRINT_DOMAIN: &[u8] = b"ctx-codex-nativepath-qualification-build-v1";

thread_local! {
    static ACTIVE_CAPTURE: RefCell<Option<RuntimeCapture>> = const { RefCell::new(None) };
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CodexNativePathQualificationEvidence {
    schema_version: u32,
    build: QualificationBuildIdentity,
    input: QualificationInputIdentity,
    work_result: &'static str,
    summary: ProviderImportSummary,
    producer: QualificationProducerCounters,
    store: QualificationStoreCounters,
}

impl CodexNativePathQualificationEvidence {
    pub fn summary(&self) -> &ProviderImportSummary {
        &self.summary
    }

    pub fn producer(&self) -> &QualificationProducerCounters {
        &self.producer
    }

    pub fn store(&self) -> &QualificationStoreCounters {
        &self.store
    }

    pub fn input(&self) -> &QualificationInputIdentity {
        &self.input
    }

    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string(self).map_err(CaptureError::from)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QualificationBuildIdentity {
    source_commit: &'static str,
    cargo_lock_sha256: &'static str,
    importer_source_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QualificationInputIdentity {
    source_root: String,
    catalog_sources: u64,
    catalog_bytes: u64,
    observation_sha256: String,
}

impl QualificationInputIdentity {
    pub fn catalog_sources(&self) -> u64 {
        self.catalog_sources
    }

    pub fn catalog_bytes(&self) -> u64 {
        self.catalog_bytes
    }

    pub fn observation_sha256(&self) -> &str {
        &self.observation_sha256
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct QualificationProducerCounters {
    worker_count: u64,
    peak_overlap: u64,
    peak_preparation_bytes: u64,
    blocked_reservations: u64,
}

impl QualificationProducerCounters {
    fn zero() -> Self {
        Self {
            worker_count: 0,
            peak_overlap: 0,
            peak_preparation_bytes: 0,
            blocked_reservations: 0,
        }
    }

    pub fn worker_count(self) -> u64 {
        self.worker_count
    }

    pub fn peak_overlap(self) -> u64 {
        self.peak_overlap
    }

    pub fn peak_preparation_bytes(self) -> u64 {
        self.peak_preparation_bytes
    }

    pub fn blocked_reservations(self) -> u64 {
        self.blocked_reservations
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QualificationStoreCounters {
    groups: u64,
    mutation_units: u64,
    core_bound_bytes: u64,
    journal_records: u64,
    journal_bytes: u64,
    checkpoint_receipts: u64,
    checkpoint_advances: u64,
    first_checkpoint: Option<JournalCheckpoint>,
    last_checkpoint: Option<JournalCheckpoint>,
}

impl QualificationStoreCounters {
    fn zero() -> Self {
        Self {
            groups: 0,
            mutation_units: 0,
            core_bound_bytes: 0,
            journal_records: 0,
            journal_bytes: 0,
            checkpoint_receipts: 0,
            checkpoint_advances: 0,
            first_checkpoint: None,
            last_checkpoint: None,
        }
    }

    pub fn groups(&self) -> u64 {
        self.groups
    }

    pub fn mutation_units(&self) -> u64 {
        self.mutation_units
    }

    pub fn core_bound_bytes(&self) -> u64 {
        self.core_bound_bytes
    }

    pub fn journal_records(&self) -> u64 {
        self.journal_records
    }

    pub fn journal_bytes(&self) -> u64 {
        self.journal_bytes
    }

    pub fn checkpoint_receipts(&self) -> u64 {
        self.checkpoint_receipts
    }

    pub fn checkpoint_advances(&self) -> u64 {
        self.checkpoint_advances
    }

    pub fn first_checkpoint(&self) -> Option<&JournalCheckpoint> {
        self.first_checkpoint.as_ref()
    }

    pub fn last_checkpoint(&self) -> Option<&JournalCheckpoint> {
        self.last_checkpoint.as_ref()
    }
}

#[derive(Debug)]
struct RuntimeCapture {
    input: InputCapture,
    producer: QualificationProducerCounters,
    store: QualificationStoreCounters,
}

impl RuntimeCapture {
    fn new(source_root: &Path) -> Self {
        let mut input = InputCapture {
            source_root: source_root.display().to_string(),
            catalog_sources: 0,
            catalog_bytes: 0,
            hasher: Sha256::new(),
        };
        input.hasher.update(INPUT_FINGERPRINT_DOMAIN);
        Self {
            input,
            producer: QualificationProducerCounters::zero(),
            store: QualificationStoreCounters::zero(),
        }
    }

    fn finish(self) -> RuntimeEvidence {
        RuntimeEvidence {
            input: QualificationInputIdentity {
                source_root: self.input.source_root,
                catalog_sources: self.input.catalog_sources,
                catalog_bytes: self.input.catalog_bytes,
                observation_sha256: hex_digest(self.input.hasher.finalize()),
            },
            producer: self.producer,
            store: self.store,
        }
    }
}

#[derive(Debug)]
struct InputCapture {
    source_root: String,
    catalog_sources: u64,
    catalog_bytes: u64,
    hasher: Sha256,
}

#[derive(Debug)]
struct RuntimeEvidence {
    input: QualificationInputIdentity,
    producer: QualificationProducerCounters,
    store: QualificationStoreCounters,
}

struct CaptureGuard {
    finished: bool,
}

impl CaptureGuard {
    fn begin(source_root: &Path) -> Result<Self> {
        ACTIVE_CAPTURE.with(|active| {
            let mut active = active.borrow_mut();
            if active.is_some() {
                return Err(CaptureError::SystemInvariant(
                    "nested Codex NativePath qualification capture",
                ));
            }
            *active = Some(RuntimeCapture::new(source_root));
            Ok(Self { finished: false })
        })
    }

    fn finish(mut self) -> Result<RuntimeEvidence> {
        let capture = ACTIVE_CAPTURE.with(|active| active.borrow_mut().take());
        self.finished = true;
        capture
            .map(RuntimeCapture::finish)
            .ok_or(CaptureError::SystemInvariant(
                "Codex NativePath qualification capture disappeared",
            ))
    }
}

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        if !self.finished {
            ACTIVE_CAPTURE.with(|active| {
                active.borrow_mut().take();
            });
        }
    }
}

pub fn qualify_codex_native_session_root(
    root: impl AsRef<Path>,
    store: &mut Store,
    options: CodexSessionImportOptions,
) -> Result<CodexNativePathQualificationEvidence> {
    let root = root.as_ref();
    let source_root = options.source_path.as_deref().unwrap_or(root);
    let capture = CaptureGuard::begin(source_root)?;
    let summary = import_codex_native_session_root(root, store, options);
    let runtime = capture.finish()?;
    let summary = summary?;
    Ok(CodexNativePathQualificationEvidence {
        schema_version: EVIDENCE_SCHEMA_VERSION,
        build: build_identity(),
        input: runtime.input,
        work_result: summary.work_result().as_str(),
        summary,
        producer: runtime.producer,
        store: runtime.store,
    })
}

pub(super) fn observe_catalog_sources(source_root: &Path, sources: &[CodexCatalogSource]) {
    ACTIVE_CAPTURE.with(|active| {
        let mut active = active.borrow_mut();
        let Some(capture) = active.as_mut() else {
            return;
        };
        for source in sources {
            let relative = source
                .source_path
                .strip_prefix(source_root)
                .unwrap_or(&source.source_path)
                .to_string_lossy();
            hash_field(&mut capture.input.hasher, relative.as_bytes());
            capture
                .input
                .hasher
                .update(source.catalog_observation.len.to_le_bytes());
            capture
                .input
                .hasher
                .update(source.catalog_observation.modified_at_ms.to_le_bytes());
            capture
                .input
                .hasher
                .update(source.catalog_observation.change_token);
            capture
                .input
                .hasher
                .update([u8::from(source.source_path.is_file())]);
            capture.input.catalog_sources = capture.input.catalog_sources.saturating_add(1);
            capture.input.catalog_bytes = capture
                .input
                .catalog_bytes
                .saturating_add(source.catalog_observation.len);
        }
    });
}

pub(super) fn observe_producer_stats(stats: CodexProducerStats) {
    ACTIVE_CAPTURE.with(|active| {
        let mut active = active.borrow_mut();
        let Some(capture) = active.as_mut() else {
            return;
        };
        capture.producer.worker_count = usize_counter(stats.worker_count);
        capture.producer.peak_overlap = usize_counter(stats.max_concurrent_producers);
        capture.producer.peak_preparation_bytes = usize_counter(stats.peak_preparation_bytes);
        capture.producer.blocked_reservations = usize_counter(stats.blocked_reservations);
    });
}

pub(super) fn observe_store_receipt(receipt: &NativePathGroupReceipt) {
    ACTIVE_CAPTURE.with(|active| {
        let mut active = active.borrow_mut();
        let Some(capture) = active.as_mut() else {
            return;
        };
        let store = &mut capture.store;
        store.groups = store.groups.saturating_add(1);
        store.mutation_units = store
            .mutation_units
            .saturating_add(usize_counter(receipt.attempted_mutation_units()));
        store.core_bound_bytes = store
            .core_bound_bytes
            .saturating_add(usize_counter(receipt.core_bound_value_bytes()));
        store.journal_records = store
            .journal_records
            .saturating_add(usize_counter(receipt.journal_records()));
        store.journal_bytes = store
            .journal_bytes
            .saturating_add(usize_counter(receipt.journal_uncompressed_bytes()));
        if let Some(checkpoint) = receipt.checkpoint() {
            store.checkpoint_receipts = store.checkpoint_receipts.saturating_add(1);
            if store.last_checkpoint.as_ref() != Some(checkpoint) {
                store.checkpoint_advances = store.checkpoint_advances.saturating_add(1);
            }
            if store.first_checkpoint.is_none() {
                store.first_checkpoint = Some(checkpoint.clone());
            }
            store.last_checkpoint = Some(checkpoint.clone());
        }
    });
}

fn build_identity() -> QualificationBuildIdentity {
    let mut hasher = Sha256::new();
    hasher.update(IMPORTER_FINGERPRINT_DOMAIN);
    for source in [
        include_bytes!("root.rs").as_slice(),
        include_bytes!("producer.rs").as_slice(),
        include_bytes!("vertical.rs").as_slice(),
        include_bytes!("vertical/producer.rs").as_slice(),
        include_bytes!("vertical/publication.rs").as_slice(),
        include_bytes!("qualification.rs").as_slice(),
    ] {
        hash_field(&mut hasher, source);
    }
    QualificationBuildIdentity {
        source_commit: env!("CTX_CODEX_QUALIFICATION_SOURCE_COMMIT"),
        cargo_lock_sha256: env!("CTX_CODEX_QUALIFICATION_CARGO_LOCK_SHA256"),
        importer_source_sha256: hex_digest(hasher.finalize()),
    }
}

fn hash_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
    hasher.update(value);
}

fn usize_counter(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    use std::fmt::Write;

    bytes
        .as_ref()
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            let _ = write!(output, "{byte:02x}");
            output
        })
}
