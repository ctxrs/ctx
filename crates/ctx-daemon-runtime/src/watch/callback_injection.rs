use super::*;

impl NativeFileWatcher {
    #[doc(hidden)]
    pub fn inject_callback_event_for_test(&self, event: NativeWatchResult) {
        forward_native_watch_event(
            &self.sender,
            self.ingress.as_ref(),
            self.accepting_events.as_ref(),
            self.watcher_epoch,
            self.callback_sequence.as_ref(),
            &self.ignore_event,
            &self.overflow_fence,
            event,
        );
    }
}
