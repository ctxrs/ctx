use std::time::{Duration as StdDuration, Instant};

use ctx_history_core::utc_now;
use ctx_semantic_index::{semantic_vector_failure_kind, SemanticVectorFailureKind};
use ctx_semantic_model::{
    semantic_embedding_failure_is_permanent, semantic_model_acquisition_integrity_error,
    SemanticModelLoadDeferred,
};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SemanticFailureClass {
    Retryable,
    Permanent,
    CorruptSidecar,
    ResourcePressure,
}

impl SemanticFailureClass {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Retryable => "retryable",
            Self::Permanent => "permanent",
            Self::CorruptSidecar => "corrupt_sidecar",
            Self::ResourcePressure => "resource_pressure",
        }
    }

    pub(super) fn parse(value: &str) -> Option<Self> {
        match value {
            "retryable" => Some(Self::Retryable),
            "permanent" => Some(Self::Permanent),
            "corrupt_sidecar" => Some(Self::CorruptSidecar),
            "resource_pressure" => Some(Self::ResourcePressure),
            _ => None,
        }
    }

    pub(super) fn retries_with_backoff(self) -> bool {
        self == Self::Retryable
    }

    pub(super) fn blocks_until_restart(self) -> bool {
        matches!(self, Self::Permanent | Self::CorruptSidecar)
    }
}

pub(super) fn classify_semantic_failure(error: &anyhow::Error) -> SemanticFailureClass {
    if semantic_embedding_failure_is_permanent(error) {
        return SemanticFailureClass::Permanent;
    }
    if error.downcast_ref::<SemanticModelLoadDeferred>().is_some() {
        return SemanticFailureClass::ResourcePressure;
    }
    if let Some(kind) = semantic_vector_failure_kind(error) {
        return match kind {
            SemanticVectorFailureKind::Unavailable => SemanticFailureClass::Retryable,
            SemanticVectorFailureKind::ResetRequired => SemanticFailureClass::CorruptSidecar,
            SemanticVectorFailureKind::StorageConflict | SemanticVectorFailureKind::NewerSchema => {
                SemanticFailureClass::Permanent
            }
        };
    }
    if semantic_model_acquisition_integrity_error(error) {
        return SemanticFailureClass::Permanent;
    }
    if let Some(code) = semantic_sqlite_error_code(error) {
        return match code {
            rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase => {
                SemanticFailureClass::CorruptSidecar
            }
            rusqlite::ErrorCode::DiskFull => SemanticFailureClass::ResourcePressure,
            rusqlite::ErrorCode::ReadOnly | rusqlite::ErrorCode::PermissionDenied => {
                SemanticFailureClass::Permanent
            }
            _ => SemanticFailureClass::Retryable,
        };
    }
    if error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|error| error.kind() == std::io::ErrorKind::PermissionDenied)
    {
        return SemanticFailureClass::Permanent;
    }
    SemanticFailureClass::Retryable
}

fn semantic_sqlite_error_code(error: &anyhow::Error) -> Option<rusqlite::ErrorCode> {
    let error = error.downcast_ref::<rusqlite::Error>()?;
    let rusqlite::Error::SqliteFailure(failure, _) = error else {
        return None;
    };
    Some(failure.code)
}

pub(super) fn annotate_semantic_failure(mut job: Value, class: SemanticFailureClass) -> Value {
    job["failure_class"] = Value::String(class.as_str().to_owned());
    job["retryable"] = Value::Bool(matches!(
        class,
        SemanticFailureClass::Retryable | SemanticFailureClass::ResourcePressure
    ));
    job
}

