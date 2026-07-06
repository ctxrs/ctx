#[allow(unused_imports)]
use super::*;

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
