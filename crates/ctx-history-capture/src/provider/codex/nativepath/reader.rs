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
        build_event_row, build_sparse_output_row, tool_context_from_row, CodexEventRow,
        CodexSessionRow,
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
const CODEX_PRO_PAGE_IDENTITY_DOMAIN: &[u8] = b"ctx/codex-nativepath/pro-page/v1\0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodexNativeProfile {
    CoreOnly,
    CoreAndPro,
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
    pub(crate) expected_frontier: CodexNativeFrontier,
    pub(crate) next_safe_frontier: CodexNativeFrontier,
    pub(crate) core_rows: Vec<CodexEventRow>,
    pub(crate) serialized_bytes: usize,
    pub(crate) physical_records: u64,
    pub(crate) terminal: bool,
}

impl CodexNativePage {
    pub(crate) fn mutation_units(&self) -> usize {
        self.core_rows
            .iter()
            .map(CodexEventRow::mutation_units)
            .sum()
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
            accepted_core_rows: self.core_rows.len(),
            accepted_physical_records: self.physical_records,
        }
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
    #[cfg(test)]
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
    core_serialized_bytes: usize,
    pro_serialized_bytes: usize,
}

impl CodexRecordProjection {
    fn core_units(&self) -> usize {
        self.core_row
            .as_ref()
            .map(CodexEventRow::mutation_units)
            .unwrap_or_default()
    }
}

enum CodexContextMutation {
    Insert(String, CodexToolCallContext, CodexPendingToolAuthority),
    Remove(String),
}

impl CodexNativeScanner {
    pub(crate) fn new(
        source: CodexCatalogSource,
        proof: Option<&CodexAppendProof>,
        profile: CodexNativeProfile,
    ) -> Result<Self> {
        if let Some(proof) = proof {
            proof.validate_source(&source)?;
        }

        let before = observed_file(&source)?;
        let file = open_ordinary_file_without_following(&source.source_path)?;
        validate_open_file_metadata(&file, &before)?;
        let mut reader = BufReader::new(file);
        let validated = if let Some(proof) = proof {
            if before.len < proof.checkpoint.observation.len {
                return Err(invalid_checkpoint_proof(
                    "checkpoint generation is longer than the observed source",
                ));
            }
            Some(validate_checkpoint_source(
                &mut reader,
                &proof.checkpoint,
                before.len > proof.checkpoint.observation.len,
            )?)
        } else {
            None
        };

        if let (Some(proof), Some(validated)) = (
            proof.filter(|proof| proof.checkpoint.observation == before),
            validated.as_ref(),
        ) {
            validate_catalog_owner(
                source.catalog_native_session_id.as_deref(),
                &proof.checkpoint.owner.native_session_id,
            )?;
            let incomplete_tail = proof
                .checkpoint
                .incomplete_tail()
                .map(|(byte_len, sha256)| CodexIncompleteTail {
                    raw_ordinal: proof.checkpoint.next_raw_ordinal(),
                    start_byte: proof.checkpoint.complete_prefix_end(),
                    byte_len,
                    sha256,
                });
            let replay = CodexSourceScan {
                source: source.clone(),
                before_observation: before.clone(),
                after_observation: before.clone(),
                disposition: CodexParseDisposition::ObservationReplay,
                prefix_proof: PrefixProof::Matched,
                resume_proof: Some(proof.clone()),
                full_revision_sha256: proof.checkpoint.full_revision_sha256,
                complete_prefix_sha256: proof.checkpoint.complete_prefix_sha256,
                complete_prefix_end: proof.checkpoint.complete_prefix_end(),
                next_raw_ordinal: proof.checkpoint.next_raw_ordinal(),
                owner: Some(proof.checkpoint.owner.clone()),
                pending_tool_authorities: proof.checkpoint.pending_tool_authorities().to_vec(),
                #[cfg(test)]
                rejections: Vec::new(),
                incomplete_tail,
                counters: CodexScanCounters {
                    bytes_read: validated.bytes_read,
                    checkpoint_validation_bytes: validated.bytes_read,
                    prefix_bytes_read: proof.checkpoint.complete_prefix_end(),
                    peak_line_buffer_bytes: CHECKPOINT_READ_BUFFER_BYTES
                        .min(usize::try_from(validated.bytes_read).unwrap_or(usize::MAX)),
                    ..CodexScanCounters::default()
                },
            };
            return Ok(Self {
                source,
                before,
                reader,
                profile,
                disposition: CodexParseDisposition::ObservationReplay,
                prefix_proof: PrefixProof::Matched,
                resume_proof: Some(proof.clone()),
                offset: replay.complete_prefix_end,
                raw_ordinal: replay.next_raw_ordinal,
                owner: replay.owner.clone(),
                tool_contexts: BTreeMap::new(),
                tool_authorities: BTreeMap::new(),
                complete_hasher: Sha256::new(),
                full_hasher: Sha256::new(),
                record_buffer: Vec::new(),
                rejections: Vec::new(),
                incomplete_tail: None,
                counters: replay.counters,
                replay: Some(replay),
                active_core_page: None,
                pro_page: None,
                ready_core_page: None,
                ready_pro_page: None,
                exhausted: true,
            });
        }

        let (
            disposition,
            prefix_proof,
            resume_proof,
            owner,
            tool_contexts,
            tool_authorities,
            raw_ordinal,
            offset,
            complete_hasher,
            validation_bytes,
        ) = match (proof, validated) {
            (Some(proof), Some(validated)) if before.len > proof.checkpoint.observation.len => {
                let ValidatedCheckpoint {
                    bytes_read,
                    complete_prefix_hasher,
                    pending_tool_contexts: tool_contexts,
                    pending_tool_authorities: tool_authorities,
                } = validated;
                reader.seek(SeekFrom::Start(proof.checkpoint.complete_prefix_end()))?;
                (
                    CodexParseDisposition::AppendDelta,
                    PrefixProof::Matched,
                    Some(proof.clone()),
                    Some(proof.checkpoint.owner.clone()),
                    tool_contexts,
                    tool_authorities,
                    proof.checkpoint.next_raw_ordinal(),
                    proof.checkpoint.complete_prefix_end(),
                    complete_prefix_hasher,
                    bytes_read,
                )
            }
            (Some(_), Some(_)) => {
                return Err(invalid_checkpoint_proof(
                    "checkpoint generation is neither an exact replay nor an append prefix",
                ));
            }
            (None, None) => {
                reader.seek(SeekFrom::Start(0))?;
                (
                    CodexParseDisposition::FullGeneration,
                    PrefixProof::NotAttempted,
                    None,
                    None,
                    BTreeMap::new(),
                    BTreeMap::new(),
                    0,
                    0,
                    Sha256::new(),
                    0,
                )
            }
            _ => {
                return Err(CaptureError::SystemInvariant(
                    "Codex checkpoint validation state is incomplete",
                ));
            }
        };

        let initial_frontier = CodexNativeFrontier {
            complete_prefix_end: offset,
            next_raw_ordinal: raw_ordinal,
            complete_prefix_sha256: complete_hasher.clone().finalize().into(),
        };
        Ok(Self {
            source,
            before,
            reader,
            profile,
            disposition,
            prefix_proof,
            resume_proof,
            offset,
            raw_ordinal,
            owner,
            tool_contexts,
            tool_authorities,
            complete_hasher: complete_hasher.clone(),
            full_hasher: complete_hasher,
            record_buffer: Vec::new(),
            rejections: Vec::new(),
            incomplete_tail: None,
            counters: CodexScanCounters {
                bytes_read: validation_bytes,
                checkpoint_validation_bytes: validation_bytes,
                prefix_bytes_read: offset,
                ..CodexScanCounters::default()
            },
            replay: None,
            active_core_page: None,
            pro_page: (profile == CodexNativeProfile::CoreAndPro)
                .then(|| new_pro_page(initial_frontier)),
            ready_core_page: None,
            ready_pro_page: None,
            exhausted: false,
        })
    }

