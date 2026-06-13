use std::sync::atomic::Ordering;
use std::time::Instant;

use super::*;
use ctx_transport_runtime::dictation_livekit::{
    translate_livekit_dictation_text_message, LiveKitDictationUpstreamEvent,
};
use futures::{SinkExt, StreamExt};
use serde_json::json;

pub(super) struct LiveKitBridge {
    pub(super) lk_rx: LiveKitStream,
    pub(super) client_tx: Arc<Mutex<ClientSink>>,
    pub(super) finalize_requested: Arc<AtomicBool>,
    pub(super) session_closed: Arc<AtomicBool>,
    pub(super) audio_started: Arc<AtomicBool>,
}

pub(super) async fn forward_livekit_transcripts(mut bridge: LiveKitBridge) {
    let mut got_final = false;
    let mut last_transcript_at = Instant::now();
    let mut transcript_messages: u64 = 0;

    while let Some(item) =
        next_livekit_message(&mut bridge.lk_rx, &bridge.finalize_requested, got_final).await
    {
        let Ok(msg) = item else { break };
        match msg {
            TMessage::Text(text) => match translate_livekit_dictation_text_message(&text) {
                LiveKitDictationUpstreamEvent::InterimTranscript { payload } => {
                    transcript_messages = transcript_messages.saturating_add(1);
                    last_transcript_at = Instant::now();
                    if send_client_text(&bridge.client_tx, payload).await.is_err() {
                        break;
                    }
                }
                LiveKitDictationUpstreamEvent::FinalTranscript { payload } => {
                    transcript_messages = transcript_messages.saturating_add(1);
                    last_transcript_at = Instant::now();
                    got_final = true;
                    if send_client_text(&bridge.client_tx, payload).await.is_err() {
                        break;
                    }
                }
                LiveKitDictationUpstreamEvent::SessionFinalized => {
                    bridge.session_closed.store(true, Ordering::Relaxed);
                    break;
                }
                LiveKitDictationUpstreamEvent::Error { payload } => {
                    let _ = send_client_text(&bridge.client_tx, payload).await;
                    break;
                }
                LiveKitDictationUpstreamEvent::SessionClosed => {
                    bridge.session_closed.store(true, Ordering::Relaxed);
                    break;
                }
                LiveKitDictationUpstreamEvent::Ignore => {}
            },
            TMessage::Close(_) => {
                bridge.session_closed.store(true, Ordering::Relaxed);
                break;
            }
            _ => {}
        }

        if bridge.finalize_requested.load(Ordering::Relaxed)
            && got_final
            && last_transcript_at.elapsed() > Duration::from_secs(5)
        {
            break;
        }
    }
    tracing::info!(
        "dictation: livekit recv loop ended got_final={} transcripts={} audio_started={}",
        got_final,
        transcript_messages,
        bridge.audio_started.load(Ordering::Relaxed)
    );
    let _ = send_client_text(&bridge.client_tx, json!({ "type": "done" }).to_string()).await;
}

async fn next_livekit_message(
    lk_rx: &mut LiveKitStream,
    finalize_requested: &AtomicBool,
    got_final: bool,
) -> Option<Result<TMessage, tokio_tungstenite::tungstenite::Error>> {
    if finalize_requested.load(Ordering::Relaxed) {
        let idle_timeout = if got_final {
            Duration::from_secs(5)
        } else {
            Duration::from_secs(60)
        };
        tokio::time::timeout(idle_timeout, lk_rx.next())
            .await
            .unwrap_or_default()
    } else {
        lk_rx.next().await
    }
}

async fn send_client_text(
    client_tx: &Arc<Mutex<ClientSink>>,
    payload: String,
) -> Result<(), axum::Error> {
    client_tx.lock().await.send(WsMessage::Text(payload)).await
}
