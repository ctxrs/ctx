use std::{fmt, str::FromStr};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::CoreError;

text_enum! {
    pub enum Visibility {
        LocalOnly => "local_only",
        Reportable => "reportable",
        SyncMetadata => "sync_metadata",
        SyncFull => "sync_full",
        Withheld => "withheld",
    }
    default LocalOnly
}

text_enum! {
    pub enum Fidelity {
        Full => "full",
        Partial => "partial",
        Imported => "imported",
        Inferred => "inferred",
        SummaryOnly => "summary_only",
    }
    default Partial
}

text_enum! {
    pub enum SyncState {
        LocalOnly => "local_only",
        Pending => "pending",
        Synced => "synced",
        Failed => "failed",
        Withheld => "withheld",
    }
    default LocalOnly
}

text_enum! {
    pub enum SyncDirection {
        Upload => "upload",
        Download => "download",
    }
    default Upload
}

text_enum! {
    pub enum SyncBatchStatus {
        Pending => "pending",
        Running => "running",
        Succeeded => "succeeded",
        Failed => "failed",
    }
    default Pending
}

text_enum! {
    pub enum SyncOutboxOperation {
        Insert => "insert",
        Update => "update",
        Delete => "delete",
        BlobUpload => "blob_upload",
    }
    default Insert
}

text_enum! {
    pub enum AuditActorKind {
        Human => "human",
        Agent => "agent",
        System => "system",
    }
    default System
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncMetadata {
    #[serde(default)]
    pub visibility: Visibility,
    #[serde(default)]
    pub fidelity: Fidelity,
    #[serde(default)]
    pub sync_state: SyncState,
    #[serde(default)]
    pub sync_version: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<DateTime<Utc>>,
    #[serde(default = "default_metadata")]
    pub metadata: serde_json::Value,
}

impl Default for SyncMetadata {
    fn default() -> Self {
        Self {
            visibility: Visibility::default(),
            fidelity: Fidelity::default(),
            sync_state: SyncState::default(),
            sync_version: 0,
            deleted_at: None,
            metadata: default_metadata(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityTimestamps {
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncAlias {
    pub id: Uuid,
    pub local_table: String,
    pub local_id: String,
    pub hosted_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    #[serde(flatten)]
    pub timestamps: EntityTimestamps,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncCursor {
    pub id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    pub device_id: String,
    pub stream: String,
    pub cursor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_synced_at: Option<DateTime<Utc>>,
    #[serde(flatten)]
    pub timestamps: EntityTimestamps,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncBatch {
    pub id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    pub device_id: String,
    pub direction: SyncDirection,
    pub status: SyncBatchStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub row_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(flatten)]
    pub timestamps: EntityTimestamps,
    #[serde(default = "default_metadata")]
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncOutboxItem {
    pub id: Uuid,
    pub local_table: String,
    pub local_id: String,
    pub operation: SyncOutboxOperation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    pub device_id: String,
    #[serde(default = "default_pending_sync_state")]
    pub sync_state: SyncState,
    #[serde(default)]
    pub attempt_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_attempt_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default = "default_metadata")]
    pub payload: serde_json::Value,
    #[serde(flatten)]
    pub timestamps: EntityTimestamps,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditLogEntry {
    pub id: Uuid,
    pub actor_kind: AuditActorKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_id: Option<String>,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_table: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
    pub occurred_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<Uuid>,
    #[serde(default = "default_metadata")]
    pub metadata: serde_json::Value,
}

pub(crate) fn default_metadata() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

fn default_pending_sync_state() -> SyncState {
    SyncState::Pending
}
