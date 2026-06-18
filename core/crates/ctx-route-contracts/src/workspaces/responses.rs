use chrono::{DateTime, Utc};
use ctx_core::ids::{WorkspaceAttachmentId, WorkspaceId, WorktreeId};
use ctx_core::models::{
    AttachmentMode, AttachmentUpdatePolicy, ChangeSet, Contribution, VcsKind, Workspace,
    WorkspaceActiveHeadBatch, WorkspaceActiveSnapshot, WorkspaceAttachment,
    WorkspaceAttachmentKind, WorkspaceAttachmentStatus, Worktree, WorktreeBootstrapStatus,
};
use serde::{Deserialize, Serialize, Serializer};

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceRouteResponse {
    pub id: WorkspaceId,
    pub name: String,
    pub root_path: String,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vcs_kind: Option<VcsKind>,
}

impl From<Workspace> for WorkspaceRouteResponse {
    fn from(workspace: Workspace) -> Self {
        Self {
            id: workspace.id,
            name: workspace.name,
            root_path: workspace.root_path,
            created_at: workspace.created_at,
            vcs_kind: workspace.vcs_kind,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct WorktreeRouteResponse {
    pub id: WorktreeId,
    pub workspace_id: WorkspaceId,
    pub root_path: String,
    pub base_commit_sha: String,
    pub git_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vcs_kind: Option<VcsKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vcs_ref: Option<String>,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootstrap_status: Option<WorktreeBootstrapStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootstrap_started_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootstrap_finished_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootstrap_exit_code: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootstrap_timeout_sec: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootstrap_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootstrap_log_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootstrap_log_truncated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootstrap_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootstrap_script_path: Option<String>,
}

impl From<Worktree> for WorktreeRouteResponse {
    fn from(worktree: Worktree) -> Self {
        Self {
            id: worktree.id,
            workspace_id: worktree.workspace_id,
            root_path: worktree.root_path,
            base_commit_sha: worktree.base_commit_sha,
            git_branch: worktree.git_branch,
            vcs_kind: worktree.vcs_kind,
            base_revision: worktree.base_revision,
            vcs_ref: worktree.vcs_ref,
            created_at: worktree.created_at,
            bootstrap_status: worktree.bootstrap_status,
            bootstrap_started_at: worktree.bootstrap_started_at,
            bootstrap_finished_at: worktree.bootstrap_finished_at,
            bootstrap_exit_code: worktree.bootstrap_exit_code,
            bootstrap_timeout_sec: worktree.bootstrap_timeout_sec,
            bootstrap_error: worktree.bootstrap_error,
            bootstrap_log_path: worktree.bootstrap_log_path,
            bootstrap_log_truncated: worktree.bootstrap_log_truncated,
            bootstrap_command: worktree.bootstrap_command,
            bootstrap_script_path: worktree.bootstrap_script_path,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceAttachmentRouteResponse {
    pub id: WorkspaceAttachmentId,
    pub workspace_id: WorkspaceId,
    pub kind: WorkspaceAttachmentKind,
    pub name: String,
    pub source: String,
    pub revision: Option<String>,
    pub subpath: Option<String>,
    pub mount_relpath: String,
    pub mode: AttachmentMode,
    pub update_policy: AttachmentUpdatePolicy,
    pub status: WorkspaceAttachmentStatus,
    pub last_sync_at: Option<DateTime<Utc>>,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<WorkspaceAttachment> for WorkspaceAttachmentRouteResponse {
    fn from(attachment: WorkspaceAttachment) -> Self {
        Self {
            id: attachment.id,
            workspace_id: attachment.workspace_id,
            kind: attachment.kind,
            name: attachment.name,
            source: attachment.source,
            revision: attachment.revision,
            subpath: attachment.subpath,
            mount_relpath: attachment.mount_relpath,
            mode: attachment.mode,
            update_policy: attachment.update_policy,
            status: attachment.status,
            last_sync_at: attachment.last_sync_at,
            error_message: attachment.error_message,
            created_at: attachment.created_at,
            updated_at: attachment.updated_at,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceHarnessContainerMountModeRouteValue {
    DiskIsolated,
    Legacy,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceHarnessContainerNetworkModeRouteValue {
    LlmOnly,
    Allowlist,
    All,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceHarnessContainerStatusRouteResponse {
    pub name: String,
    pub running: bool,
    pub known: bool,
    pub mount_mode: Option<WorkspaceHarnessContainerMountModeRouteValue>,
    pub network_mode: Option<WorkspaceHarnessContainerNetworkModeRouteValue>,
    pub allowlist: Vec<String>,
    pub egress_guard: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceAgentWorkRouteResponse {
    pub change_sets: Vec<ChangeSet>,
    pub contributions: Vec<Contribution>,
}

impl WorkspaceAgentWorkRouteResponse {
    pub fn new(change_sets: Vec<ChangeSet>, contributions: Vec<Contribution>) -> Self {
        Self {
            change_sets,
            contributions,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct WorkspaceAgentWorkRouteQuery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_set_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_json: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct WorkspaceActiveSnapshotRouteResponse {
    value: WorkspaceActiveSnapshot,
}

impl From<WorkspaceActiveSnapshot> for WorkspaceActiveSnapshotRouteResponse {
    fn from(value: WorkspaceActiveSnapshot) -> Self {
        Self { value }
    }
}

impl Serialize for WorkspaceActiveSnapshotRouteResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.value.serialize(serializer)
    }
}

#[derive(Debug, Clone)]
pub struct WorkspaceActiveHeadBatchRouteResponse {
    value: WorkspaceActiveHeadBatch,
}

impl From<WorkspaceActiveHeadBatch> for WorkspaceActiveHeadBatchRouteResponse {
    fn from(value: WorkspaceActiveHeadBatch) -> Self {
        Self { value }
    }
}

impl Serialize for WorkspaceActiveHeadBatchRouteResponse {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.value.serialize(serializer)
    }
}
