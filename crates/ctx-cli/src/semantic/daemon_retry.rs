#[derive(Debug, Default)]
pub(super) struct DaemonRetryBackoff {
    pub(super) consecutive_failures: u32,
    pub(super) retry_not_before: Option<Instant>,
    pub(super) retry_not_before_at_ms: Option<i64>,
}

impl DaemonRetryBackoff {
    pub(super) const BASE_DELAY: StdDuration = StdDuration::from_secs(10);
    pub(super) const MAX_DELAY: StdDuration = StdDuration::from_secs(5 * 60);

    pub(super) fn ready(&self) -> bool {
        self.retry_not_before
            .is_none_or(|retry_not_before| Instant::now() >= retry_not_before)
    }

    pub(super) fn retry_after_ms(&self) -> Option<u64> {
        self.retry_not_before.map(|retry_not_before| {
            retry_not_before
                .saturating_duration_since(Instant::now())
                .as_millis()
                .min(u128::from(u64::MAX)) as u64
        })
    }

    pub(super) fn record_failure(&mut self) -> StdDuration {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        let exponent = self.consecutive_failures.saturating_sub(1).min(31);
        let multiplier = 1_u32.checked_shl(exponent).unwrap_or(u32::MAX);
        let delay = Self::BASE_DELAY
            .checked_mul(multiplier)
            .unwrap_or(Self::MAX_DELAY)
            .min(Self::MAX_DELAY);
        self.retry_not_before = Some(Instant::now() + delay);
        self.retry_not_before_at_ms = Some(
            utc_now()
                .timestamp_millis()
                .saturating_add(delay.as_millis().min(i64::MAX as u128) as i64),
        );
        delay
    }

    pub(super) fn restore(&mut self, value: Option<&Value>) {
        let Some(value) = value else {
            return;
        };
        let Some(retry_at_ms) = value.get("retry_not_before_at_ms").and_then(Value::as_i64) else {
            return;
        };
        let remaining_ms = retry_at_ms.saturating_sub(utc_now().timestamp_millis());
        if remaining_ms <= 0 {
            return;
        }
        self.consecutive_failures = value
            .get("consecutive_failures")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            .min(u64::from(u32::MAX)) as u32;
        self.retry_not_before =
            Some(Instant::now() + StdDuration::from_millis(remaining_ms as u64));
        self.retry_not_before_at_ms = Some(retry_at_ms);
    }

    pub(super) fn reset(&mut self) {
        self.consecutive_failures = 0;
        self.retry_not_before = None;
        self.retry_not_before_at_ms = None;
    }
}
use std::time::{Duration as StdDuration, Instant};

use ctx_history_core::utc_now;
use serde_json::Value;
