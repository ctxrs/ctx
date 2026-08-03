use std::{
    collections::{HashMap, HashSet},
    panic::{catch_unwind, AssertUnwindSafe},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering},
        mpsc::{self, Receiver, SyncSender},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(test)]
use ctx_history_core::CertifiedSourceDeletion;
use ctx_history_core::{
    derive_event_id, derive_session_id, CaptureProvider, CertifiedSource, CertifiedSourceAppend,
    CertifiedSourceInventory, CoreRecord, CoreRecordAnnotation, CoreRecordError,
    EventIdentityInput, NativeItemKey, NativeSessionKey, ProjectionContractError,
    ScannedSourceCounts, SessionIdentityInput, SourceAnchor, SourceFrontier,
    SourceInventoryObservation, SourceKey, SourceObservation, StableEntityId, TypedKey,
};
#[cfg(test)]
use ctx_history_index::VerifiedIndex;
use ctx_history_index::{BaseEventIdentityLookup, GenerationWriter, IndexError};
#[cfg(test)]
use ctx_history_index::{CommitReceipt, RevalidationTarget, WriterOptions};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    discover_codex_catalog_sources,
    reader::{CodexParseDisposition, CodexScanCounters},
    rows::{CodexProviderEventIdentityV0, CodexSourceBackedRowV0},
    source::{CodexCatalogSource, CodexFileObservation, CodexSourceIdentity},
    CodexAppendProof, CodexCheckpointGeneration, CodexNativeCheckpoint, CodexNativeOwnedPage,
    CodexNativeScanner, CodexSessionRow, CodexSourceScan,
};
#[cfg(test)]
use crate::provider::codex::catalog::discover_codex_session_catalog;
use crate::{
    common::io::{
        open_provider_source_file, OpenedProviderSourcePath, ProviderSourceRoot,
        PROVIDER_JSONL_INVENTORY_MAX_DEPTH, PROVIDER_JSONL_INVENTORY_MAX_DIRECTORIES,
        PROVIDER_JSONL_INVENTORY_MAX_METADATA_ENTRIES, PROVIDER_JSONL_INVENTORY_MAX_PATH_BYTES,
    },
    provider::codex::{
        catalog::catalog_codex_explicit_session_opened,
        nativepath::{opened_codex_file_observation, revalidate_codex_source_observation},
    },
    CaptureError, CODEX_SESSION_SOURCE_FORMAT,
};

const CODEX_SOURCE_ANCHOR_NAMESPACE: &str = "codex.session";
const CODEX_NATIVE_SESSION_NAMESPACE: &str = "codex.session";
const CODEX_LOGICAL_SESSION_KIND: &str = "codex-session";
const CODEX_LOGICAL_EVENT_KIND: &str = "codex-event";
const CODEX_SOURCE_SCHEMA_VARIANT: &str = "codex-nativepath-jsonl-v0";
const CODEX_SOURCE_REVISION_KIND: &str = "codex-ordinary-file-observation-v1";
const CODEX_FRONTIER_KIND: &str = "codex-nativepath-checkpoint-v8";
const CODEX_PARSER_REVISION: &str = "codex-nativepath-core-record-v11";
const CODEX_INVENTORY_AUTHORITY_NAMESPACE: &str = "codex.sessions-root";
const CODEX_INVENTORY_REVISION_KIND: &str = "codex-session-tree-inventory-v1";
const CODEX_DISCOVERY_REVISION: &str = "codex-session-catalog-v1";
const CODEX_EXPLICIT_INVENTORY_AUTHORITY_NAMESPACE: &str = "codex.explicit-session-file";
const CODEX_EXPLICIT_INVENTORY_REVISION_KIND: &str = "codex-explicit-session-inventory-v1";
const CODEX_EXPLICIT_DISCOVERY_REVISION: &str = "codex-explicit-session-file-v1";
const CODEX_EXPLICIT_INVENTORY_DIGEST_DOMAIN: &[u8] = b"ctx/codex-explicit-session-inventory/v1\0";
const MAX_CODEX_SCANNER_WORKERS: usize = 16;

