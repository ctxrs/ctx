use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::*;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceAttachmentKind {
    ReferenceRepo,
    DocMirror,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentMode {
    Ro,
    Rw,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentUpdatePolicy {
    Manual,
    OnOpen,
    Scheduled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceAttachmentStatus {
    Pending,
    Syncing,
    Ready,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceAttachment {
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeAttachmentStatus {
    Ready,
    Stale,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeAttachmentMount {
    pub worktree_id: WorktreeId,
    pub attachment_id: WorkspaceAttachmentId,
    pub mount_abs_path: String,
    pub materialized_id: String,
    pub status: WorktreeAttachmentStatus,
    pub last_sync_at: Option<DateTime<Utc>>,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
