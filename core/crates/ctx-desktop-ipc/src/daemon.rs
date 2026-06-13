use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopDaemonRequest {
    #[serde(default)]
    #[ts(optional = nullable)]
    pub body: Option<String>,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    pub method: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopHttpResponse {
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub content_type: Option<String>,
    pub status: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopRemoteDaemonUpdateReq {
    #[serde(default)]
    #[ts(optional = nullable)]
    pub channel: Option<String>,
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopRemoteDaemonUpdateResp {
    pub message: String,
    pub updated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopLinuxSandboxEnsureResp {
    pub ready: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopLocalLinuxSandboxEnsureReq {
    #[serde(default)]
    #[ts(optional = nullable)]
    pub admin_password_once: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopRemoteLinuxSandboxEnsureReq {
    #[serde(default)]
    #[ts(optional = nullable)]
    pub admin_password_once: Option<String>,
}
