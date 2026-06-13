use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message as WsMessage, WebSocket};
use futures::StreamExt;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message as TMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type LiveKitSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type ClientSink = futures::stream::SplitSink<WebSocket, WsMessage>;
type ClientStream = futures::stream::SplitStream<WebSocket>;
type LiveKitSink = futures::stream::SplitSink<LiveKitSocket, TMessage>;
type LiveKitStream = futures::stream::SplitStream<LiveKitSocket>;

mod client;
mod close;
mod livekit;

pub(super) async fn run_livekit_dictation_bridge(socket: WebSocket, lk_ws: LiveKitSocket) {
    let (client_tx, client_rx) = socket.split();
    let client_tx = Arc::new(Mutex::new(client_tx));
    let (lk_tx, lk_rx) = lk_ws.split();
    let lk_tx = Arc::new(Mutex::new(lk_tx));

    let finalize_requested = Arc::new(AtomicBool::new(false));
    let session_closed = Arc::new(AtomicBool::new(false));
    let audio_started = Arc::new(AtomicBool::new(false));
    let close_scheduled = Arc::new(AtomicBool::new(false));
    let client_tx_send = client_tx.clone();
    let client_tx_recv = client_tx.clone();
    let lk_tx_send = lk_tx.clone();
    let lk_tx_close = lk_tx.clone();
    let finalize_requested_send = finalize_requested.clone();
    let finalize_requested_recv = finalize_requested.clone();
    let session_closed_send = session_closed.clone();
    let session_closed_recv = session_closed.clone();
    let audio_started_send = audio_started.clone();
    let audio_started_recv = audio_started.clone();
    let close_scheduled_send = close_scheduled.clone();

    let send_task = tokio::spawn(async move {
        client::forward_client_audio_and_control(client::ClientBridge {
            client_rx,
            client_tx: client_tx_send,
            lk_tx: lk_tx_send,
            lk_tx_close,
            finalize_requested: finalize_requested_send,
            session_closed: session_closed_send,
            audio_started: audio_started_send,
            close_scheduled: close_scheduled_send,
        })
        .await;
    });

    let recv_task = tokio::spawn(async move {
        livekit::forward_livekit_transcripts(livekit::LiveKitBridge {
            lk_rx,
            client_tx: client_tx_recv,
            finalize_requested: finalize_requested_recv,
            session_closed: session_closed_recv,
            audio_started: audio_started_recv,
        })
        .await;
    });

    let _ = tokio::join!(send_task, recv_task);
}
