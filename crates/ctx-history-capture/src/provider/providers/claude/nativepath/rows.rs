use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::source::ClaudeSessionKey;

pub(crate) const CLAUDE_MAX_PAGE_ROWS: usize = 64;
pub(crate) const CLAUDE_MAX_PAGE_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const CLAUDE_MAX_REJECTION_SAMPLES: usize = 32;
pub(crate) const CLAUDE_MAX_RECORD_ROWS: usize = CLAUDE_MAX_PAGE_ROWS;
pub(crate) const CLAUDE_MAX_FILE_TOUCHES_PER_RECORD: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ClaudeEventKind {
    Message,
    Summary,
    Notice,
    ToolCall,
    ToolOutput,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClaudeEventIdentity {
    pub(crate) source_record_ordinal: u64,
    pub(crate) source_subrecord_index: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClaudeNativeOrder {
    pub(crate) source_record_ordinal: u64,
    pub(crate) source_subrecord_index: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClaudePhysicalLocator {
    pub(crate) path: PathBuf,
    pub(crate) byte_start: u64,
    pub(crate) byte_end_exclusive: u64,
    pub(crate) line_number: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClaudeFileTouch {
    pub(crate) path: String,
    pub(crate) previous_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ToolCallRequest {
    pub(crate) call_id: Option<String>,
    pub(crate) tool_name: Option<String>,
    pub(crate) file_touches: Vec<ClaudeFileTouch>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ClaudeOutputOutcome {
    Failure,
    Timeout,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClaudeSparseOutputDiagnostic {
    pub(crate) call_id: Option<String>,
    pub(crate) outcome: ClaudeOutputOutcome,
    pub(crate) exit_code: Option<i32>,
    pub(crate) duration_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClaudeRetainedRow {
    pub(crate) identity: ClaudeEventIdentity,
    pub(crate) native_order: ClaudeNativeOrder,
    pub(crate) native_record_id: Option<String>,
    pub(crate) parent_native_record_id: Option<String>,
    pub(crate) kind: ClaudeEventKind,
    pub(crate) role: Option<String>,
    pub(crate) occurred_at: Option<String>,
    pub(crate) body: Option<String>,
    pub(crate) body_sha256: Option<[u8; 32]>,
    pub(crate) tool_call: Option<ToolCallRequest>,
    pub(crate) sparse_output: Option<ClaudeSparseOutputDiagnostic>,
    pub(crate) locator: ClaudePhysicalLocator,
}

impl ClaudeRetainedRow {
    pub(super) fn exact_encoded_bytes(
        &self,
    ) -> Result<usize, super::source::ClaudeNativePathError> {
        super::reader::exact_json_encoded_bytes(self)
    }
}

#[derive(Debug)]
#[allow(dead_code)]
pub(crate) struct ClaudeRowPage {
    pub(crate) rows: Vec<ClaudeRetainedRow>,
    pub(crate) estimated_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClaudeSessionMetadata {
    pub(crate) key: ClaudeSessionKey,
    pub(crate) started_at: Option<String>,
    pub(crate) cwd: Option<String>,
    pub(crate) version: Option<String>,
    pub(crate) git_branch: Option<String>,
}

impl ClaudeSessionMetadata {
    pub(super) fn new(key: ClaudeSessionKey) -> Self {
        Self {
            key,
            started_at: None,
            cwd: None,
            version: None,
            git_branch: None,
        }
    }

    pub(super) fn observe(
        &mut self,
        timestamp: Option<&str>,
        cwd: Option<&str>,
        version: Option<&str>,
        git_branch: Option<&str>,
    ) {
        if let Some(timestamp) = timestamp.filter(|value| !value.trim().is_empty()) {
            let replace = self
                .started_at
                .as_deref()
                .is_none_or(|current| timestamp < current);
            if replace {
                self.started_at = Some(timestamp.to_owned());
            }
        }
        if self.cwd.is_none() {
            self.cwd = owned_nonempty(cwd);
        }
        if self.version.is_none() {
            self.version = owned_nonempty(version);
        }
        if self.git_branch.is_none() {
            self.git_branch = owned_nonempty(git_branch);
        }
    }
}

fn owned_nonempty(value: Option<&str>) -> Option<String> {
    value
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RejectionKind {
    MalformedJson,
    OversizeRecord,
    OversizeRetainedRecord,
    SessionIdentityMismatch,
    OversizeProOutput,
    TooManyResultSubrecords,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecordRejection {
    pub(crate) kind: RejectionKind,
    pub(crate) source_record_ordinal: u64,
    pub(crate) locator: ClaudePhysicalLocator,
    pub(crate) diagnostic: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RejectionSummary {
    pub(crate) total: u64,
    pub(crate) samples: Vec<RecordRejection>,
}

impl RejectionSummary {
    pub(super) fn record(
        &mut self,
        rejection: RecordRejection,
    ) -> Result<(), super::source::ClaudeNativePathError> {
        self.total = self
            .total
            .checked_add(1)
            .ok_or(super::source::ClaudeNativePathError::PositionOverflow)?;
        if self.samples.len() < CLAUDE_MAX_REJECTION_SAMPLES {
            self.samples.push(rejection);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ParseStats {
    pub(crate) prefix_verification_bytes: u64,
    pub(crate) prefix_verification_records: u64,
    pub(crate) source_bytes_read: u64,
    pub(crate) parsed_source_bytes: u64,
    pub(crate) metadata_only_noop: bool,
    pub(crate) complete_records: u64,
    pub(crate) ignored_records: u64,
    pub(crate) malformed_records: u64,
    pub(crate) native_result_records: u64,
    pub(crate) native_result_record_bytes: u64,
    pub(crate) preallocation_excluded_result_records: u64,
    pub(crate) tagged_command_output_records: u64,
    pub(crate) result_block_records: u64,
    pub(crate) result_like_shape_records: u64,
    pub(crate) result_body_bytes_decoded_or_allocated: u64,
    pub(crate) result_hashes_created: u64,
    pub(crate) result_previews_created: u64,
    pub(crate) result_touches_created: u64,
    pub(crate) result_fts_rows_created: u64,
    pub(crate) semantic_record_parses: u64,
    pub(crate) retention_pass_records: u64,
    pub(crate) retained_messages: u64,
    pub(crate) retained_summaries: u64,
    pub(crate) retained_notices: u64,
    pub(crate) retained_tool_calls: u64,
    pub(crate) retained_body_bytes: u64,
    pub(crate) retained_body_hashes: u64,
    pub(crate) emitted_pages: u64,
    pub(crate) emitted_rows: u64,
    pub(crate) peak_page_rows: usize,
    pub(crate) peak_page_bytes: usize,
    pub(crate) emitted_pro_pages: u64,
    pub(crate) emitted_pro_outputs: u64,
    pub(crate) peak_pro_page_outputs: usize,
    pub(crate) peak_pro_page_bytes: usize,
}

impl ParseStats {
    pub(super) fn observe_row(&mut self, row: &ClaudeRetainedRow) {
        match row.kind {
            ClaudeEventKind::Message => self.retained_messages += 1,
            ClaudeEventKind::Summary => self.retained_summaries += 1,
            ClaudeEventKind::Notice => self.retained_notices += 1,
            ClaudeEventKind::ToolCall => self.retained_tool_calls += 1,
            ClaudeEventKind::ToolOutput => {}
        }
        if let Some(body) = row.body.as_ref() {
            self.retained_body_bytes = self
                .retained_body_bytes
                .saturating_add(u64::try_from(body.len()).unwrap_or(u64::MAX));
        }
        if row.body_sha256.is_some() {
            self.retained_body_hashes += 1;
        }
    }
}
