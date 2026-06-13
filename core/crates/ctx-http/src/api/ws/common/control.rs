use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub(in crate::api::ws) const WORKSPACE_STREAM_QUEUE_LIMIT: usize = 256;
pub(in crate::api::ws) const WORKSPACE_STREAM_QUEUE_MAX_AGE: Duration = Duration::from_secs(10);
pub(in crate::api::ws) const HEAD_BATCH_FLUSH_INTERVAL: Duration = Duration::from_millis(25);
pub(in crate::api::ws) const HEAD_BATCH_SESSION_LIMIT: usize = 200;

pub(in crate::api::ws) struct StreamSendControl {
    disconnect_after_flush: AtomicBool,
    hydrating: AtomicBool,
}

impl StreamSendControl {
    pub(in crate::api::ws) fn new() -> Self {
        Self {
            disconnect_after_flush: AtomicBool::new(false),
            hydrating: AtomicBool::new(false),
        }
    }

    pub(in crate::api::ws) fn set_disconnect_after_flush(&self) {
        self.disconnect_after_flush.store(true, Ordering::Relaxed);
    }

    pub(in crate::api::ws) fn clear_disconnect_after_flush(&self) {
        self.disconnect_after_flush.store(false, Ordering::Relaxed);
    }

    pub(in crate::api::ws) fn should_disconnect_after_flush(&self) -> bool {
        self.disconnect_after_flush.load(Ordering::Relaxed)
    }

    pub(in crate::api::ws) fn set_hydrating(&self) {
        self.hydrating.store(true, Ordering::Relaxed);
    }

    pub(in crate::api::ws) fn clear_hydrating(&self) {
        self.hydrating.store(false, Ordering::Relaxed);
    }

    pub(in crate::api::ws) fn is_hydrating(&self) -> bool {
        self.hydrating.load(Ordering::Relaxed)
    }
}