    pub(crate) fn next_page(&mut self) -> Result<Option<CodexNativeOwnedPage>> {
        if let Some(page) = self.take_ready_page() {
            return Ok(Some(page));
        }
        if self.exhausted {
            return Ok(None);
        }
        if self.active_core_page.is_none() {
            self.active_core_page = Some(self.new_core_page()?);
        }

        loop {
            let core_is_full = self.active_core_page.as_ref().is_some_and(|page| {
                page.physical_records >= MAX_CODEX_PAGE_UNITS as u64
                    || page.units() >= MAX_CODEX_PAGE_UNITS
            });
            if core_is_full {
                return self.emit_active_core_page().map(Some);
            }

            let position = self.position();
            let record_start = self.offset;
            let record_read = {
                let reader = &mut self.reader;
                let record_buffer = &mut self.record_buffer;
                let full_hasher = &mut self.full_hasher;
                let complete_hasher = &mut self.complete_hasher;
                read_bounded_record(reader, record_buffer, full_hasher, complete_hasher)?
            };
            let Some(record_read) = record_read else {
                self.exhausted = true;
                self.queue_end_pages(true)?;
                return Ok(self.take_ready_page());
            };

            self.offset = self.offset.checked_add(record_read.byte_len).ok_or(
                CaptureError::SystemInvariant("Codex source offset exceeds u64"),
            )?;
            self.counters.bytes_read = self
                .counters
                .bytes_read
                .saturating_add(record_read.byte_len);
            self.counters.peak_line_buffer_bytes = self
                .counters
                .peak_line_buffer_bytes
                .max(record_read.stored_len);

            if !record_read.complete {
                self.incomplete_tail = Some(CodexIncompleteTail {
                    raw_ordinal: self.raw_ordinal,
                    start_byte: record_start,
                    byte_len: record_read.byte_len,
                    sha256: record_read.sha256,
                });
                self.counters.incomplete_records =
                    self.counters.incomplete_records.saturating_add(1);
                if record_read.oversized {
                    self.counters.oversized_records =
                        self.counters.oversized_records.saturating_add(1);
                }
                self.exhausted = true;
                self.queue_end_pages(false)?;
                return Ok(self.take_ready_page());
            }

            self.counters.complete_records = self.counters.complete_records.saturating_add(1);
            let record_end = self.offset;
            let mut projection = if record_read.oversized {
                self.reject(
                    record_start,
                    record_end,
                    "Codex JSONL record exceeds the 16 MiB provider bound",
                    true,
                );
                CodexRecordProjection::default()
            } else {
                let record_buffer = std::mem::take(&mut self.record_buffer);
                let result = self.process_record(
                    &record_buffer[..record_read.stored_len],
                    record_start,
                    record_end,
                );
                self.record_buffer = record_buffer;
                result?
            };

            let page = self
                .active_core_page
                .as_ref()
                .ok_or(CaptureError::SystemInvariant(
                    "Codex NativePath lost its active Core page",
                ))?;
            let next_units = page.units().saturating_add(projection.core_units());
            let next_bytes = page
                .serialized_bytes
                .saturating_add(projection.core_serialized_bytes);
            if next_units > MAX_CODEX_PAGE_UNITS || next_bytes > MAX_CODEX_PAGE_BYTES {
                if page.has_progress() {
                    self.restore(position)?;
                    return self.emit_active_core_page().map(Some);
                }
                self.reject(
                    record_start,
                    record_end,
                    "Codex record projection exceeds the bounded NativePath Core page",
                    false,
                );
                projection = CodexRecordProjection::default();
            } else {
                let page = self
                    .active_core_page
                    .as_mut()
                    .ok_or(CaptureError::SystemInvariant(
                        "Codex NativePath lost its active Core page",
                    ))?;
                if let Some(row) = projection.core_row.take() {
                    page.core_rows.push(row);
                }
                page.serialized_bytes = next_bytes;
            }
            if let Some(mutation) = projection.context_mutation.take() {
                self.apply_context_mutation(mutation);
            }

            self.raw_ordinal = self.raw_ordinal.saturating_add(1);
            let next_frontier = self.frontier();
            let page = self
                .active_core_page
                .as_mut()
                .ok_or(CaptureError::SystemInvariant(
                    "Codex NativePath lost its active Core page",
                ))?;
            page.physical_records = page.physical_records.saturating_add(1);
            if let Some(output) = projection.pro_output.take() {
                self.push_pro_output(output, projection.pro_serialized_bytes, next_frontier)?;
            }
            if let Some(page) = self.take_ready_page() {
                return Ok(Some(page));
            }
        }
    }

    fn new_core_page(&self) -> Result<CodexNativePage> {
        let expected_frontier = self.frontier();
        let owner_bytes = self
            .owner
            .as_ref()
            .map(serialized_owner_bytes)
            .transpose()?
            .unwrap_or_default();
        Ok(CodexNativePage {
            identity: CodexNativePageIdentity::default(),
            owner: self.owner.clone(),
            next_safe_frontier: expected_frontier.clone(),
            expected_frontier,
            core_rows: Vec::new(),
            serialized_bytes: PAGE_FIXED_WIRE_BYTES.saturating_add(owner_bytes),
            physical_records: 0,
            terminal: false,
        })
    }

    fn take_ready_page(&mut self) -> Option<CodexNativeOwnedPage> {
        self.ready_pro_page
            .take()
            .map(Box::new)
            .map(CodexNativeOwnedPage::Pro)
            .or_else(|| {
                self.ready_core_page
                    .take()
                    .map(Box::new)
                    .map(CodexNativeOwnedPage::Core)
            })
    }

    fn emit_active_core_page(&mut self) -> Result<CodexNativeOwnedPage> {
        let page = self
            .active_core_page
            .take()
            .ok_or(CaptureError::SystemInvariant(
                "Codex NativePath has no active Core page to emit",
            ))?;
        Ok(CodexNativeOwnedPage::Core(Box::new(
            self.finish_page(page)?,
        )))
    }

    fn queue_end_pages(&mut self, terminal: bool) -> Result<()> {
        if let Some(mut page) = self.active_core_page.take() {
            if page.has_progress() {
                page.terminal = terminal;
                self.ready_core_page = Some(self.finish_page(page)?);
            }
        }
        self.flush_pro_page()
    }

