use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::Path,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[cfg(test)]
use super::source::{CodexCheckpointGeneration, CodexSourceIdentity};
use super::{
    checkpoint::{
        CodexNativeCheckpoint, CodexPendingToolAuthority, MAX_CODEX_TOOL_CALL_ID_BYTES,
        MAX_CODEX_TOOL_CONTEXTS,
    },
    record::{
        classify_codex_record, parse_decoded_record, parse_session_meta, CodexRecordClass,
        CodexRecordProbe, CodexResultKind,
    },
    rows::{
        attach_complete_message_locator, build_event_row, build_source_backed_event_row,
        build_source_backed_sparse_output_row, build_sparse_output_row, tool_context_from_row,
        CodexEventRow, CodexRetainedNonMaterialized, CodexSessionRow, CodexSourceBackedRowV0,
    },
    source::{CodexAppendProof, CodexCatalogSource, CodexFileObservation},
};
use crate::{
    observe_ordinary_file,
    provider::codex::events::{codex_is_command_tool, codex_result_content, CodexToolCallContext},
    provider::file_touches::{
        event_type_supports_structured_file_touches, visit_provider_file_touch_drafts_with_limit,
        MAX_PACKED_PROVIDER_EVENT_INDEX, MAX_PROVIDER_FILE_TOUCHES_PER_EVENT,
        PROVIDER_FILE_TOUCH_LIMIT_REJECTION,
    },
    provider_sources::open_ordinary_file_without_following,
    CaptureError, OutputAssociations, OutputCommandContext, OutputNativeCoordinate,
    OutputObservationKind, OutputOutcome, OutputOutcomeMetadata, OutputSourceLocator,
    ProOutputObservation, Result,
};

const MAX_REJECTION_DETAILS: usize = 32;
const CHECKPOINT_READ_BUFFER_BYTES: usize = 64 * 1024;
const MAX_CODEX_PAGE_UNITS: usize = 64;
const MAX_CODEX_OUTPUT_LOCATOR_BYTES: usize = 8 * 1024;
const PAGE_FIXED_WIRE_BYTES: usize = 4 * 1024;
const PRO_OUTPUT_FIXED_WIRE_BYTES: usize = 4 * 1024;
const MAX_CODEX_TOOL_NAME_BYTES: usize = 512;
const MAX_CODEX_TOOL_PREVIEW_BYTES: usize = 4 * 1024;

pub(crate) const MAX_CODEX_RECORD_BYTES: usize = 16 * 1024 * 1024;
#[cfg(test)]
pub(crate) const MAX_CODEX_PAGE_ROWS: usize = MAX_CODEX_PAGE_UNITS;
pub(crate) const MAX_CODEX_PAGE_BYTES: usize = 8 * 1024 * 1024;
const CODEX_CORE_PAGE_IDENTITY_DOMAIN: &[u8] = b"ctx/codex-nativepath/core-page/v1\0";
const CODEX_SOURCE_BACKED_PAGE_IDENTITY_DOMAIN: &[u8] =
    b"ctx/codex-nativepath/source-backed-page/v0\0";
