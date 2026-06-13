use super::*;
use ctx_core::ids::WorkspaceId;

pub(super) async fn post_mobile_secure_request(
    app: &axum::Router,
    device_id: &str,
    key: &ctx_transport_runtime::mobile_e2ee::E2eeKey,
    seq: i64,
    payload: serde_json::Value,
) -> axum::response::Response {
    let plaintext = serde_json::to_vec(&payload).unwrap();
    let envelope =
        ctx_transport_runtime::mobile_e2ee::encrypt(key, device_id, seq, &plaintext).unwrap();
    let req = Request::builder()
        .method("POST")
        .uri("/api/mobile/secure")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "device_id": envelope.device_id,
                "seq": envelope.seq,
                "nonce": envelope.nonce_b64,
                "ciphertext": envelope.ciphertext_b64
            })
            .to_string(),
        ))
        .unwrap();
    app.clone().oneshot(req).await.unwrap()
}

pub(super) async fn decode_mobile_secure_response(
    res: axum::response::Response,
    device_id: &str,
    key: &ctx_transport_runtime::mobile_e2ee::E2eeKey,
) -> serde_json::Value {
    let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let plaintext = ctx_transport_runtime::mobile_e2ee::decrypt(
        key,
        device_id,
        payload["seq"].as_i64().unwrap(),
        payload["nonce"].as_str().unwrap(),
        payload["ciphertext"].as_str().unwrap(),
    )
    .unwrap();
    serde_json::from_slice(&plaintext).unwrap()
}

pub(super) fn mobile_secure_stream_query(
    device_id: &str,
    key: &ctx_transport_runtime::mobile_e2ee::E2eeKey,
    workspace_id: WorkspaceId,
) -> String {
    let token =
        ctx_transport_runtime::mobile_e2ee::derive_stream_token(key, &workspace_id.0.to_string());
    format!("device_id={device_id}&token={token}")
}
