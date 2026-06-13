use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MobileTunnelState {
    Idle,
    Running,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MobileAccessStatus {
    pub enabled: bool,
    #[serde(default)]
    pub tunnel_id: Option<String>,
    #[serde(default)]
    pub public_base_url: Option<String>,
    #[serde(default)]
    pub relay_base_url: Option<String>,
    #[serde(default)]
    pub daemon_public_key: Option<String>,
    pub tunnel_state: MobileTunnelState,
    #[serde(default)]
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnableMobileAccessResponse {
    pub status: MobileAccessStatus,
    pub qr_payload: Value,
    pub pairing_expires_at: String,
}
