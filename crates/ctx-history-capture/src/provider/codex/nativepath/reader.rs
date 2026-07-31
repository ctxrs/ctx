use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::Path,
    sync::Arc,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[cfg(test)]
use super::source::{CodexCheckpointGeneration, CodexSourceIdentity};
use super::{
    checkpoint::{
        CodexNativeCheckpoint, CodexPendingToolAuthority, MAX_CODEX_CONTINUATION_CELL_ID_BYTES,
        MAX_CODEX_TOOL_CALL_ID_BYTES, MAX_CODEX_TOOL_CONTEXTS,
    },
    record::{
        classify_codex_record, parse_decoded_record, parse_session_meta, prefilter_codex_record,
        CodexRecordAdmission, CodexRecordClass, CodexRecordProbe, CodexResultKind,
        CodexSkipProjection,
    },
    rows::{
        build_source_backed_event_row, build_source_backed_sparse_output_row,
        source_backed_output_eligibility, CodexEventRow, CodexRetainedNonMaterialized,
        CodexSessionRow, CodexSourceBackedDocumentEligibility, CodexSourceBackedRowV0,
    },
    source::{CodexAppendProof, CodexCatalogSource, CodexFileObservation},
};
use crate::{
    common::io::{open_provider_source_file, OpenedProviderSourceFile},
    provider::codex::events::CodexToolCallContext,
    provider::file_touches::{
        event_type_supports_structured_file_touches, visit_provider_file_touch_drafts_with_limit,
        MAX_PROVIDER_FILE_TOUCHES_PER_EVENT,
    },
    CaptureError, OutputOutcome, Result,
};
#[cfg(test)]
pub(crate) use checkpoint::install_after_codex_prefix_hash_hook;
const CHECKPOINT_READ_BUFFER_BYTES: usize = 64 * 1024;
const MAX_CODEX_PAGE_UNITS: usize = 64;
const MAX_CODEX_SOURCE_BACKED_PAGE_RECORDS: u64 = 4 * 1024;
const MAX_CODEX_SOURCE_BACKED_PAGE_PROGRESS_BYTES: u64 = 32 * 1024 * 1024;
const PAGE_FIXED_WIRE_BYTES: usize = 4 * 1024;
const MAX_CODEX_TOOL_NAME_BYTES: usize = 512;
const MAX_CODEX_TOOL_PREVIEW_BYTES: usize = 4 * 1024;