    fn push_pro_output(
        &mut self,
        output: ProOutputObservation,
        serialized_bytes: usize,
        next_frontier: CodexNativeFrontier,
    ) -> Result<()> {
        let page = self.pro_page.as_ref().ok_or(CaptureError::SystemInvariant(
            "Codex NativePath produced Pro output without an active Pro lane",
        ))?;
        if page.units() >= MAX_CODEX_PAGE_UNITS
            || page
                .serialized_bytes
                .checked_add(serialized_bytes)
                .is_none_or(|bytes| bytes > MAX_CODEX_PAGE_BYTES)
        {
            self.flush_pro_page()?;
        }
        let page = self.pro_page.as_mut().ok_or(CaptureError::SystemInvariant(
            "Codex NativePath lost its active Pro page",
        ))?;
        if serialized_bytes > MAX_CODEX_PAGE_BYTES
            || page.units() >= MAX_CODEX_PAGE_UNITS
            || page
                .serialized_bytes
                .checked_add(serialized_bytes)
                .is_none_or(|bytes| bytes > MAX_CODEX_PAGE_BYTES)
        {
            return Err(CaptureError::SystemInvariant(
                "Codex NativePath Pro output was pushed past an individual page bound",
            ));
        }
        page.outputs.push(output);
        page.serialized_bytes = page.serialized_bytes.checked_add(serialized_bytes).ok_or(
            CaptureError::SystemInvariant("Codex NativePath Pro page byte count overflowed"),
        )?;
        page.next_safe_frontier = next_frontier;
        if page.units() == MAX_CODEX_PAGE_UNITS {
            self.flush_pro_page()?;
        }
        Ok(())
    }

    fn flush_pro_page(&mut self) -> Result<()> {
        let Some(mut page) = self.pro_page.take() else {
            return Ok(());
        };
        let next = new_pro_page(page.next_safe_frontier.clone());
        if page.outputs.is_empty() {
            self.pro_page = Some(next);
            return Ok(());
        }
        if self.ready_pro_page.is_some() {
            return Err(CaptureError::SystemInvariant(
                "Codex NativePath attempted to queue multiple unacknowledged Pro pages",
            ));
        }
        debug_assert!(page.units() <= MAX_CODEX_PAGE_UNITS);
        debug_assert!(page.serialized_bytes <= MAX_CODEX_PAGE_BYTES);
        page.identity = pro_page_identity(&page)?;
        self.counters.pro_output_pages_emitted =
            self.counters.pro_output_pages_emitted.saturating_add(1);
        self.counters.peak_pro_page_rows = self.counters.peak_pro_page_rows.max(page.units());
        self.counters.peak_pro_page_bytes =
            self.counters.peak_pro_page_bytes.max(page.serialized_bytes);
        self.ready_pro_page = Some(page);
        self.pro_page = Some(next);
        Ok(())
    }

    pub(crate) fn finish(mut self) -> Result<CodexSourceScan> {
        if !self.exhausted
            || self.active_core_page.is_some()
            || self.ready_core_page.is_some()
            || self.ready_pro_page.is_some()
            || self
                .pro_page
                .as_ref()
                .is_some_and(|page| !page.outputs.is_empty())
        {
            return Err(CaptureError::InvalidPayload(
                "Codex NativePath scan must drain every owned page before certification".to_owned(),
            ));
        }
        if let Some(mut replay) = self.replay.take() {
            let after = observed_file(&replay.source)?;
            if after != replay.before_observation {
                return Err(source_changed_during_scan());
            }
            replay.after_observation = after;
            return Ok(replay);
        }

        let full_revision_sha256 = self.full_hasher.finalize().into();
        let complete_prefix_sha256 = self.complete_hasher.finalize().into();
        let after = observed_file(&self.source)?;
        if after != self.before {
            return Err(source_changed_during_scan());
        }
        if let Some(owner) = self.owner.as_ref() {
            validate_catalog_owner(
                self.source.catalog_native_session_id.as_deref(),
                &owner.native_session_id,
            )?;
        }

        Ok(CodexSourceScan {
            source: self.source,
            before_observation: self.before,
            after_observation: after,
            disposition: self.disposition,
            prefix_proof: self.prefix_proof,
            resume_proof: self.resume_proof,
            full_revision_sha256,
            complete_prefix_sha256,
            complete_prefix_end: self
                .incomplete_tail
                .as_ref()
                .map(|tail| tail.start_byte)
                .unwrap_or(self.offset),
            next_raw_ordinal: self.raw_ordinal,
            owner: self.owner,
            pending_tool_authorities: self.tool_authorities.into_values().collect(),
            #[cfg(test)]
            rejections: self.rejections,
            incomplete_tail: self.incomplete_tail,
            counters: self.counters,
        })
    }

    fn position(&self) -> ScannerPosition {
        ScannerPosition {
            offset: self.offset,
            raw_ordinal: self.raw_ordinal,
            had_owner: self.owner.is_some(),
            complete_hasher: self.complete_hasher.clone(),
            full_hasher: self.full_hasher.clone(),
            rejection_len: self.rejections.len(),
            counters: self.counters,
        }
    }

    fn restore(&mut self, position: ScannerPosition) -> Result<()> {
        let actual_parse_counts = (
            self.counters.structural_json_parses,
            self.counters.typed_json_parses,
            self.counters.structural_output_probes,
            self.counters.typed_output_parses,
        );
        self.reader.seek(SeekFrom::Start(position.offset))?;
        self.offset = position.offset;
        self.raw_ordinal = position.raw_ordinal;
        if !position.had_owner {
            self.owner = None;
        }
        self.complete_hasher = position.complete_hasher;
        self.full_hasher = position.full_hasher;
        self.rejections.truncate(position.rejection_len);
        self.counters = position.counters;
        (
            self.counters.structural_json_parses,
            self.counters.typed_json_parses,
            self.counters.structural_output_probes,
            self.counters.typed_output_parses,
        ) = actual_parse_counts;
        Ok(())
    }

    fn frontier(&self) -> CodexNativeFrontier {
        CodexNativeFrontier {
            complete_prefix_end: self
                .incomplete_tail
                .as_ref()
                .map(|tail| tail.start_byte)
                .unwrap_or(self.offset),
            next_raw_ordinal: self.raw_ordinal,
            complete_prefix_sha256: self.complete_hasher.clone().finalize().into(),
        }
    }

    fn finish_page(&mut self, mut page: CodexNativePage) -> Result<CodexNativePage> {
        page.owner = self.owner.clone();
        page.next_safe_frontier = self.frontier();
        debug_assert!(page.physical_records <= MAX_CODEX_PAGE_UNITS as u64);
        debug_assert!(page.units() <= MAX_CODEX_PAGE_UNITS);
        debug_assert!(page.serialized_bytes <= MAX_CODEX_PAGE_BYTES);
        self.counters.emitted_pages = self.counters.emitted_pages.saturating_add(1);
        self.counters.peak_page_rows = self.counters.peak_page_rows.max(page.units());
        self.counters.peak_page_bytes = self.counters.peak_page_bytes.max(page.serialized_bytes);
        page.identity = core_page_identity(&page)?;
        Ok(page)
    }

