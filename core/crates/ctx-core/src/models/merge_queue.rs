use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::*;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MergeQueueEntryStatus {
    Queued,
    Running,
    Passed,
    Failed,
    Conflict,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MergeQueuePatchSource {
    Generated,
    Provided,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeQueueEntry {
    pub id: MergeQueueEntryId,
    pub workspace_id: WorkspaceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<WorktreeId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    pub target_branch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub patch_source: MergeQueuePatchSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_commit_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_commit_sha: Option<String>,
    pub patch_path: String,
    pub patch_size: i64,
    pub status: MergeQueueEntryStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_commit_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MergeQueueRunStatus {
    Running,
    Passed,
    Failed,
    Conflict,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeQueueRun {
    pub id: MergeQueueRunId,
    pub entry_id: MergeQueueEntryId,
    pub status: MergeQueueRunStatus,
    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_commit_sha: Option<String>,
}