pub(crate) const MAX_CODEX_RECORD_BYTES: usize = 16 * 1024 * 1024;
#[cfg(test)]
pub(crate) const MAX_CODEX_PAGE_ROWS: usize = MAX_CODEX_PAGE_UNITS;
pub(crate) const MAX_CODEX_PAGE_BYTES: usize = 8 * 1024 * 1024;
// These stay wire-identical to provider_sources::ordinary_file so a catalog
// observation can be certified against identity read from the scanner's handle.
const ORDINARY_FILE_TOKEN_DOMAIN: &[u8] = b"ctx-ordinary-file-observation-v2\0";
const ORDINARY_FILE_FULL_FINGERPRINT_MAX_BYTES: u64 = 64 * 1024;
const ORDINARY_FILE_SPARSE_SAMPLE_BYTES: u64 = 8 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodexParseDisposition {
    FullGeneration,
    AppendDelta,
    ObservationReplay,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexIncompleteTail {
    pub(crate) raw_ordinal: u64,
    pub(crate) start_byte: u64,
    pub(crate) byte_len: u64,
    pub(crate) sha256: [u8; 32],
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CodexScanCounters {
    pub(crate) bytes_read: u64,
    pub(crate) checkpoint_validation_bytes: u64,
    pub(crate) prefix_bytes_read: u64,
    pub(crate) complete_records: u64,
    pub(crate) retained_records: u64,
    pub(crate) ignored_records: u64,
    pub(crate) rejected_complete_records: u64,
    pub(crate) native_result_records: u64,
    pub(crate) native_result_record_bytes: u64,
    pub(crate) malformed_records: u64,
    pub(crate) oversized_records: u64,
    pub(crate) incomplete_records: u64,
    /// Records the pre-parse byte classifier answered without a structural parse.
    pub(crate) prefiltered_records: u64,
    /// Actual structural parse attempts, including a record retried after page rollback.
    pub(crate) structural_json_parses: u64,
    /// Actual typed parse attempts, including a record retried after page rollback.
    pub(crate) typed_json_parses: u64,
    pub(crate) structural_output_probes: u64,
    pub(crate) retained_json_parses: u64,
    pub(crate) retained_body_bytes: u64,
    pub(crate) retained_hashes_created: u64,
    pub(crate) legacy_body_json_serializations: u64,
    pub(crate) legacy_row_json_serializations: u64,
    pub(crate) legacy_json_serialized_bytes: u64,
    pub(crate) legacy_file_touch_rows_created: u64,
    pub(crate) legacy_complete_content_locators_created: u64,
    pub(crate) legacy_page_owner_json_serializations: u64,
    pub(crate) legacy_page_identity_owner_json_serializations: u64,
    pub(crate) legacy_page_identity_row_json_serializations: u64,
    pub(crate) emitted_pages: u64,
    pub(crate) peak_page_rows: usize,
    pub(crate) peak_page_bytes: usize,
    pub(crate) peak_line_buffer_bytes: usize,
}

/// A provider-private cursor at a complete JSONL-record boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CodexNativeFrontier {
    pub(crate) complete_prefix_end: u64,
    pub(crate) next_raw_ordinal: u64,
    pub(crate) complete_prefix_sha256: [u8; 32],
}

/// One owned, bounded Core page.
///
/// The scanner retains no event past `next_safe_frontier`. If a record would
/// overflow the current page, its scanner state is restored to
/// `next_safe_frontier` and that record is parsed as part of the next page.
#[derive(Debug)]
pub(crate) struct CodexNativePage {
    pub(crate) owner: Option<CodexSessionRow>,
    pub(crate) expected_frontier: CodexNativeFrontier,
    pub(crate) next_safe_frontier: CodexNativeFrontier,
    pub(crate) core_rows: Vec<CodexEventRow>,
    pub(crate) source_backed_rows: Vec<CodexSourceBackedRowV0>,
    pub(crate) serialized_bytes: usize,
    pub(crate) physical_records: u64,
    pub(crate) terminal: bool,
}

impl CodexNativePage {
    fn units(&self) -> usize {
        self.source_backed_rows.len()
    }

    fn has_progress(&self) -> bool {
        self.physical_records != 0
    }
}

#[derive(Debug)]
pub(crate) enum CodexNativeOwnedPage {
    Core(Box<CodexNativePage>),
}

#[derive(Debug)]
pub(crate) struct CodexSourceScan {
    pub(crate) source: CodexCatalogSource,
    pub(crate) before_observation: CodexFileObservation,
    pub(crate) after_observation: CodexFileObservation,
    pub(crate) disposition: CodexParseDisposition,
    pub(crate) full_revision_sha256: [u8; 32],
    pub(crate) complete_prefix_sha256: [u8; 32],
    pub(crate) complete_prefix_end: u64,
    pub(crate) next_raw_ordinal: u64,
    pub(crate) owner: Option<CodexSessionRow>,
    pending_tool_authorities: Vec<CodexPendingToolAuthority>,
    pub(crate) incomplete_tail: Option<CodexIncompleteTail>,
    pub(crate) counters: CodexScanCounters,
}

impl CodexSourceScan {
    #[cfg(test)]
    pub(crate) fn terminal(&self) -> bool {
        self.incomplete_tail.is_none()
    }

    pub(crate) fn checkpoint(&self) -> Option<CodexNativeCheckpoint> {
        Some(CodexNativeCheckpoint::new(
            self.after_observation.clone(),
            self.full_revision_sha256,
            self.complete_prefix_sha256,
            self.complete_prefix_end,
            self.next_raw_ordinal,
            self.incomplete_tail
                .as_ref()
                .map(|tail| (tail.byte_len, tail.sha256)),
            &self.pending_tool_authorities,
            self.owner.clone()?,
        ))
    }

    #[cfg(test)]
    pub(crate) fn bind_checkpoint(
        &self,
        canonical_source_key: impl Into<String>,
        generation: CodexCheckpointGeneration,
    ) -> Result<Option<CodexAppendProof>> {
        let identity = CodexSourceIdentity::new(
            canonical_source_key,
            self.source.source_root.clone(),
            self.source.source_path.clone(),
        )?;
        Ok(self
            .checkpoint()
            .map(|checkpoint| CodexAppendProof::new(identity, generation, checkpoint)))
    }
}

#[derive(Debug)]
pub(crate) struct CodexNativeScanner {
    source: CodexCatalogSource,
    opened: Arc<OpenedProviderSourceFile>,
    before: CodexFileObservation,
    frozen_len: u64,
    reader: BufReader<File>,
    disposition: CodexParseDisposition,
    offset: u64,
    raw_ordinal: u64,
    owner: Option<CodexSessionRow>,
    tool_contexts: BTreeMap<String, CodexToolCallContext>,
    tool_authorities: BTreeMap<String, CodexPendingToolAuthority>,
    continuations: BTreeMap<String, String>,
    complete_hasher: Sha256,
    full_hasher: Sha256,
    record_buffer: Vec<u8>,
    incomplete_tail: Option<CodexIncompleteTail>,
    counters: CodexScanCounters,
    replay: Option<CodexSourceScan>,
    active_core_page: Option<CodexNativePage>,
    ready_core_page: Option<CodexNativePage>,
    exhausted: bool,
}

impl CodexNativeScanner {
    pub(crate) fn new_source_backed_v0(
        source: CodexCatalogSource,
        proof: Option<&CodexAppendProof>,
    ) -> Result<Self> {
        Self::new(source, proof)
    }
}

struct ScannerPosition {
    offset: u64,
    raw_ordinal: u64,
    had_owner: bool,
    complete_hasher: Sha256,
    full_hasher: Sha256,
    counters: CodexScanCounters,
}

#[derive(Default)]
struct CodexRecordProjection {
    context_mutation: Option<CodexContextMutation>,
    source_backed_units: usize,
    core_serialized_bytes: usize,
}

impl CodexRecordProjection {
    fn core_units(&self) -> usize {
        self.source_backed_units
    }
}

// Produced once per decoded record: boxing the 296-byte source-backed mutation
// to match the 24-byte removal variant would add a per-record heap allocation.
#[allow(clippy::large_enum_variant)]
enum CodexContextMutation {
    Remove(Vec<String>),
    RegisterContinuation {
        cell_id: String,
        origin_call_id: String,
    },
    SourceBackedRow {
        row: CodexSourceBackedRowV0,
        insert_context: Option<(String, CodexToolCallContext, CodexPendingToolAuthority)>,
        remove_contexts: Vec<String>,
    },
}

mod checkpoint;
mod identity;
mod page_builder;
mod project;
mod scanner;

use checkpoint::*;
pub(crate) use checkpoint::{
    open_codex_source_capability, opened_file_observation as opened_codex_file_observation,
    revalidate_codex_source_observation,
};
use identity::*;