    fn process_record(
        &mut self,
        record: &[u8],
        start_byte: u64,
        end_byte: u64,
    ) -> Result<CodexRecordProjection> {
        let record = trim_jsonl_terminator(record);
        if record.iter().all(u8::is_ascii_whitespace) {
            self.counters.ignored_records = self.counters.ignored_records.saturating_add(1);
            return Ok(CodexRecordProjection::default());
        }

        self.counters.structural_json_parses =
            self.counters.structural_json_parses.saturating_add(1);
        let probe = match classify_codex_record(record) {
            Ok(probe) => probe,
            Err(_) => {
                self.reject(start_byte, end_byte, "malformed Codex JSON record", false);
                return Ok(CodexRecordProjection::default());
            }
        };
        match probe.class {
            CodexRecordClass::SessionMeta => {
                self.counters.typed_json_parses = self.counters.typed_json_parses.saturating_add(1);
                match parse_session_meta(record) {
                    Some(owner) if self.owner.is_none() => {
                        let owner_bytes = serialized_owner_bytes(&owner)?;
                        if owner_bytes > MAX_CODEX_PAGE_BYTES.saturating_sub(PAGE_FIXED_WIRE_BYTES)
                        {
                            self.reject(
                                start_byte,
                                end_byte,
                                "Codex session metadata exceeds the bounded NativePath page",
                                false,
                            );
                            return Ok(CodexRecordProjection::default());
                        }
                        self.owner = Some(owner);
                        return Ok(CodexRecordProjection {
                            core_row: None,
                            pro_output: None,
                            context_mutation: None,
                            core_serialized_bytes: owner_bytes,
                            pro_serialized_bytes: 0,
                        });
                    }
                    Some(_) => {
                        self.counters.ignored_records =
                            self.counters.ignored_records.saturating_add(1);
                    }
                    None => self.reject(
                        start_byte,
                        end_byte,
                        "malformed Codex session metadata",
                        false,
                    ),
                }
                Ok(CodexRecordProjection::default())
            }
            CodexRecordClass::Ignored => {
                self.counters.ignored_records = self.counters.ignored_records.saturating_add(1);
                Ok(CodexRecordProjection::default())
            }
            CodexRecordClass::Retained(kind) => {
                let Some(owner) = self.owner.as_ref() else {
                    self.reject(
                        start_byte,
                        end_byte,
                        "Codex retained record appeared before session metadata",
                        false,
                    );
                    return Ok(CodexRecordProjection::default());
                };
                self.counters.retained_json_parses =
                    self.counters.retained_json_parses.saturating_add(1);
                self.counters.typed_json_parses = self.counters.typed_json_parses.saturating_add(1);
                let Some(retained) = parse_decoded_record(record, owner) else {
                    self.reject(
                        start_byte,
                        end_byte,
                        "malformed retained Codex record",
                        false,
                    );
                    return Ok(CodexRecordProjection::default());
                };
                let Some(mut row) = build_event_row(self.raw_ordinal, kind, &retained) else {
                    self.reject(
                        start_byte,
                        end_byte,
                        "unsupported retained Codex record",
                        false,
                    );
                    return Ok(CodexRecordProjection::default());
                };
                let raw_source_path = self.source.source_path.display().to_string();
                let line_number = usize::try_from(self.raw_ordinal)
                    .ok()
                    .and_then(|ordinal| ordinal.checked_add(1))
                    .ok_or(CaptureError::SystemInvariant(
                        "Codex NativePath raw ordinal exceeds platform limits",
                    ))?;
                let provider_event_index = row.provider_event.provider_event_index;
                let occurred_at = row.provider_event.occurred_at;
                let touch_outcome = visit_provider_file_touch_drafts_with_limit(
                    &retained.payload,
                    event_type_supports_structured_file_touches(row.provider_event.event_type),
                    MAX_PROVIDER_FILE_TOUCHES_PER_EVENT,
                    |(touch_ordinal, touch)| {
                        let provider_touch_index =
                            if provider_event_index > MAX_PACKED_PROVIDER_EVENT_INDEX {
                                touch_ordinal
                            } else {
                                ((line_number as u64) << 16) | touch_ordinal
                            };
                        row.file_touches.push(super::CodexFileTouch {
                            provider: ctx_history_core::CaptureProvider::Codex,
                            provider_session_id: owner.native_session_id.clone(),
                            provider_touch_index,
                            provider_event_index: Some(provider_event_index),
                            raw_source_path: Some(raw_source_path.clone()),
                            source_root: Some(self.source.source_root.clone()),
                            path: touch.path,
                            change_kind: touch.change_kind,
                            old_path: touch.old_path,
                            line_count_delta: None,
                            confidence: touch.confidence,
                            occurred_at,
                            source_format: crate::CODEX_SESSION_SOURCE_FORMAT.to_owned(),
                            metadata: touch.metadata,
                        });
                        Ok::<(), CaptureError>(())
                    },
                )?;
                if touch_outcome.limit_exceeded() {
                    self.reject(
                        start_byte,
                        end_byte,
                        PROVIDER_FILE_TOUCH_LIMIT_REJECTION,
                        false,
                    );
                }
                let body_bytes = serde_json::to_vec(&row.provider_event.payload)?.len();
                let row_bytes = serde_json::to_vec(&row)?.len().saturating_add(1);
                self.counters.retained_records = self.counters.retained_records.saturating_add(1);
                self.counters.retained_body_bytes = self
                    .counters
                    .retained_body_bytes
                    .saturating_add(u64::try_from(body_bytes).unwrap_or(u64::MAX));
                self.counters.retained_hashes_created =
                    self.counters.retained_hashes_created.saturating_add(1);
                let context_mutation = tool_context_from_row(&row).map(|(call_id, context)| {
                    let authority = CodexPendingToolAuthority::new(
                        &call_id,
                        start_byte,
                        end_byte,
                        self.raw_ordinal,
                    );
                    CodexContextMutation::Insert(call_id, context, authority)
                });
                Ok(CodexRecordProjection {
                    core_row: Some(row),
                    pro_output: None,
                    context_mutation,
                    core_serialized_bytes: row_bytes,
                    pro_serialized_bytes: 0,
                })
            }
            CodexRecordClass::ExcludedResult(result_kind) => {
                self.process_output(record, &probe, result_kind, start_byte, end_byte)
            }
        }
    }

