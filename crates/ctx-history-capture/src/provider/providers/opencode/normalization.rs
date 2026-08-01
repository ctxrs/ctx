use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::provider::normalization::provider_required_timestamp_millis;
use crate::{CaptureError, Result};

use super::schema::OpenCodeSqliteDialect;

pub(super) fn opencode_event_time(
    data: &Value,
    dialect: &OpenCodeSqliteDialect,
) -> Result<Option<DateTime<Utc>>> {
    let Some(value) = data.pointer("/time/created") else {
        return Ok(None);
    };
    let millis = value.as_i64().ok_or_else(|| {
        CaptureError::InvalidPayload(format!(
            "{} event time.created must be integer millis",
            dialect.display_name
        ))
    })?;
    provider_required_timestamp_millis(millis, dialect.event_time_created_field).map(Some)
}
