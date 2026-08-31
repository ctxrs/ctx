use std::time::{Duration as StdDuration, Instant};

const DAEMON_CONSUMER_RETRY_QUERY_GRACE: StdDuration = StdDuration::from_secs(2);

#[derive(Debug, Default)]
pub(crate) struct DaemonConsumerRetryDeferral {
    pub(crate) retry_at: Option<Instant>,
}

impl DaemonConsumerRetryDeferral {
    pub(super) fn defer_for_foreground_query(&mut self, now: Instant) -> bool {
        let retry_at = self
            .retry_at
            .get_or_insert(now + DAEMON_CONSUMER_RETRY_QUERY_GRACE);
        if now < *retry_at {
            return true;
        }
        self.reset();
        false
    }

    pub(crate) fn remaining(&self, now: Instant) -> Option<StdDuration> {
        self.retry_at
            .and_then(|retry_at| retry_at.checked_duration_since(now))
    }

    pub(super) fn reset(&mut self) {
        self.retry_at = None;
    }
}
