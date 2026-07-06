use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::{CaptureError, Result};

pub(crate) fn parse_rfc3339_utc(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|time| time.with_timezone(&Utc))
}

pub(crate) fn parse_optional_rfc3339_field(
    value: &Value,
    field: &'static str,
) -> Result<Option<DateTime<Utc>>> {
    let Some(raw_value) = value.get(field) else {
        return Ok(None);
    };
    let raw = raw_value.as_str().ok_or_else(|| {
        CaptureError::InvalidPayload(format!("{field} must be an RFC3339 string"))
    })?;
    parse_rfc3339_utc(raw)
        .ok_or_else(|| {
            CaptureError::InvalidPayload(format!("{field} is not a valid RFC3339 timestamp"))
        })
        .map(Some)
}

pub(crate) fn system_time_ms(value: SystemTime) -> i64 {
    value
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}
