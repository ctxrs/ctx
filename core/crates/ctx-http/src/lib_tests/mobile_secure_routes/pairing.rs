use super::*;

fn encrypted_pair_request_value(
    pairing_token: &str,
    device_id: &str,
    daemon_public_key: &str,
    device_public_key: &str,
    device_secret_key: &str,
) -> serde_json::Value {
    let key = ctx_transport_runtime::mobile_e2ee::derive_client_key(
        device_id,
        device_secret_key,
        daemon_public_key,
    )
    .unwrap();
    let payload = json!({
        "pairing_token": pairing_token,
        "device_label": "phone",
        "platform": "ios",
        "app_version": "1.0.0",
    });
    let plaintext = serde_json::to_vec(&payload).unwrap();
    let envelope = ctx_transport_runtime::mobile_e2ee::encrypt_pairing_request(
        &key,
        device_id,
        device_public_key,
        &plaintext,
    )
    .unwrap();
    json!({
        "device_id": envelope.device_id,
        "public_key": device_public_key,
        "seq": envelope.seq,
        "nonce": envelope.nonce_b64,
        "ciphertext": envelope.ciphertext_b64,
    })
}

mod access_state;
mod encrypted_requests;
mod lifecycle;
