use serde::{Deserialize, Serialize};

use crate::ids::*;

use super::{default_true, is_false, is_true};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeVcsComputeState {
    Computing,
    Ready,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeVcsFreshness {
    #[default]
    Unknown,
    Refreshing,
    Fresh,
    Stale,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiffUnavailableReason {
    NoRepo,
    NoTargetBranch,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeVcsBaseResolutionKind {
    ExplicitBase,
    MergeBase,
    #[default]
    WorktreeBase,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeVcsTargetSource {
    Explicit,
    PrimaryBranchConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorktreeVcsBaseResolution {
    pub kind: WorktreeVcsBaseResolutionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_source: Option<WorktreeVcsTargetSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorktreeVcsSummary {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_count: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_additions: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_deletions: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_count: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct WorktreeVcsTouchedFile {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orig_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct WorktreeVcsTouchedFiles {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<WorktreeVcsTouchedFile>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_count: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeVcsTouchedFilesState {
    #[default]
    NotLoaded,
    Loading,
    Ready,
    Stale,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorktreeVcsGitStatusSummary {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub raw: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary_line: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
    pub ahead: i64,
    pub behind: i64,
    pub detached: bool,
    pub staged: i64,
    pub unstaged: i64,
    pub untracked: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<WorktreeVcsTouchedFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Invariant: summary counts align with default session diff summary semantics.
pub struct WorktreeVcsSnapshot {
    pub worktree_id: WorktreeId,
    pub rev: i64,
    pub emitted_at_ms: i64,
    pub base_commit_sha: String,
    pub head_commit_sha: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_branch_commit_sha: Option<String>,
    pub base_resolution: WorktreeVcsBaseResolution,
    pub compute_state: WorktreeVcsComputeState,
    #[serde(default)]
    pub summary: WorktreeVcsSummary,
    #[serde(default)]
    pub git_status: WorktreeVcsGitStatusSummary,
    #[serde(default)]
    pub touched_files: WorktreeVcsTouchedFiles,
    #[serde(default)]
    pub touched_files_state: WorktreeVcsTouchedFilesState,
    #[serde(default)]
    pub freshness: WorktreeVcsFreshness,
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<DiffUnavailableReason>,
    #[serde(default)]
    pub schema_version: i64,
}
