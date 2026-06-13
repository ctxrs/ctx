use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub(super) struct EnableMobileAccessResp {
    pub(super) qr_payload: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub(super) struct EnableMobileAccessReq {
    pub(super) supabase_token: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct DaemonAuthFile {
    pub(super) token: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct SecureEnvelope {
    pub(super) device_id: String,
    pub(super) seq: i64,
    pub(super) nonce: String,
    pub(super) ciphertext: String,
}

#[derive(Debug, Serialize)]
pub(super) struct PairMobileDeviceReq {
    pub(super) device_id: String,
    pub(super) public_key: String,
    pub(super) seq: i64,
    pub(super) nonce: String,
    pub(super) ciphertext: String,
}

#[derive(Debug, Serialize)]
pub(super) struct PairMobileDevicePayload {
    pub(super) pairing_token: String,
    pub(super) device_label: Option<String>,
    pub(super) platform: Option<String>,
    pub(super) app_version: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct SecureRequestPayload {
    pub(super) method: String,
    pub(super) path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) query: Option<String>,
    #[serde(default)]
    pub(super) headers: Vec<(String, String)>,
    #[serde(default)]
    pub(super) body_b64: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct SecureResponsePayload {
    pub(super) status: u16,
    pub(super) body_b64: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct WorkspaceSummary {
    pub(super) id: String,
}
