use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum DesktopConnectionKind {
    None,
    Local,
    Ssh,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum DesktopConnectionIntent {
    AutoLocalBootstrap,
    ExplicitLocal,
    ExplicitRemote,
    ExplicitDisconnected,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, TS, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DesktopRemoteDaemonUpdateState {
    Pending,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopConnectionInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub base_url: Option<String>,
    #[ts(optional, as = "Option<_>")]
    pub intent: DesktopConnectionIntent,
    pub kind: DesktopConnectionKind,
    #[ts(optional, as = "Option<_>")]
    pub local_auto_bootstrap_allowed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub remote_data_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub remote_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub remote_update_message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub remote_update_state: Option<DesktopRemoteDaemonUpdateState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub browser_query_secret: Option<String>,
    #[serde(skip_serializing)]
    #[ts(skip)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub user: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct SshConnectReq {
    pub host: String,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub password_once: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub remote_data_dir: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub remote_port: Option<u16>,
    #[serde(default = "default_true")]
    pub start_remote: bool,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub user: Option<String>,
}

const fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopSshConnectPollReq {
    #[serde(default)]
    pub consume: bool,
    pub job_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopSshConnectJobStatus {
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(type = "number", optional = nullable)]
    pub created_at_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub info: Option<DesktopConnectionInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub phase: Option<String>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(type = "number", optional = nullable)]
    pub updated_at_ms: Option<u64>,
}
