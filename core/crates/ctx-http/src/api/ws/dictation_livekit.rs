use axum::extract::ws::{Message as WsMessage, WebSocket};
use ctx_transport_runtime::dictation_livekit::connect_livekit_inference_stt;
use serde_json::json;

use ctx_daemon::daemon::DictationHandle;

mod bridge;
mod settings;

pub async fn dictation_livekit_stream(mut socket: WebSocket, state: DictationHandle) {
    tracing::info!("dictation: client connected");
    let cfg = match settings::load_livekit_dictation_config(&state).await {
        Ok(cfg) => cfg,
        Err(error) => {
            settings::send_dictation_error(&mut socket, error).await;
            return;
        }
    };

    let lk_ws = match connect_livekit_inference_stt(&cfg).await {
        Ok(ws) => ws,
        Err(e) => {
            settings::send_dictation_error(
                &mut socket,
                settings::DictationStreamError::new(
                    format!("Failed to connect to LiveKit Inference STT: {e:#}"),
                    "{\"type\":\"error\",\"message\":\"connect failed\"}",
                ),
            )
            .await;
            return;
        }
    };

    let _ = socket
        .send(WsMessage::Text(json!({ "type": "ready" }).to_string()))
        .await;

    bridge::run_livekit_dictation_bridge(socket, lk_ws).await;
    tracing::info!("dictation: client disconnected");
}