    fn process_output(
        &mut self,
        record: &[u8],
        probe: &CodexRecordProbe<'_>,
        result_kind: CodexResultKind,
        start_byte: u64,
        end_byte: u64,
    ) -> Result<CodexRecordProjection> {
        self.counters.native_result_records = self.counters.native_result_records.saturating_add(1);
        self.counters.native_result_record_bytes = self
            .counters
            .native_result_record_bytes
            .saturating_add(end_byte.saturating_sub(start_byte));

        if !result_kind.is_eligible_output() {
            return Ok(CodexRecordProjection::default());
        }

        self.counters.structural_output_probes =
            self.counters.structural_output_probes.saturating_add(1);
        let Some(structural) = probe.output.as_ref() else {
            return Err(CaptureError::SystemInvariant(
                "eligible Codex output is missing its structural outcome probe",
            ));
        };
        let sparse_core_diagnostic = matches!(
            structural.outcome.outcome,
            OutputOutcome::Failure | OutputOutcome::Timeout
        );
        let Some(owner) = self.owner.clone() else {
            self.reject(
                start_byte,
                end_byte,
                "Codex output appeared before session metadata",
                false,
            );
            return Ok(CodexRecordProjection::default());
        };
        let Some(occurred_at) = probe_timestamp(probe, owner.started_at) else {
            self.reject(
                start_byte,
                end_byte,
                "Codex output timestamp is not valid RFC3339",
                false,
            );
            return Ok(CodexRecordProjection::default());
        };

        let call_id = probe.call_id.as_deref();
        let context = call_id
            .and_then(|call_id| self.tool_contexts.get(call_id))
            .cloned();

        if self.profile == CodexNativeProfile::CoreOnly && !sparse_core_diagnostic {
            // Structural admission is complete and successful/unknown output
            // bodies have no Core projection. Retire the context without
            // hydrating canonical output or allocating a removal key.
            if let Some(call_id) = probe.call_id.as_deref() {
                self.tool_contexts.remove(call_id);
                self.tool_authorities.remove(call_id);
            }
            return Ok(CodexRecordProjection::default());
        }

        let core_row = build_sparse_output_row(
            self.raw_ordinal,
            occurred_at,
            result_kind,
            call_id,
            context.as_ref(),
            &structural.outcome,
            structural.output_bytes,
        );
        let core_bytes = core_row
            .as_ref()
            .map(|row| serde_json::to_vec(row).map(|bytes| bytes.len().saturating_add(1)))
            .transpose()?
            .unwrap_or_default();
        if let Some(row) = core_row.as_ref() {
            let body_bytes = serde_json::to_vec(&row.provider_event.payload)?.len();
            self.counters.retained_records = self.counters.retained_records.saturating_add(1);
            self.counters.retained_body_bytes = self
                .counters
                .retained_body_bytes
                .saturating_add(u64::try_from(body_bytes).unwrap_or(u64::MAX));
            self.counters.retained_hashes_created =
                self.counters.retained_hashes_created.saturating_add(1);
        }
        let context_mutation =
            call_id.map(|call_id| CodexContextMutation::Remove(call_id.to_owned()));
        let mut projection = CodexRecordProjection {
            core_row,
            pro_output: None,
            context_mutation,
            core_serialized_bytes: core_bytes,
            pro_serialized_bytes: 0,
        };
        if self.profile == CodexNativeProfile::CoreOnly {
            return Ok(projection);
        }

        self.counters.typed_json_parses = self.counters.typed_json_parses.saturating_add(1);
        self.counters.typed_output_parses = self.counters.typed_output_parses.saturating_add(1);
        let Some(typed) = parse_decoded_record(record, &owner) else {
            // Structural admission is the shared Core authority. Any failure
            // to hydrate the transient Pro representation stays lane-local.
            return Ok(projection);
        };
        if typed.occurred_at != occurred_at {
            return Ok(projection);
        }
        let content = codex_result_content(&typed.payload)
            .map(|content| content.into_owned())
            .unwrap_or_default()
            .into_bytes();
        self.counters.result_body_bytes_decoded_or_allocated = self
            .counters
            .result_body_bytes_decoded_or_allocated
            .saturating_add(u64::try_from(content.len()).unwrap_or(u64::MAX));
        let output = match self.build_pro_output(
            call_id,
            &owner,
            result_kind,
            context.as_ref(),
            start_byte,
            end_byte,
            occurred_at,
            structural.outcome.clone(),
            content,
        ) {
            Ok(output) => output,
            Err(CaptureError::InvalidPayload(_)) => return Ok(projection),
            Err(error) => return Err(error),
        };
        let Some(output_bytes) = estimated_output_wire_bytes(&output) else {
            return Ok(projection);
        };
        if output_bytes > MAX_CODEX_PAGE_BYTES {
            return Ok(projection);
        }
        self.counters.result_handoffs_created =
            self.counters.result_handoffs_created.saturating_add(1);
        projection.pro_output = Some(output);
        projection.pro_serialized_bytes = output_bytes;
        Ok(projection)
    }

    #[allow(clippy::too_many_arguments)]
    fn build_pro_output(
        &self,
        call_id: Option<&str>,
        owner: &CodexSessionRow,
        result_kind: CodexResultKind,
        context: Option<&CodexToolCallContext>,
        start_byte: u64,
        end_byte: u64,
        occurred_at: DateTime<Utc>,
        outcome: OutputOutcomeMetadata,
        content: Vec<u8>,
    ) -> Result<ProOutputObservation> {
        let locator = serde_json::to_vec(&CodexOutputSourceLocator {
            source_root: &self.source.source_root,
            source_path: &self.source.source_path,
            byte_start: start_byte,
            byte_end_exclusive: end_byte,
            raw_ordinal: self.raw_ordinal,
        })?;
        if locator.len() > MAX_CODEX_OUTPUT_LOCATOR_BYTES {
            return Err(CaptureError::InvalidPayload(
                "Codex output locator exceeds its bounded page allowance".to_owned(),
            ));
        }
        let root_session_id = owner
            .root_native_session_id
            .clone()
            .or_else(|| owner.parent_native_session_id.clone())
            .unwrap_or_else(|| owner.native_session_id.clone());
        let tool_name = context
            .map(|context| context.tool_name.clone())
            .unwrap_or_else(|| result_kind.item_type().to_owned());
        let kind = if codex_is_command_tool(&tool_name) {
            OutputObservationKind::Command
        } else {
            OutputObservationKind::Tool
        };
        let command = context.map(|context| OutputCommandContext {
            tool_name: context.tool_name.clone(),
            command: context
                .command_preview
                .clone()
                .or_else(|| context.arguments_preview.clone())
                .unwrap_or_default(),
            working_directory: owner.cwd.clone(),
        });
        let line_number = self.raw_ordinal.saturating_add(1);
        Ok(ProOutputObservation {
            kind,
            coordinate: OutputNativeCoordinate {
                unit_key: format!(
                    "codex/nativepath/{}/{}/0",
                    owner.native_session_id, self.raw_ordinal
                ),
                native_sequence: self.raw_ordinal,
                native_record_id: Some(format!("line-{line_number}")),
                source_record_ordinal: Some(self.raw_ordinal),
                source_record_subrecord_index: Some(0),
                byte_start: Some(start_byte),
                byte_end_exclusive: Some(end_byte),
            },
            occurred_at_unix_ms: Some(occurred_at.timestamp_millis()),
            associations: OutputAssociations {
                direct_session_id: owner.native_session_id.clone(),
                root_session_id,
                parent_session_id: owner.parent_native_session_id.clone(),
                provider_session_id: Some(owner.native_session_id.clone()),
                agent_id: owner.external_agent_id.clone(),
                repository: None,
            },
            call_id: call_id.map(str::to_owned),
            command,
            outcome,
            locator: OutputSourceLocator {
                version: 1,
                kind: "codex/nativepath/jsonl-result".to_owned(),
                payload: locator,
            },
            content,
        })
    }

    fn apply_context_mutation(&mut self, mutation: CodexContextMutation) {
        match mutation {
            CodexContextMutation::Insert(call_id, mut context, authority)
                if call_id.len() <= MAX_CODEX_TOOL_CALL_ID_BYTES =>
            {
                context = bound_tool_context(context);
                self.tool_authorities.insert(call_id.clone(), authority);
                self.tool_contexts.insert(call_id, context);
                while self.tool_contexts.len() > MAX_CODEX_TOOL_CONTEXTS {
                    let Some(oldest) = self.tool_contexts.keys().next().cloned() else {
                        break;
                    };
                    self.tool_contexts.remove(&oldest);
                    self.tool_authorities.remove(&oldest);
                }
            }
            CodexContextMutation::Insert(_, _, _) => {}
            CodexContextMutation::Remove(call_id) => {
                self.tool_contexts.remove(&call_id);
                self.tool_authorities.remove(&call_id);
            }
        }
    }

    fn reject(&mut self, start_byte: u64, end_byte: u64, reason: &'static str, oversized: bool) {
        if oversized {
            self.counters.oversized_records = self.counters.oversized_records.saturating_add(1);
        } else {
            self.counters.malformed_records = self.counters.malformed_records.saturating_add(1);
        }
        if self.rejections.len() < MAX_REJECTION_DETAILS {
            self.rejections.push(CodexRecordRejection {
                raw_ordinal: self.raw_ordinal,
                start_byte,
                end_byte,
                reason,
            });
        }
    }
}

