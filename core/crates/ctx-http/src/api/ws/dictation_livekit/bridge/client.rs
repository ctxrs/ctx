use super::*;
use ctx_transport_runtime::dictation_livekit::{
    livekit_client_control_requests_stop, livekit_dictation_finalize_payload,
    livekit_dictation_input_audio_payload,
};
use futures::{SinkExt, StreamExt};
use serde_json::json;
use std::sync::atomic::Ordering;

pub(super) struct ClientBridge {
    pub(super) client_rx: ClientStream,
    pub(super) client_tx: Arc<Mutex<ClientSink>>,
    pub(super) lk_tx: Arc<Mutex<LiveKitSink>>,
    pub(super) lk_tx_close: Arc<Mutex<LiveKitSink>>,
    pub(super) finalize_requested: Arc<AtomicBool>,
    pub(super) session_closed: Arc<AtomicBool>,
    pub(super) audio_started: Arc<AtomicBool>,
    pub(super) close_scheduled: Arc<AtomicBool>,
}

pub(super) async fn forward_client_audio_and_control(mut bridge: ClientBridge) {
    let mut finalized = false;
    let mut audio_bytes_sent: u64 = 0;
    while let Some(Ok(msg)) = bridge.client_rx.next().await {
        match msg {
            WsMessage::Binary(bytes) if !finalized => {
                audio_bytes_sent = audio_bytes_sent.saturating_add(bytes.len() as u64);
                if !bridge.audio_started.swap(true, Ordering::Relaxed) {
                    let _ = bridge
                        .client_tx
                        .lock()
                        .await
                        .send(WsMessage::Text(
                            json!({ "type": "audio_started" }).to_string(),
                        ))
                        .await;
                }
                let payload = livekit_dictation_input_audio_payload(&bytes);
                if bridge
                    .lk_tx
                    .lock()
                    .await
                    .send(TMessage::Text(payload.into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            WsMessage::Binary(_) => {}
            WsMessage::Text(text) if livekit_client_control_requests_stop(&text) && !finalized => {
                request_livekit_finalize(&bridge).await;
                finalized = true;
            }
            WsMessage::Text(_) => {}
            WsMessage::Ping(payload) => {
                let _ = bridge
                    .client_tx
                    .lock()
                    .await
                    .send(WsMessage::Pong(payload))
                    .await;
            }
            WsMessage::Close(_) => {
                if !finalized {
                    request_livekit_finalize(&bridge).await;
                }
                break;
            }
            _ => {}
        }
    }
    tracing::info!(
        "dictation: client send loop ended finalized={} bytes={}",
        finalized,
        audio_bytes_sent
    );
}

async fn request_livekit_finalize(bridge: &ClientBridge) {
    bridge.finalize_requested.store(true, Ordering::Relaxed);
    let _ = bridge
        .lk_tx
        .lock()
        .await
        .send(TMessage::Text(livekit_dictation_finalize_payload().into()))
        .await;
    if !bridge.close_scheduled.swap(true, Ordering::Relaxed) {
        close::schedule_livekit_close(bridge.lk_tx_close.clone(), bridge.session_closed.clone());
    }
}