pub(super) fn semantic_failure_class_from_job(job: &Value) -> Option<SemanticFailureClass> {
    job.get("failure_class")
        .and_then(Value::as_str)
        .and_then(SemanticFailureClass::parse)
}

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
            let remaining = retry_not_before.saturating_duration_since(Instant::now());
            let millis = remaining
                .as_nanos()
                .div_ceil(1_000_000)
                .min(u128::from(u64::MAX)) as u64;
            if remaining.is_zero() {
                0
            } else {
                millis.max(1)
            }
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
        let now_ms = utc_now().timestamp_millis();
        let remaining_ms = retry_at_ms.saturating_sub(now_ms);
        if remaining_ms <= 0 {
            return;
        }
        let delay = StdDuration::from_millis(remaining_ms as u64).min(Self::MAX_DELAY);
        self.consecutive_failures = value
            .get("consecutive_failures")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            .min(u64::from(u32::MAX)) as u32;
        self.retry_not_before = Some(Instant::now() + delay);
        self.retry_not_before_at_ms =
            Some(now_ms.saturating_add(delay.as_millis().min(i64::MAX as u128) as i64));
    }

    pub(super) fn reset(&mut self) {
        self.consecutive_failures = 0;
        self.retry_not_before = None;
        self.retry_not_before_at_ms = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        daemon_scheduler::daemon_job_should_backoff, daemon_worker::daemon_semantic_job_json,
    };
    use ctx_semantic_index::test_support::{
        newer_schema_error, reset_required_error, semantic_vector_schema_version,
        storage_conflict_error, unavailable_error,
    };
    use serde_json::json;

    #[test]
    fn restored_daemon_retry_deadline_is_clamped_to_runtime_maximum() {
        let now_ms = utc_now().timestamp_millis();
        let persisted = json!({
            "consecutive_failures": 99,
            "retry_not_before_at_ms": now_ms + 24 * 60 * 60 * 1_000,
        });
        let mut backoff = DaemonRetryBackoff::default();
        backoff.restore(Some(&persisted));
        let restored_at_ms = utc_now().timestamp_millis();

        let maximum_ms = DaemonRetryBackoff::MAX_DELAY.as_millis() as u64;
        assert!(
            backoff
                .retry_after_ms()
                .is_some_and(|remaining| remaining <= maximum_ms),
            "{backoff:#?}"
        );
        assert!(
            backoff.retry_not_before_at_ms.is_some_and(|deadline| {
                deadline > now_ms && deadline <= restored_at_ms + maximum_ms as i64
            }),
            "{backoff:#?}"
        );
        assert_eq!(backoff.consecutive_failures, 99);
    }

    #[test]
    fn semantic_failure_classes_control_retry_backoff() {
        let retryable = anyhow::anyhow!("transient flat segment publication failure");
        assert_eq!(
            classify_semantic_failure(&retryable),
            SemanticFailureClass::Retryable
        );
        let permanent = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied").into();
        assert_eq!(
            classify_semantic_failure(&permanent),
            SemanticFailureClass::Permanent
        );
        let corrupt: anyhow::Error = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
            None,
        )
        .into();
        assert_eq!(
            classify_semantic_failure(&corrupt),
            SemanticFailureClass::CorruptSidecar
        );
        for typed in [
            storage_conflict_error("sidecar identity changed"),
            newer_schema_error(semantic_vector_schema_version() + 1),
        ] {
            assert_eq!(
                classify_semantic_failure(&anyhow::Error::new(typed)),
                SemanticFailureClass::Permanent
            );
        }
        assert_eq!(
            classify_semantic_failure(&anyhow::Error::new(reset_required_error(
                "sidecar reset required"
            ))),
            SemanticFailureClass::CorruptSidecar
        );
        assert_eq!(
            classify_semantic_failure(&anyhow::Error::new(unavailable_error(
                "flat segment store temporarily unavailable"
            ))),
            SemanticFailureClass::Retryable
        );
        let pressure = SemanticModelLoadDeferred::for_test(1, 2);
        assert_eq!(
            classify_semantic_failure(&anyhow::Error::new(pressure)),
            SemanticFailureClass::ResourcePressure
        );

        for (class, should_backoff) in [
            (SemanticFailureClass::Retryable, true),
            (SemanticFailureClass::Permanent, false),
            (SemanticFailureClass::CorruptSidecar, false),
            (SemanticFailureClass::ResourcePressure, false),
        ] {
            let job = annotate_semantic_failure(
                daemon_semantic_job_json("failed", None, 1234, None, Some("failure".to_owned())),
                class,
            );
            assert_eq!(daemon_job_should_backoff(&job), should_backoff);
        }
    }

    #[test]
    fn daemon_retry_backoff_is_capped() {
        let mut backoff = DaemonRetryBackoff::default();
        let mut last = StdDuration::ZERO;
        for _ in 0..40 {
            let delay = backoff.record_failure();
            assert!(delay >= last);
            assert!(delay <= DaemonRetryBackoff::MAX_DELAY);
            last = delay;
        }
        assert_eq!(last, DaemonRetryBackoff::MAX_DELAY);
        assert!(!backoff.ready());
        assert!(backoff.retry_after_ms().is_some_and(|delay| delay > 0));
        let persisted = json!({
            "consecutive_failures": backoff.consecutive_failures,
            "retry_not_before_at_ms": backoff.retry_not_before_at_ms,
        });
        let mut restarted = DaemonRetryBackoff::default();
        restarted.restore(Some(&persisted));
        assert!(!restarted.ready(), "restart must preserve watcher backoff");
        backoff.reset();
        assert!(backoff.ready());
    }
}