#[derive(Serialize)]
struct CodexOutputSourceLocator<'a> {
    source_root: &'a str,
    source_path: &'a Path,
    byte_start: u64,
    byte_end_exclusive: u64,
    raw_ordinal: u64,
}

fn probe_timestamp(probe: &CodexRecordProbe<'_>, fallback: DateTime<Utc>) -> Option<DateTime<Utc>> {
    match probe.timestamp.as_deref() {
        Some(timestamp) => DateTime::parse_from_rfc3339(timestamp)
            .ok()
            .map(|timestamp| timestamp.with_timezone(&Utc)),
        None => Some(fallback),
    }
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes.min(value.len());
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

fn bound_tool_context(mut context: CodexToolCallContext) -> CodexToolCallContext {
    context.tool_name = truncate_utf8(&context.tool_name, MAX_CODEX_TOOL_NAME_BYTES);
    context.command_preview = context
        .command_preview
        .as_deref()
        .map(|value| truncate_utf8(value, MAX_CODEX_TOOL_PREVIEW_BYTES));
    context.arguments_preview = context
        .arguments_preview
        .as_deref()
        .map(|value| truncate_utf8(value, MAX_CODEX_TOOL_PREVIEW_BYTES));
    context
}

fn serialized_owner_bytes(owner: &CodexSessionRow) -> Result<usize> {
    Ok(serde_json::to_vec(owner)?.len().saturating_add(1))
}

fn new_pro_page(expected_frontier: CodexNativeFrontier) -> CodexNativeProOutputPage {
    CodexNativeProOutputPage {
        identity: CodexNativeProOutputPageIdentity::default(),
        next_safe_frontier: expected_frontier.clone(),
        expected_frontier,
        outputs: Vec::new(),
        serialized_bytes: 0,
    }
}

fn core_page_identity(page: &CodexNativePage) -> Result<CodexNativePageIdentity> {
    let mut hasher = Sha256::new();
    hasher.update(CODEX_CORE_PAGE_IDENTITY_DOMAIN);
    hash_frontier(&mut hasher, &page.expected_frontier);
    hash_frontier(&mut hasher, &page.next_safe_frontier);
    hash_optional_serialized(&mut hasher, page.owner.as_ref())?;
    hash_usize(&mut hasher, page.core_rows.len())?;
    for row in &page.core_rows {
        hash_serialized(&mut hasher, row)?;
    }
    hasher.update(page.physical_records.to_le_bytes());
    hash_usize(&mut hasher, page.serialized_bytes)?;
    hasher.update([u8::from(page.terminal)]);
    Ok(CodexNativePageIdentity(hasher.finalize().into()))
}

fn pro_page_identity(page: &CodexNativeProOutputPage) -> Result<CodexNativeProOutputPageIdentity> {
    let mut hasher = Sha256::new();
    hasher.update(CODEX_PRO_PAGE_IDENTITY_DOMAIN);
    hash_frontier(&mut hasher, &page.expected_frontier);
    hash_frontier(&mut hasher, &page.next_safe_frontier);
    hash_usize(&mut hasher, page.outputs.len())?;
    for output in &page.outputs {
        hash_pro_output(&mut hasher, output)?;
    }
    hash_usize(&mut hasher, page.serialized_bytes)?;
    Ok(CodexNativeProOutputPageIdentity(hasher.finalize().into()))
}

fn hash_frontier(hasher: &mut Sha256, frontier: &CodexNativeFrontier) {
    hasher.update(frontier.complete_prefix_end.to_le_bytes());
    hasher.update(frontier.next_raw_ordinal.to_le_bytes());
    hasher.update(frontier.complete_prefix_sha256);
}

fn hash_serialized<T: Serialize>(hasher: &mut Sha256, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    hash_bytes(hasher, &bytes)
}

fn hash_optional_serialized<T: Serialize>(hasher: &mut Sha256, value: Option<&T>) -> Result<()> {
    match value {
        Some(value) => {
            hasher.update([1]);
            hash_serialized(hasher, value)
        }
        None => {
            hasher.update([0]);
            Ok(())
        }
    }
}

fn hash_pro_output(hasher: &mut Sha256, output: &ProOutputObservation) -> Result<()> {
    hasher.update([match output.kind {
        OutputObservationKind::Command => 1,
        OutputObservationKind::Tool => 2,
    }]);
    hash_text(hasher, &output.coordinate.unit_key)?;
    hasher.update(output.coordinate.native_sequence.to_le_bytes());
    hash_optional_text(hasher, output.coordinate.native_record_id.as_deref())?;
    hash_optional_u64(hasher, output.coordinate.source_record_ordinal);
    hash_optional_u32(hasher, output.coordinate.source_record_subrecord_index);
    hash_optional_u64(hasher, output.coordinate.byte_start);
    hash_optional_u64(hasher, output.coordinate.byte_end_exclusive);
    hash_optional_i64(hasher, output.occurred_at_unix_ms);
    hash_text(hasher, &output.associations.direct_session_id)?;
    hash_text(hasher, &output.associations.root_session_id)?;
    hash_optional_text(hasher, output.associations.parent_session_id.as_deref())?;
    hash_optional_text(hasher, output.associations.provider_session_id.as_deref())?;
    hash_optional_text(hasher, output.associations.agent_id.as_deref())?;
    match output.associations.repository.as_ref() {
        Some(repository) => {
            hasher.update([1]);
            hash_text(hasher, &repository.repository_id)?;
            hash_optional_text(hasher, repository.checkout_id.as_deref())?;
            hash_optional_text(hasher, repository.worktree_id.as_deref())?;
            hash_optional_text(hasher, repository.object_format.as_deref())?;
        }
        None => hasher.update([0]),
    }
    hash_optional_text(hasher, output.call_id.as_deref())?;
    match output.command.as_ref() {
        Some(command) => {
            hasher.update([1]);
            hash_text(hasher, &command.tool_name)?;
            hash_text(hasher, &command.command)?;
            hash_optional_text(hasher, command.working_directory.as_deref())?;
        }
        None => hasher.update([0]),
    }
    hasher.update([match output.outcome.outcome {
        OutputOutcome::Success => 1,
        OutputOutcome::Failure => 2,
        OutputOutcome::Timeout => 3,
        OutputOutcome::Unknown => 4,
    }]);
    hash_optional_i32(hasher, output.outcome.exit_code);
    hash_optional_u64(hasher, output.outcome.duration_ms);
    hasher.update(output.locator.version.to_le_bytes());
    hash_text(hasher, &output.locator.kind)?;
    hash_bytes(hasher, &output.locator.payload)?;
    hash_bytes(hasher, &output.content)
}

fn hash_text(hasher: &mut Sha256, value: &str) -> Result<()> {
    hash_bytes(hasher, value.as_bytes())
}

fn hash_bytes(hasher: &mut Sha256, value: &[u8]) -> Result<()> {
    let len = u64::try_from(value.len())
        .map_err(|_| CaptureError::SystemInvariant("Codex page identity length exceeds u64"))?;
    hasher.update(len.to_le_bytes());
    hasher.update(value);
    Ok(())
}

fn hash_usize(hasher: &mut Sha256, value: usize) -> Result<()> {
    let value = u64::try_from(value)
        .map_err(|_| CaptureError::SystemInvariant("Codex page count exceeds u64"))?;
    hasher.update(value.to_le_bytes());
    Ok(())
}

fn hash_optional_text(hasher: &mut Sha256, value: Option<&str>) -> Result<()> {
    match value {
        Some(value) => {
            hasher.update([1]);
            hash_text(hasher, value)
        }
        None => {
            hasher.update([0]);
            Ok(())
        }
    }
}

fn hash_optional_u64(hasher: &mut Sha256, value: Option<u64>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hasher.update(value.to_le_bytes());
        }
        None => hasher.update([0]),
    }
}

