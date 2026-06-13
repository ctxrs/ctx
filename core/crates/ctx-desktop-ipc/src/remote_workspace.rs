use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopDeepLinkToken {
    #[ts(type = "number")]
    pub expires_at_ms: u64,
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopSshHost {
    pub host: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub host_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub user: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopSshTestReq {
    pub host: String,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub password_once: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub user: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopRemotePrewarmReq {
    pub host: String,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub remote_data_dir: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub remote_port: Option<u16>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub user: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopSshPathReq {
    pub host: String,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub path: Option<String>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub user: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopSshPathEntry {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopGitBranchReq {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct DesktopGitCloneReq {
    pub dest_parent: String,
    pub repo_url: String,
}
