use super::*;
use futures::SinkExt;
use std::sync::atomic::Ordering;

pub(super) fn schedule_livekit_close(
    lk_tx_close: Arc<Mutex<LiveKitSink>>,
    session_closed: Arc<AtomicBool>,
) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        if session_closed.load(Ordering::Relaxed) {
            return;
        }
        let _ = lk_tx_close.lock().await.send(TMessage::Close(None)).await;
    });
}
