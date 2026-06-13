use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopAppUpdateAttemptStageResp {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub error_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(type = "number", optional = nullable)]
    pub finished_at_ms: Option<u64>,
    pub result: String,
    pub stage: String,
    #[ts(type = "number")]
    pub started_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopAppUpdateAttemptResp {
    pub attempt_id: String,
    pub channel: String,
    pub current_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(type = "number", optional = nullable)]
    pub finished_at_ms: Option<u64>,
    pub result: String,
    pub stages: Vec<DesktopAppUpdateAttemptStageResp>,
    #[ts(type = "number")]
    pub started_at_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub target_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopAppUpdateCheckReq {
    #[serde(default)]
    #[ts(optional = nullable)]
    pub channel: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopUpdateChannelSettings {
    pub channel: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopAppUpdateCheckResp {
    pub available: bool,
    pub configured: bool,
    pub current_version: String,
    pub endpoint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub last_attempt_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub last_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub latest_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub message: Option<String>,
    pub phase: String,
    pub restart_required: bool,
    pub staged: bool,
    pub target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopAppUpdateStateResp {
    pub available: bool,
    pub configured: bool,
    pub current_version: String,
    pub endpoint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub last_attempt_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub last_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub latest_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub message: Option<String>,
    pub phase: String,
    pub restart_required: bool,
    pub staged: bool,
    pub target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopAppUpdateApplyReq {
    #[serde(default)]
    #[ts(optional = nullable)]
    pub channel: Option<String>,
    #[serde(default)]
    pub confirm: bool,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub download_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopAppUpdateApplyResp {
    pub applied: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub latest_version: Option<String>,
    pub message: String,
    pub needs_restart: bool,
    pub up_to_date: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopAppRestartResp {
    pub message: String,
    pub requested: bool,
}
