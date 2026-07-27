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

use super::{
    health_search::semantic_model_acquisition_integrity_error,
    model_contract::SemanticModelLoadDeferred,
    vector_store_schema::{semantic_vector_failure_kind, SemanticVectorFailureKind},
};