fn hash_optional_u32(hasher: &mut Sha256, value: Option<u32>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hasher.update(value.to_le_bytes());
        }
        None => hasher.update([0]),
    }
}

fn hash_optional_i64(hasher: &mut Sha256, value: Option<i64>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hasher.update(value.to_le_bytes());
        }
        None => hasher.update([0]),
    }
}

fn hash_optional_i32(hasher: &mut Sha256, value: Option<i32>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hasher.update(value.to_le_bytes());
        }
        None => hasher.update([0]),
    }
}

fn estimated_output_wire_bytes(output: &ProOutputObservation) -> Option<usize> {
    let mut total = PRO_OUTPUT_FIXED_WIRE_BYTES;
    for value in [
        Some(output.coordinate.unit_key.as_str()),
        output.coordinate.native_record_id.as_deref(),
        Some(output.associations.direct_session_id.as_str()),
        Some(output.associations.root_session_id.as_str()),
        output.associations.parent_session_id.as_deref(),
        output.associations.provider_session_id.as_deref(),
        output.associations.agent_id.as_deref(),
        output.call_id.as_deref(),
        output
            .command
            .as_ref()
            .map(|command| command.tool_name.as_str()),
        output
            .command
            .as_ref()
            .map(|command| command.command.as_str()),
        output
            .command
            .as_ref()
            .and_then(|command| command.working_directory.as_deref()),
        Some(output.locator.kind.as_str()),
    ]
    .into_iter()
    .flatten()
    {
        total = total.checked_add(worst_case_json_string_bytes(value.len())?)?;
    }
    total = total.checked_add(base64_json_bytes(output.locator.payload.len())?)?;
    total.checked_add(base64_json_bytes(output.content.len())?)
}

fn worst_case_json_string_bytes(bytes: usize) -> Option<usize> {
    bytes.checked_mul(6)?.checked_add(2)
}

fn base64_json_bytes(bytes: usize) -> Option<usize> {
    bytes
        .checked_add(2)?
        .checked_div(3)?
        .checked_mul(4)?
        .checked_add(2)
}

struct BoundedRecordRead {
    complete: bool,
    oversized: bool,
    stored_len: usize,
    byte_len: u64,
    sha256: [u8; 32],
}

fn read_bounded_record(
    reader: &mut BufReader<File>,
    storage: &mut Vec<u8>,
    full_hasher: &mut Sha256,
    complete_hasher: &mut Sha256,
) -> Result<Option<BoundedRecordRead>> {
    storage.clear();
    let complete_before_record = complete_hasher.clone();
    let mut record_hasher = Sha256::new();
    let mut byte_len = 0_u64;
    let mut oversized = false;

    loop {
        let (consumed, complete) = {
            let available = reader.fill_buf()?;
            if available.is_empty() {
                if byte_len == 0 {
                    return Ok(None);
                }
                *complete_hasher = complete_before_record;
                return Ok(Some(BoundedRecordRead {
                    complete: false,
                    oversized,
                    stored_len: storage.len(),
                    byte_len,
                    sha256: record_hasher.finalize().into(),
                }));
            }

            let newline = available.iter().position(|byte| *byte == b'\n');
            let consumed = newline.map_or(available.len(), |index| index + 1);
            let chunk = &available[..consumed];
            full_hasher.update(chunk);
            complete_hasher.update(chunk);
            record_hasher.update(chunk);
            byte_len =
                byte_len
                    .checked_add(u64::try_from(consumed).map_err(|_| {
                        CaptureError::SystemInvariant("Codex record chunk exceeds u64")
                    })?)
                    .ok_or(CaptureError::SystemInvariant(
                        "Codex JSONL record length exceeds u64",
                    ))?;

            let content_len = if newline.is_some() {
                consumed.saturating_sub(1)
            } else {
                consumed
            };
            let remaining = MAX_CODEX_RECORD_BYTES.saturating_sub(storage.len());
            let copied = content_len.min(remaining);
            storage.extend_from_slice(&chunk[..copied]);
            if copied != content_len {
                oversized = true;
            }
            (consumed, newline.is_some())
        };
        reader.consume(consumed);
        if complete {
            return Ok(Some(BoundedRecordRead {
                complete: true,
                oversized,
                stored_len: storage.len(),
                byte_len,
                sha256: [0; 32],
            }));
        }
    }
}

fn trim_jsonl_terminator(mut record: &[u8]) -> &[u8] {
    if record.last() == Some(&b'\r') {
        record = &record[..record.len() - 1];
    }
    record
}

struct ValidatedCheckpoint {
    bytes_read: u64,
    complete_prefix_hasher: Sha256,
    pending_tool_contexts: BTreeMap<String, CodexToolCallContext>,
    pending_tool_authorities: BTreeMap<String, CodexPendingToolAuthority>,
}

fn decode_pending_tool_authority(
    record: &[u8],
    authority: &CodexPendingToolAuthority,
    owner: &CodexSessionRow,
) -> Result<(String, CodexToolCallContext)> {
    let Some(record) = record.strip_suffix(b"\n") else {
        return Err(invalid_checkpoint_proof(
            "pending tool-call authority does not end at a JSONL boundary",
        ));
    };
    let record = trim_jsonl_terminator(record);
    let probe = classify_codex_record(record).map_err(|_| {
        invalid_checkpoint_proof("pending tool-call authority is not valid Codex JSON")
    })?;
    let CodexRecordClass::Retained(kind @ super::record::CodexRetainedKind::ToolCall) = probe.class
    else {
        return Err(invalid_checkpoint_proof(
            "pending tool-call authority does not identify a tool call",
        ));
    };
    let retained = parse_decoded_record(record, owner)
        .ok_or_else(|| invalid_checkpoint_proof("pending tool-call authority cannot be decoded"))?;
    let row = build_event_row(authority.raw_ordinal, kind, &retained).ok_or_else(|| {
        invalid_checkpoint_proof("pending tool-call authority cannot be projected")
    })?;
    let (call_id, context) = tool_context_from_row(&row).ok_or_else(|| {
        invalid_checkpoint_proof("pending tool-call authority has no correlation identity")
    })?;
    if !authority.matches_call_id(&call_id) {
        return Err(invalid_checkpoint_proof(
            "pending tool-call authority correlation does not match checkpoint state",
        ));
    }
    Ok((call_id, bound_tool_context(context)))
}

