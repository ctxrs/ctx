use std::{collections::BTreeMap, fs::File, path::Path, sync::Arc};

use chrono::{DateTime, Utc};
use ctx_history_core::{CoreRecord, SourceKey, StableEntityId};
use serde_json::Value;

use super::raw_json::SelectorGroup;
use super::{
    checkpoint::CodexPendingCallV0,
    record::{
        classify_after_selector_ambiguity, classify_codex_record, parse_decoded_record,
        parse_session_meta, parse_turn_context, prefilter_codex_record, CodexRecordAdmission,
        CodexRecordClass, CodexRecordProbe, CodexResultKind, CodexSkipProjection,
    },
    rows::{
        audit_codex_record, build_source_backed_event_row, build_source_backed_sparse_output_row,
        provider_event_identity, CodexCoreRecordDraft, CodexRetainedNonMaterialized,
        CodexSessionRow,
    },
    source::{CodexCatalogSource, CodexFileObservation},
    source_backed::{
        codex_core_record, codex_session_identity, codex_source_key_in_root,
        CodexEventIdentityStateV0, CodexSourceBackedErrorV0,
    },
};
use crate::{
    common::io::{open_provider_source_file, OpenedProviderSourceFile},
    provider::source_backed::family::jsonl::{
        JsonlFamilyExecutionIo, JsonlFamilyExecutionPosition,
    },
    CaptureError, Result,
};
// Pages are also the exact progress-publication boundary. Keep them small
// enough that a worker cannot make the visible counters appear stalled while
// projecting a dense rollout.
const MAX_CODEX_PAGE_UNITS: usize = 16;
const MAX_CODEX_SOURCE_BACKED_PAGE_RECORDS: u64 = 4 * 1024;
const MAX_CODEX_SOURCE_BACKED_PAGE_PROGRESS_BYTES: u64 = 32 * 1024 * 1024;
const PAGE_FIXED_WIRE_BYTES: usize = 4 * 1024;
pub(crate) const MAX_CODEX_RECORD_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MAX_CODEX_PAGE_BYTES: usize = 8 * 1024 * 1024;
// One source-backed row may retain both decoded text and structured/path data
// derived from a single 16 MiB provider record. The ordinary page bound is a
// rollover target; this larger envelope is valid only for a singleton row.
pub(crate) const MAX_CODEX_SOURCE_BACKED_SINGLE_ROW_PAGE_BYTES: usize =
    PAGE_FIXED_WIRE_BYTES + (MAX_CODEX_RECORD_BYTES * 2) + (1024 * 1024);
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct CodexScanCounters {
    pub(crate) bytes_read: u64,
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
    pub(crate) emitted_pages: u64,
    pub(crate) peak_page_rows: usize,
    pub(crate) peak_page_bytes: usize,
    pub(crate) peak_line_buffer_bytes: usize,
}

/// One owned, bounded Core page.
pub(crate) struct CodexNativePage {
    expected_offset: u64,
    pub(crate) records: Vec<CoreRecord>,
    pub(crate) serialized_bytes: usize,
    pub(crate) physical_records: u64,
}

pub(super) struct CodexSemanticScan {
    pub(super) checkpoint: Option<super::checkpoint::CodexSemanticCheckpoint>,
    pub(super) counters: CodexScanCounters,
}

pub(crate) struct CodexNativeScanner {
    source: CodexCatalogSource,
    owner: Option<CodexSessionRow>,
    session_metadata: Vec<CodexSessionRow>,
    pending_calls: BTreeMap<String, CodexPendingCallV0>,
    terminal_authority: CodexTerminalAuthority,
    counters: CodexScanCounters,
    local_turn_started: bool,
    core_source: SourceKey,
    core_session_id: StableEntityId,
    event_identity_state: CodexEventIdentityStateV0,
    active_core_page: Option<CodexNativePage>,
    exhausted: bool,
    ownership_quarantined: bool,
}

struct SemanticScannerPosition {
    input: JsonlFamilyExecutionPosition,
    had_owner: bool,
    counters: CodexScanCounters,
    local_turn_started: bool,
}

#[derive(Clone, Copy)]
struct CodexPhysicalRecordContext {
    raw_ordinal: u64,
    start_byte: u64,
    end_byte: u64,
}

#[derive(Default)]
struct CodexRecordProjection {
    context_mutation: Option<CodexContextMutation>,
}

// Produced once per decoded record: boxing the 296-byte source-backed mutation
// to match the 24-byte removal variant would add a per-record heap allocation.
#[allow(clippy::large_enum_variant)]
enum CodexContextMutation {
    SourceBackedRow {
        row: CodexCoreRecordDraft,
        estimated_bytes: usize,
        insert_pending_call: Option<(String, CodexPendingCallV0)>,
        remove_pending_call_id: Option<String>,
    },
}

mod checkpoint;
mod identity;
mod page_builder;
mod project;
mod scanner;
mod terminal;

use checkpoint::*;
pub(crate) use checkpoint::{
    opened_file_observation as opened_codex_file_observation, opened_file_prefix_sha256,
    reopen_codex_source_capability, revalidate_codex_catalog_source_capability,
};
use identity::*;
use terminal::*;