#[derive(Debug, Error)]
pub enum CodexSourceBackedErrorV0 {
    #[error(transparent)]
    Capture(#[from] CaptureError),
    #[error(transparent)]
    Index(#[from] IndexError),
    #[error(transparent)]
    Projection(#[from] ProjectionContractError),
    #[error(transparent)]
    CoreRecord(#[from] CoreRecordError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("Codex catalog discovery rejected {rejected} sources and failed {failed} sources")]
    IncompleteCatalog { rejected: usize, failed: usize },
    #[error("Codex catalog source {path:?} has no native session ID")]
    MissingNativeSessionId { path: PathBuf },
    #[error("Codex native session ID {0:?} resolves to more than one source")]
    DuplicateNativeSessionId(String),
    #[error("Codex source {0:?} is not a cold source or exact append")]
    UnsupportedLifecycle(String),
    #[error("Codex source certificate has no NativePath checkpoint frontier")]
    MissingCheckpoint,
    #[error("Codex source certificate has an unsupported checkpoint kind or payload")]
    InvalidCheckpoint,
    #[error("Codex scanner emitted a row without lexical body text")]
    MissingLexicalBody,
    #[error("Codex scanner emitted a row without its native session owner")]
    MissingPageOwner,
    #[error("Codex scanner owner {actual:?} does not match catalog owner {expected:?}")]
    OwnerMismatch { expected: String, actual: String },
    #[error("Codex scan counters do not reconcile with streamed Core records")]
    ScanCountMismatch,
    #[error("Codex source count overflow")]
    CountOverflow,
    #[error("Codex lineage working set exceeded its bounded task-local capacity")]
    LineageWorkingSetExhausted,
    #[error("Codex lineage working set is unavailable")]
    LineageWorkingSetUnavailable,
    #[error("Codex cold scanner lane {lane} disconnected before completing its sources")]
    ColdLaneDisconnected { lane: usize },
    #[error("Codex cold scanner lane {lane} panicked")]
    ColdWorkerPanicked { lane: usize },
    #[error("Codex cold scanner protocol mismatch: {0}")]
    ColdProtocolMismatch(&'static str),
    #[cfg(test)]
    #[error("injected Codex cold scanner failure for source {source_index}")]
    InjectedColdWorkerFailure { source_index: usize },
    #[error("Codex source-backed scanner emitted a legacy Core publication row")]
    UnexpectedLegacyRow,
    #[error("explicit Codex session source changed its native session identity")]
    ExplicitSourceIdentityChanged,
    #[error("explicit Codex session inventory changed while it was being certified")]
    ExplicitInventoryChanged,
}

#[derive(Debug, Clone)]
pub(crate) struct CodexTerminalSourceEvidenceV0 {
    pub(crate) source: CodexCatalogSource,
    pub(crate) observation: CodexFileObservation,
    pub(crate) certified_len: u64,
    pub(crate) full_revision_sha256: [u8; 32],
}

impl CodexTerminalSourceEvidenceV0 {
    pub(crate) fn new(
        mut source: CodexCatalogSource,
        observation: CodexFileObservation,
        certified_len: u64,
        full_revision_sha256: [u8; 32],
    ) -> Self {
        // The retained root plus relative route can reopen the same ordinary
        // object without following links. Keeping one opened file per source
        // until publication exhausts ordinary process descriptor limits on
        // large provider trees.
        source.opened = None;
        Self {
            source,
            observation,
            certified_len,
            full_revision_sha256,
        }
    }

    pub(crate) fn revalidate(&self) -> bool {
        revalidate_codex_source_observation(
            &self.source,
            &self.observation,
            self.certified_len,
            self.full_revision_sha256,
        )
        .is_ok()
    }
}

pub type CodexSourceBackedResultV0<T> = Result<T, CodexSourceBackedErrorV0>;

#[derive(Debug, Clone, Copy, Default)]
pub struct CodexSourceBackedPhaseTimingsV0 {
    #[cfg(test)]
    pub discovery: Duration,
    #[cfg(test)]
    pub writer_open: Duration,
    pub scan_and_stage: Duration,
    pub scanner_worker_busy: Duration,
    pub writer_add_document: Duration,
    pub certification: Duration,
    #[cfg(test)]
    pub commit: Duration,
    #[cfg(test)]
    pub total: Duration,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CodexSourceBackedCountersV0 {
    pub catalog_sources: u64,
    pub catalog_source_bytes: u64,
    pub inventory_walks: u64,
    pub inventory_source_observations: u64,
    pub catalog_source_body_reads: u64,
    pub catalog_session_meta_parses: u64,
    pub cold_sources: u64,
    pub appended_sources: u64,
    pub replaced_sources: u64,
    pub replayed_sources: u64,
    pub deleted_sources: u64,
    pub writer_exact_replay_sources: u64,
    pub writer_mutated_sources: u64,
    pub scanner_workers: u64,
    pub scanner_sources_started: u64,
    pub scanner_sources_completed: u64,
    pub peak_active_scanners: u64,
    pub repository_full_git_certification_probes: u64,
    pub staged_documents: u64,
    pub complete_records_scanned: u64,
    pub retained_records_scanned: u64,
    pub rejected_records_scanned: u64,
    pub ignored_records_scanned: u64,
    pub scanner_bytes_read: u64,
    pub checkpoint_validation_bytes: u64,
    pub prefiltered_records: u64,
    pub structural_json_parses: u64,
    pub typed_json_parses: u64,
    pub emitted_pages: u64,
    pub scanner_legacy_body_json_serializations: u64,
    pub scanner_legacy_row_json_serializations: u64,
    pub scanner_legacy_json_serialized_bytes: u64,
    pub scanner_legacy_normalized_payload_hashes: u64,
    pub scanner_legacy_file_touch_rows: u64,
    pub scanner_legacy_duplicate_preview_allocations: u64,
    pub scanner_legacy_page_owner_json_serializations: u64,
    pub scanner_legacy_page_identity_owner_json_serializations: u64,
    pub scanner_legacy_page_identity_row_json_serializations: u64,
}

impl CodexSourceBackedCountersV0 {
    #[cfg(test)]
    pub(crate) fn add_catalog_work(&mut self, work: CodexCatalogWorkV0) {
        self.inventory_walks = self.inventory_walks.saturating_add(work.inventory_walks);
        self.inventory_source_observations = self
            .inventory_source_observations
            .saturating_add(work.source_observations);
        self.catalog_source_body_reads = self
            .catalog_source_body_reads
            .saturating_add(work.source_body_reads);
        self.catalog_session_meta_parses = self
            .catalog_session_meta_parses
            .saturating_add(work.session_meta_parses);
    }

    fn add_scan(&mut self, scan: CodexScanCounters) {
        self.complete_records_scanned = self
            .complete_records_scanned
            .saturating_add(scan.complete_records);
        self.retained_records_scanned = self
            .retained_records_scanned
            .saturating_add(scan.retained_records);
        self.rejected_records_scanned = self
            .rejected_records_scanned
            .saturating_add(scan.rejected_complete_records);
        let classified = scan
            .retained_records
            .saturating_add(scan.rejected_complete_records);
        self.ignored_records_scanned = self
            .ignored_records_scanned
            .saturating_add(scan.complete_records.saturating_sub(classified));
        self.scanner_bytes_read = self.scanner_bytes_read.saturating_add(scan.bytes_read);
        self.checkpoint_validation_bytes = self
            .checkpoint_validation_bytes
            .saturating_add(scan.checkpoint_validation_bytes);
        self.prefiltered_records = self
            .prefiltered_records
            .saturating_add(scan.prefiltered_records);
        self.structural_json_parses = self
            .structural_json_parses
            .saturating_add(scan.structural_json_parses);
        self.typed_json_parses = self
            .typed_json_parses
            .saturating_add(scan.typed_json_parses);
        self.emitted_pages = self.emitted_pages.saturating_add(scan.emitted_pages);
        self.scanner_legacy_body_json_serializations = self
            .scanner_legacy_body_json_serializations
            .saturating_add(scan.legacy_body_json_serializations);
        self.scanner_legacy_row_json_serializations = self
            .scanner_legacy_row_json_serializations
            .saturating_add(scan.legacy_row_json_serializations);
        self.scanner_legacy_json_serialized_bytes = self
            .scanner_legacy_json_serialized_bytes
            .saturating_add(scan.legacy_json_serialized_bytes);
        self.scanner_legacy_normalized_payload_hashes = self
            .scanner_legacy_normalized_payload_hashes
            .saturating_add(scan.retained_hashes_created);
        self.scanner_legacy_file_touch_rows = self
            .scanner_legacy_file_touch_rows
            .saturating_add(scan.legacy_file_touch_rows_created);
        self.scanner_legacy_page_owner_json_serializations = self
            .scanner_legacy_page_owner_json_serializations
            .saturating_add(scan.legacy_page_owner_json_serializations);
        self.scanner_legacy_page_identity_owner_json_serializations = self
            .scanner_legacy_page_identity_owner_json_serializations
            .saturating_add(scan.legacy_page_identity_owner_json_serializations);
        self.scanner_legacy_page_identity_row_json_serializations = self
            .scanner_legacy_page_identity_row_json_serializations
            .saturating_add(scan.legacy_page_identity_row_json_serializations);
    }
}

#[cfg(test)]
#[derive(Debug, Clone)]
pub struct CodexSourceBackedIngestReceiptV0 {
    pub commit: CommitReceipt,
    pub timings: CodexSourceBackedPhaseTimingsV0,
    pub counters: CodexSourceBackedCountersV0,
}

mod catalog;
mod cold;
mod identity;
mod ingestion;
mod lineage;

use lineage::{CodexOutcomeLineageAuthorityV0, CodexOutcomeOriginV0};

#[cfg(test)]
pub(crate) use catalog::{
    discover_codex_session_tree_inventory_from_base_v0,
    discover_codex_session_tree_inventory_from_plans_v0, install_after_codex_directory_visit_hook,
    install_after_codex_metadata_inventory_hook, writer_base_sources, CodexCatalogWorkV0,
};
pub(crate) use catalog::{
    discover_codex_session_tree_inventory_v0, managed_codex_session_source,
    observe_codex_explicit_session_source_backed_v0, CodexExplicitSessionSourceBackedInputV0,
    CodexSessionTreeInventoryV0,
};
use cold::{
    cold_scanner_worker_count, ingest_codex_cold_parallel_v0, ColdIngestionTargetV0,
    ColdParallelOptionsV0,
};
#[cfg(test)]
use cold::{cold_scanner_worker_count_for_parallelism, take_cold_scanner_activity_v0};
pub(crate) use identity::source_observation;
use identity::{
    certify_scan, codex_core_record, codex_session_identity, codex_source_key, decode_append_proof,
    validate_owner, CodexEventIdentityStateV0,
};
#[cfg(test)]
use ingestion::{ingest_codex_source_backed_inner_v0, ingest_codex_source_backed_v0};
pub(crate) use ingestion::{ingest_codex_sources_serial_v0, ingest_codex_sources_v0};

#[cfg(test)]
mod tests;