fn validate_checkpoint_source(
    reader: &mut BufReader<File>,
    checkpoint: &CodexNativeCheckpoint,
    hydrate_pending_tools: bool,
) -> Result<ValidatedCheckpoint> {
    // The prefix proof is the sole read pass over checkpointed bytes. On
    // append, only the at-most-24 authority spans are retained long enough to
    // reconstruct transient correlation state during that same pass.
    reader.seek(SeekFrom::Start(0))?;
    let complete_prefix_end = checkpoint.complete_prefix_end();
    let mut remaining = checkpoint.observation.len;
    let mut offset = 0_u64;
    let mut buffer = vec![0_u8; CHECKPOINT_READ_BUFFER_BYTES];
    let mut full_hasher = Sha256::new();
    let mut complete_prefix_hasher = Sha256::new();
    let mut incomplete_tail_hasher = Sha256::new();
    let mut complete_records = 0_u64;
    let mut final_prefix_byte = None;
    let mut tail_contains_newline = false;
    let mut authorities = checkpoint
        .pending_tool_authorities()
        .iter()
        .collect::<Vec<_>>();
    authorities.sort_by_key(|authority| authority.record_start);
    let mut authority_index = 0_usize;
    let mut current_record_start = 0_u64;
    let mut pending_tool_record = Vec::new();
    let mut pending_tool_contexts = BTreeMap::new();
    let mut pending_tool_authorities = BTreeMap::new();

    while remaining != 0 {
        let wanted = usize::try_from(remaining.min(CHECKPOINT_READ_BUFFER_BYTES as u64))
            .map_err(|_| CaptureError::SystemInvariant("Codex checkpoint read exceeds usize"))?;
        let read = reader.read(&mut buffer[..wanted])?;
        if read == 0 {
            return Err(invalid_checkpoint_proof(
                "checkpoint observation ends after source EOF",
            ));
        }
        let chunk = &buffer[..read];
        full_hasher.update(chunk);
        let read_u64 = u64::try_from(read)
            .map_err(|_| CaptureError::SystemInvariant("Codex checkpoint read exceeds u64"))?;
        let chunk_end = offset
            .checked_add(read_u64)
            .ok_or(CaptureError::SystemInvariant(
                "Codex checkpoint offset exceeds u64",
            ))?;

        if offset < complete_prefix_end {
            let prefix_len = usize::try_from((complete_prefix_end.min(chunk_end)) - offset)
                .map_err(|_| CaptureError::SystemInvariant("Codex prefix length exceeds usize"))?;
            let prefix = &chunk[..prefix_len];
            complete_prefix_hasher.update(prefix);
            for (index, byte) in prefix.iter().enumerate() {
                let absolute_offset = offset
                    .checked_add(u64::try_from(index).unwrap_or(u64::MAX))
                    .ok_or(CaptureError::SystemInvariant(
                        "Codex checkpoint record offset exceeds u64",
                    ))?;
                if hydrate_pending_tools
                    && authorities.get(authority_index).is_some_and(|authority| {
                        absolute_offset >= authority.record_start
                            && absolute_offset < authority.record_end
                    })
                {
                    pending_tool_record.push(*byte);
                }
                if *byte != b'\n' {
                    continue;
                }
                let record_end =
                    absolute_offset
                        .checked_add(1)
                        .ok_or(CaptureError::SystemInvariant(
                            "Codex checkpoint record boundary exceeds u64",
                        ))?;
                if let Some(authority) = authorities.get(authority_index) {
                    if authority.record_start < record_end {
                        if authority.record_start != current_record_start
                            || authority.record_end != record_end
                            || authority.raw_ordinal != complete_records
                        {
                            return Err(invalid_checkpoint_proof(
                                "pending tool-call authority does not match its JSONL record boundary",
                            ));
                        }
                        if hydrate_pending_tools {
                            let (call_id, context) = decode_pending_tool_authority(
                                &pending_tool_record,
                                authority,
                                &checkpoint.owner,
                            )?;
                            if pending_tool_contexts
                                .insert(call_id.clone(), context)
                                .is_some()
                                || pending_tool_authorities
                                    .insert(call_id, (*authority).clone())
                                    .is_some()
                            {
                                return Err(invalid_checkpoint_proof(
                                    "pending tool-call authority correlation is duplicated",
                                ));
                            }
                            pending_tool_record.clear();
                        }
                        authority_index = authority_index.saturating_add(1);
                    }
                }
                current_record_start = record_end;
                complete_records = complete_records.saturating_add(1);
            }
            final_prefix_byte = prefix.last().copied().or(final_prefix_byte);
            if prefix_len < chunk.len() {
                let tail = &chunk[prefix_len..];
                incomplete_tail_hasher.update(tail);
                tail_contains_newline |= tail.contains(&b'\n');
            }
        } else {
            incomplete_tail_hasher.update(chunk);
            tail_contains_newline |= chunk.contains(&b'\n');
        }
        offset = chunk_end;
        remaining -= read_u64;
    }

    let full_revision_sha256: [u8; 32] = full_hasher.finalize().into();
    let complete_prefix_sha256: [u8; 32] = complete_prefix_hasher.clone().finalize().into();
    if full_revision_sha256 != checkpoint.full_revision_sha256
        || complete_prefix_sha256 != checkpoint.complete_prefix_sha256
        || complete_records != checkpoint.next_raw_ordinal()
        || authority_index != authorities.len()
        || (complete_prefix_end != 0 && final_prefix_byte != Some(b'\n'))
    {
        return Err(invalid_checkpoint_proof(
            "checkpoint digest, boundary, or raw ordinal does not match source bytes",
        ));
    }

    match checkpoint.incomplete_tail() {
        None if complete_prefix_end == checkpoint.observation.len => {}
        Some((tail_len, tail_sha256))
            if !tail_contains_newline
                && tail_len == checkpoint.observation.len - complete_prefix_end
                && <[u8; 32]>::from(incomplete_tail_hasher.finalize()) == tail_sha256 => {}
        _ => {
            return Err(invalid_checkpoint_proof(
                "checkpoint incomplete-tail proof does not match source bytes",
            ));
        }
    }

    Ok(ValidatedCheckpoint {
        bytes_read: checkpoint.observation.len,
        complete_prefix_hasher,
        pending_tool_contexts,
        pending_tool_authorities,
    })
}

fn invalid_checkpoint_proof(reason: &str) -> CaptureError {
    CaptureError::InvalidPayload(format!("invalid Codex append proof: {reason}"))
}

fn observed_file(source: &CodexCatalogSource) -> Result<CodexFileObservation> {
    let observation = observe_ordinary_file(&source.source_path)?;
    let observed = CodexFileObservation::from_parts(
        observation.len(),
        observation.modified_at(),
        *observation.token(),
    );
    if observed != source.catalog_observation {
        return Err(CaptureError::InvalidPayload(
            "Codex catalog observation changed before NativePath admission".to_owned(),
        ));
    }
    Ok(observed)
}

pub(crate) fn revalidate_codex_source_observation(
    source: &CodexCatalogSource,
    certified: &CodexFileObservation,
) -> Result<()> {
    let observed = observed_file(source)?;
    if &observed != certified {
        return Err(source_changed_during_scan());
    }
    Ok(())
}

fn validate_open_file_metadata(file: &File, observation: &CodexFileObservation) -> Result<()> {
    let metadata = file.metadata()?;
    let modified_at_ms = CodexFileObservation::from_parts(
        metadata.len(),
        metadata.modified().unwrap_or(std::time::UNIX_EPOCH),
        observation.change_token,
    )
    .modified_at_ms;
    if !metadata.is_file()
        || metadata.len() != observation.len
        || modified_at_ms != observation.modified_at_ms
    {
        return Err(source_changed_during_scan());
    }
    Ok(())
}

fn validate_catalog_owner(catalog_owner: Option<&str>, scanned_owner: &str) -> Result<()> {
    if catalog_owner.is_some_and(|catalog_owner| catalog_owner != scanned_owner) {
        return Err(CaptureError::InvalidPayload(
            "Codex catalog owner changed before NativePath admission".to_owned(),
        ));
    }
    Ok(())
}

fn source_changed_during_scan() -> CaptureError {
    CaptureError::InvalidPayload("Codex source changed while NativePath was reading it".to_owned())
}
