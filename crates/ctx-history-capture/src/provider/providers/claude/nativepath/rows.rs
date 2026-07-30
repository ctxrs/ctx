use std::path::PathBuf;

use ctx_history_core::ContentRef;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::source::ClaudeSessionKey;

pub(crate) const CLAUDE_MAX_RECORD_ROWS: usize = 64;
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
    pub(crate) record_sha256: [u8; 32],
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
    pub(crate) body_text_retention: Option<Value>,
    pub(crate) complete_body_ref: Option<ContentRef>,
    pub(crate) tool_call: Option<ToolCallRequest>,
    pub(crate) sparse_output: Option<ClaudeSparseOutputDiagnostic>,
    pub(crate) locator: ClaudePhysicalLocator,
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
    pub(crate) fn new(key: ClaudeSessionKey) -> Self {
        Self {
            key,
            started_at: None,
            cwd: None,
            version: None,
            git_branch: None,
        }
    }

    pub(crate) fn observe(
        &mut self,
        timestamp: Option<&str>,
        cwd: Option<&str>,
        version: Option<&str>,
        git_branch: Option<&str>,
    ) {
        if let Some(timestamp) = timestamp.filter(|value| !value.trim().is_empty()) {
            if self
                .started_at
                .as_deref()
                .is_none_or(|current| timestamp < current)
            {
                self.started_at = Some(timestamp.to_owned());
            }
        }
        self.cwd = self.cwd.clone().or_else(|| owned_nonempty(cwd));
        self.version = self.version.clone().or_else(|| owned_nonempty(version));
        self.git_branch = self
            .git_branch
            .clone()
            .or_else(|| owned_nonempty(git_branch));
    }
}

fn owned_nonempty(value: Option<&str>) -> Option<String> {
    value
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}