const CODEX_PRO_PAGE_IDENTITY_DOMAIN: &[u8] = b"ctx/codex-nativepath/pro-page/v1\0";
// These stay wire-identical to provider_sources::ordinary_file so a catalog
// observation can be certified against identity read from the scanner's handle.
const ORDINARY_FILE_TOKEN_DOMAIN: &[u8] = b"ctx-ordinary-file-observation-v2\0";
const ORDINARY_FILE_FULL_FINGERPRINT_MAX_BYTES: u64 = 64 * 1024;
const ORDINARY_FILE_SPARSE_SAMPLE_BYTES: u64 = 8 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodexPublicationProfile {
    CoreOnly,
    CoreAndPro,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CodexProjectionMode {
    Legacy,
    SourceBackedV0,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CodexNativeProfile {
    publication: CodexPublicationProfile,
    projection: CodexProjectionMode,
}

#[allow(non_upper_case_globals)]
impl CodexNativeProfile {
    pub(crate) const CoreOnly: Self = Self {
        publication: CodexPublicationProfile::CoreOnly,
        projection: CodexProjectionMode::Legacy,
    };
    pub(crate) const CoreAndPro: Self = Self {
        publication: CodexPublicationProfile::CoreAndPro,
        projection: CodexProjectionMode::Legacy,
    };

    const fn source_backed_v0() -> Self {
        Self {
            publication: CodexPublicationProfile::CoreOnly,
            projection: CodexProjectionMode::SourceBackedV0,
        }
    }

    pub(super) const fn is_core_only(self) -> bool {
        matches!(self.publication, CodexPublicationProfile::CoreOnly)
    }

    pub(super) const fn projection_mode(self) -> CodexProjectionMode {
        self.projection
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodexParseDisposition {
    FullGeneration,
    AppendDelta,
    ObservationReplay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrefixProof {
    NotAttempted,
    Matched,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexRecordRejection {
    pub(crate) raw_ordinal: u64,
    pub(crate) start_byte: u64,
    pub(crate) end_byte: u64,
    pub(crate) reason: &'static str,
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
    /// Actual structural parse attempts, including a record retried after page rollback.
    pub(crate) structural_json_parses: u64,
    /// Actual typed parse attempts, including a record retried after page rollback.
    pub(crate) typed_json_parses: u64,
    pub(crate) structural_output_probes: u64,
    pub(crate) typed_output_parses: u64,
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
    pub(crate) pro_output_pages_emitted: u64,
    pub(crate) peak_pro_page_rows: usize,
    pub(crate) peak_pro_page_bytes: usize,
    pub(crate) result_body_bytes_decoded_or_allocated: u64,
    pub(crate) result_hashes_created: u64,
    pub(crate) result_previews_created: u64,
    pub(crate) result_touches_created: u64,
    pub(crate) result_fts_rows_created: u64,
    pub(crate) result_handoffs_created: u64,
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
    pub(crate) identity: CodexNativePageIdentity,
    pub(crate) owner: Option<CodexSessionRow>,
    projection_mode: CodexProjectionMode,
    pub(crate) expected_frontier: CodexNativeFrontier,
    pub(crate) next_safe_frontier: CodexNativeFrontier,
    pub(crate) core_rows: Vec<CodexEventRow>,
    pub(crate) source_backed_rows: Vec<CodexSourceBackedRowV0>,
    pub(crate) serialized_bytes: usize,
    pub(crate) physical_records: u64,
    pub(crate) terminal: bool,
}

impl CodexNativePage {
    pub(crate) fn mutation_units(&self) -> usize {
        self.core_rows
            .iter()
            .map(CodexEventRow::mutation_units)
            .sum::<usize>()
            .saturating_add(self.source_backed_rows.len())
    }

    fn units(&self) -> usize {
        self.mutation_units()
    }

    fn has_progress(&self) -> bool {
        self.physical_records != 0
    }

    pub(crate) fn receipt(&self) -> CodexNativePageReceipt {
        CodexNativePageReceipt {
            identity: self.identity,
            expected_frontier: self.expected_frontier.clone(),
            committed_frontier: self.next_safe_frontier.clone(),
            accepted_core_rows: self
                .core_rows
                .len()
                .saturating_add(self.source_backed_rows.len()),
            accepted_physical_records: self.physical_records,
        }
    }

    pub(crate) fn recompute_identity(&mut self) -> Result<()> {
        self.identity = core_page_identity(self)?.0;
        Ok(())
    }

    pub(crate) fn cursor_only(
        owner: CodexSessionRow,
        frontier: CodexNativeFrontier,
        terminal: bool,
    ) -> Result<Self> {
        let mut page = Self {
            identity: CodexNativePageIdentity::default(),
            serialized_bytes: PAGE_FIXED_WIRE_BYTES.saturating_add(serialized_owner_bytes(&owner)?),
            owner: Some(owner),
            projection_mode: CodexProjectionMode::Legacy,
            expected_frontier: frontier.clone(),
            next_safe_frontier: frontier,
            core_rows: Vec::new(),
            source_backed_rows: Vec::new(),
            physical_records: 0,
            terminal,
        };
        page.recompute_identity()?;
        Ok(page)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CodexNativePageIdentity([u8; 32]);

impl CodexNativePageIdentity {
    pub(crate) const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexNativePageReceipt {
    pub(crate) identity: CodexNativePageIdentity,
    pub(crate) expected_frontier: CodexNativeFrontier,
    pub(crate) committed_frontier: CodexNativeFrontier,
    pub(crate) accepted_core_rows: usize,
    pub(crate) accepted_physical_records: u64,
}

/// One independently bounded transient-output page.
///
/// Pro units and bytes never participate in Core page accounting or frontiers.
#[derive(Debug)]
pub(crate) struct CodexNativeProOutputPage {
    pub(crate) identity: CodexNativeProOutputPageIdentity,
    pub(crate) expected_frontier: CodexNativeFrontier,
    pub(crate) next_safe_frontier: CodexNativeFrontier,
    pub(crate) outputs: Vec<ProOutputObservation>,
    pub(crate) serialized_bytes: usize,
}

impl CodexNativeProOutputPage {
    fn units(&self) -> usize {
        self.outputs.len()
    }

    #[cfg(test)]
    pub(crate) fn receipt(&self) -> CodexNativeProOutputPageReceipt {
        CodexNativeProOutputPageReceipt {
            identity: self.identity,
            expected_frontier: self.expected_frontier.clone(),
            committed_frontier: self.next_safe_frontier.clone(),
            accepted_outputs: self.outputs.len(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CodexNativeProOutputPageIdentity([u8; 32]);

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CodexNativeProOutputPageReceipt {
    pub(crate) identity: CodexNativeProOutputPageIdentity,
    pub(crate) expected_frontier: CodexNativeFrontier,
    pub(crate) committed_frontier: CodexNativeFrontier,
    pub(crate) accepted_outputs: usize,
}

#[derive(Debug)]
pub(crate) enum CodexNativeOwnedPage {
    Core(Box<CodexNativePage>),
    Pro(Box<CodexNativeProOutputPage>),
}

#[derive(Debug)]
pub(crate) struct CodexSourceScan {
    pub(crate) source: CodexCatalogSource,
    pub(crate) before_observation: CodexFileObservation,
    pub(crate) after_observation: CodexFileObservation,
    pub(crate) disposition: CodexParseDisposition,
    prefix_proof: PrefixProof,
    resume_proof: Option<CodexAppendProof>,
    pub(crate) full_revision_sha256: [u8; 32],
    pub(crate) complete_prefix_sha256: [u8; 32],
    pub(crate) complete_prefix_end: u64,
    pub(crate) next_raw_ordinal: u64,
    pub(crate) owner: Option<CodexSessionRow>,
    pending_tool_authorities: Vec<CodexPendingToolAuthority>,
    pub(crate) rejections: Vec<CodexRecordRejection>,
    pub(crate) incomplete_tail: Option<CodexIncompleteTail>,
    pub(crate) counters: CodexScanCounters,
}

impl CodexSourceScan {
    pub(crate) fn terminal(&self) -> bool {
        self.incomplete_tail.is_none()
    }

    pub(crate) fn prefix_proof_matches(&self) -> bool {
        self.prefix_proof == PrefixProof::Matched
    }

    pub(crate) fn is_observation_replay(&self) -> bool {
        self.disposition == CodexParseDisposition::ObservationReplay
    }

    pub(crate) fn resume_proof(&self) -> Option<&CodexAppendProof> {
        self.resume_proof.as_ref()
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
    before: CodexFileObservation,
    reader: BufReader<File>,
    profile: CodexNativeProfile,
    disposition: CodexParseDisposition,
    prefix_proof: PrefixProof,
    resume_proof: Option<CodexAppendProof>,
    offset: u64,
    raw_ordinal: u64,
    owner: Option<CodexSessionRow>,
    tool_contexts: BTreeMap<String, CodexToolCallContext>,
    tool_authorities: BTreeMap<String, CodexPendingToolAuthority>,
    complete_hasher: Sha256,
    full_hasher: Sha256,
    record_buffer: Vec<u8>,
    rejections: Vec<CodexRecordRejection>,
    incomplete_tail: Option<CodexIncompleteTail>,
    counters: CodexScanCounters,
    replay: Option<CodexSourceScan>,
    active_core_page: Option<CodexNativePage>,
    pro_page: Option<CodexNativeProOutputPage>,
    ready_core_page: Option<CodexNativePage>,
    ready_pro_page: Option<CodexNativeProOutputPage>,
    exhausted: bool,
}

impl CodexNativeScanner {
    pub(crate) fn new_source_backed_v0(
        source: CodexCatalogSource,
        proof: Option<&CodexAppendProof>,
    ) -> Result<Self> {
        Self::new(source, proof, CodexNativeProfile::source_backed_v0())
    }
}

struct ScannerPosition {
    offset: u64,
    raw_ordinal: u64,
    had_owner: bool,
    complete_hasher: Sha256,
    full_hasher: Sha256,
    rejection_len: usize,
    counters: CodexScanCounters,
}

#[derive(Default)]
struct CodexRecordProjection {
    core_row: Option<CodexEventRow>,
    pro_output: Option<ProOutputObservation>,
    context_mutation: Option<CodexContextMutation>,
    source_backed_units: usize,
    core_serialized_bytes: usize,
    pro_serialized_bytes: usize,
}

impl CodexRecordProjection {
    fn core_units(&self) -> usize {
        self.core_row
            .as_ref()
            .map(CodexEventRow::mutation_units)
            .unwrap_or_default()
            .saturating_add(self.source_backed_units)
    }
}

enum CodexContextMutation {
    Insert(String, CodexToolCallContext, CodexPendingToolAuthority),
    Remove(String),
    SourceBackedRow {
        row: CodexSourceBackedRowV0,
        insert_context: Option<(String, CodexToolCallContext, CodexPendingToolAuthority)>,
        remove_context: Option<String>,
    },
}

mod checkpoint;
mod identity;
mod page_builder;
mod project;
mod scanner;
#[cfg(test)]
mod tests;

pub(crate) use checkpoint::revalidate_codex_source_observation;
use checkpoint::*;
use identity::*;
