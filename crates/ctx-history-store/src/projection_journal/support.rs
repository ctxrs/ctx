use std::time::{SystemTime, UNIX_EPOCH};

use crate::{Result, StoreError};

pub(super) fn nonnegative_u64(value: i64, field: &str) -> Result<u64> {
    u64::try_from(value)
        .map_err(|_| StoreError::InvalidProjectionJournalData(format!("negative {field}: {value}")))
}

pub(super) fn to_i64(value: u64, field: &str) -> Result<i64> {
    i64::try_from(value).map_err(|_| {
        StoreError::InvalidProjectionJournalData(format!("{field} exceeds SQLite INTEGER"))
    })
}

pub(super) fn current_time_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(0)
}
