use anyhow::{anyhow, Context, Result};
use base64::Engine;
use url::Url;

use ctx_transport_runtime::mobile_e2ee;

use super::dto::{
    PairMobileDevicePayload, PairMobileDeviceReq, SecureEnvelope, SecureRequestPayload,
};

pub(super) fn parse_qr_payload(payload: &serde_json::Value) -> Result<(String, String, String)> {
    let base_url = payload
        .get("base_url")
        .and_then(|v| v.as_str())
        .map(|v| v.trim_end_matches('/').to_string())
        .context("qr payload missing base_url")?;
    let pairing_token = payload
        .get("pairing_token")
        .and_then(|v| v.as_str())
        .context("qr payload missing pairing_token")?
        .to_string();
    let daemon_public_key = payload
        .get("daemon_public_key")
        .and_then(|v| v.as_str())
        .context("qr payload missing daemon_public_key")?
        .to_string();
    let encryption = payload
        .get("pairing_request_encryption")
        .and_then(|v| v.as_str())
        .context("qr payload missing pairing_request_encryption")?;
    if encryption != mobile_e2ee::PAIRING_REQUEST_ENCRYPTION {
        return Err(anyhow!(
            "unsupported pairing request encryption: {encryption}"
        ));
    }
    Ok((base_url, pairing_token, daemon_public_key))
}

pub(super) fn encrypt_pairing_request(
    key: &mobile_e2ee::E2eeKey,
    device_id: &str,
    device_public: &str,
    payload: PairMobileDevicePayload,
) -> Result<PairMobileDeviceReq> {
    let plaintext = serde_json::to_vec(&payload)?;
    let enc = mobile_e2ee::encrypt_pairing_request(key, device_id, device_public, &plaintext)?;
    Ok(PairMobileDeviceReq {
        device_id: enc.device_id,
        public_key: device_public.to_string(),
        seq: enc.seq,
        nonce: enc.nonce_b64,
        ciphertext: enc.ciphertext_b64,
    })
}

pub(super) fn encrypt_secure_request(
    key: &mobile_e2ee::E2eeKey,
    device_id: &str,
    seq: i64,
    path: &str,
) -> Result<SecureEnvelope> {
    let req_payload = SecureRequestPayload {
        method: "GET".to_string(),
        path: path.to_string(),
        query: None,
        headers: Vec::new(),
        body_b64: String::new(),
    };
    let payload_bytes = serde_json::to_vec(&req_payload)?;
    let enc = mobile_e2ee::encrypt(key, device_id, seq, &payload_bytes)?;
    Ok(SecureEnvelope {
        device_id: enc.device_id,
        seq: enc.seq,
        nonce: enc.nonce_b64,
        ciphertext: enc.ciphertext_b64,
    })
}

pub(super) fn build_ws_url(
    base_url: &str,
    workspace_id: &str,
    device_id: &str,
    key: &mobile_e2ee::E2eeKey,
) -> Result<Url> {
    let mut url = Url::parse(base_url)?;
    let ws_scheme = match url.scheme() {
        "https" => "wss",
        "http" => "ws",
        other => return Err(anyhow!("unsupported base url scheme: {other}")),
    };
    url.set_scheme(ws_scheme)
        .map_err(|_| anyhow!("failed to set ws scheme"))?;
    let prefix = url.path().trim_end_matches('/');
    let path = if prefix.is_empty() {
        format!("/api/mobile/secure/workspaces/{workspace_id}/stream")
    } else {
        format!("{prefix}/api/mobile/secure/workspaces/{workspace_id}/stream")
    };
    url.set_path(&path);
    let token = mobile_e2ee::derive_stream_token(key, workspace_id);
    url.set_query(Some(&format!("device_id={device_id}&token={token}")));
    Ok(url)
}

pub(super) fn decode_body_b64(value: &str) -> Result<Vec<u8>> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let mut normalized = trimmed.replace('-', "+").replace('_', "/");
    while !normalized.len().is_multiple_of(4) {
        normalized.push('=');
    }
    base64::engine::general_purpose::STANDARD
        .decode(normalized.as_bytes())
        .map_err(|_| anyhow!("invalid base64 body"))
}
