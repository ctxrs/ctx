use axum::extract::ws::Message as WsMessage;
use futures::{Sink, SinkExt};

use crate::api::SecureEnvelope;

pub(in crate::api::ws) async fn send_secure_ws<S>(
    sink: &mut S,
    key: &ctx_transport_runtime::mobile_e2ee::E2eeKey,
    device_id: &str,
    seq: i64,
    payload: &ctx_core::models::WorkspaceActiveSnapshotStreamMessage,
) -> Result<(), anyhow::Error>
where
    S: Sink<WsMessage> + Unpin,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    let plaintext = serde_json::to_vec(payload)?;
    let envelope = ctx_transport_runtime::mobile_e2ee::encrypt(key, device_id, seq, &plaintext)?;
    let frame = SecureEnvelope {
        device_id: envelope.device_id,
        seq: envelope.seq,
        nonce: envelope.nonce_b64,
        ciphertext: envelope.ciphertext_b64,
    };
    let text = serde_json::to_string(&frame)?;
    sink.send(WsMessage::Text(text)).await?;
    Ok(())
}
