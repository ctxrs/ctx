use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub(in crate::api) struct EnableMobileAccessReq {}

#[derive(Debug, Serialize)]
pub(in crate::api) struct MobileAccessStatus {
    pub(in crate::api) enabled: bool,
    pub(in crate::api) tunnel_id: Option<String>,
    pub(in crate::api) public_base_url: Option<String>,
    pub(in crate::api) relay_base_url: Option<String>,
    pub(in crate::api) daemon_public_key: Option<String>,
    pub(in crate::api) tunnel_state: ctx_transport_runtime::mobile_tunnel::MobileTunnelState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(in crate::api) last_error: Option<String>,
}

#[derive(Debug, Serialize)]
pub(in crate::api) struct EnableMobileAccessResp {
    pub(in crate::api) status: MobileAccessStatus,
    pub(in crate::api) qr_payload: serde_json::Value,
    pub(in crate::api) pairing_expires_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::api) struct PairMobileDeviceReq {
    pub(in crate::api) device_id: String,
    pub(in crate::api) public_key: String,
    pub(in crate::api) seq: i64,
    pub(in crate::api) nonce: String,
    pub(in crate::api) ciphertext: String,
}

#[derive(Debug, Serialize)]
pub(in crate::api) struct SecureEnvelope {
    pub(in crate::api) device_id: String,
    pub(in crate::api) seq: i64,
    pub(in crate::api) nonce: String,
    pub(in crate::api) ciphertext: String,
}

#[derive(Debug, Deserialize)]
pub(in crate::api) struct MobileSecureEnvelope {
    pub(in crate::api) device_id: String,
    pub(in crate::api) seq: i64,
    pub(in crate::api) nonce: String,
    pub(in crate::api) ciphertext: String,
}

#[derive(Debug, Deserialize)]
pub(in crate::api) struct MobileSecureStreamQuery {
    pub(in crate::api) device_id: String,
    pub(in crate::api) token: String,
}
