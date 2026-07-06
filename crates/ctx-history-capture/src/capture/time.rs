#[allow(unused_imports)]
use super::*;

pub(crate) fn system_time_ms(value: SystemTime) -> i64 {
    value
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

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

pub(crate) fn task_json_started_at(
    metadata: Option<&Value>,
    history_item: Option<&Value>,
    index_item: Option<&Value>,
    root_history_item: Option<&Value>,
    fallback: DateTime<Utc>,
) -> DateTime<Utc> {
    metadata
        .and_then(|value| {
            task_json_time_field(value, &["createdAt", "created_at", "ts", "timestamp"])
        })
        .or_else(|| {
            history_item.and_then(|value| {
                task_json_time_field(value, &["createdAt", "created_at", "ts", "timestamp"])
            })
        })
        .or_else(|| {
            index_item.and_then(|value| {
                task_json_time_field(value, &["createdAt", "created_at", "ts", "timestamp"])
            })
        })
        .or_else(|| {
            root_history_item.and_then(|value| {
                task_json_time_field(value, &["createdAt", "created_at", "ts", "timestamp"])
            })
        })
        .unwrap_or(fallback)
}

pub(crate) fn task_json_ended_at(
    metadata: Option<&Value>,
    history_item: Option<&Value>,
    index_item: Option<&Value>,
) -> Option<DateTime<Utc>> {
    metadata
        .and_then(|value| {
            task_json_time_field(
                value,
                &["lastModified", "updatedAt", "completedAt", "last_modified"],
            )
        })
        .or_else(|| {
            history_item.and_then(|value| {
                task_json_time_field(
                    value,
                    &["lastModified", "updatedAt", "completedAt", "last_modified"],
                )
            })
        })
        .or_else(|| {
            index_item.and_then(|value| {
                task_json_time_field(
                    value,
                    &["lastModified", "updatedAt", "completedAt", "last_modified"],
                )
            })
        })
}

pub(crate) fn task_json_time_field(value: &Value, fields: &[&str]) -> Option<DateTime<Utc>> {
    for field in fields {
        let Some(value) = value.get(*field) else {
            continue;
        };
        if let Some(text) = value.as_str() {
            if let Some(parsed) = parse_rfc3339_utc(text) {
                return Some(parsed);
            }
            if let Ok(number) = text.parse::<i64>() {
                if let Some(parsed) = task_json_timestamp_number(number) {
                    return Some(parsed);
                }
            }
        }
        if let Some(number) = value.as_i64().and_then(task_json_timestamp_number) {
            return Some(number);
        }
    }
    None
}

pub(crate) fn task_json_timestamp_number(value: i64) -> Option<DateTime<Utc>> {
    if value > 10_000_000_000 {
        DateTime::<Utc>::from_timestamp_millis(value)
    } else {
        DateTime::<Utc>::from_timestamp(value, 0)
    }
}

pub(crate) fn provider_timestamp_seconds_to_datetime(value: f64) -> Option<DateTime<Utc>> {
    if !value.is_finite() {
        return None;
    }
    let millis = if value.abs() > 1_000_000_000_000.0 {
        value.round()
    } else {
        (value * 1000.0).round()
    };
    if millis < i64::MIN as f64 || millis > i64::MAX as f64 {
        return None;
    }
    DateTime::<Utc>::from_timestamp_millis(millis as i64)
}

pub(crate) fn provider_timestamp_seconds(
    value: Option<f64>,
    fallback: DateTime<Utc>,
) -> DateTime<Utc> {
    value
        .and_then(provider_timestamp_seconds_to_datetime)
        .unwrap_or(fallback)
}

pub(crate) fn provider_required_timestamp_seconds(
    value: f64,
    field: &'static str,
) -> Result<DateTime<Utc>> {
    provider_timestamp_seconds_to_datetime(value).ok_or_else(|| {
        CaptureError::InvalidPayload(format!(
            "{field} is outside representable timestamp range: {value}"
        ))
    })
}

pub(crate) fn provider_timestamp_millis(
    value: Option<i64>,
    fallback: DateTime<Utc>,
) -> DateTime<Utc> {
    value
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        .unwrap_or(fallback)
}

pub(crate) fn provider_required_timestamp_millis(
    value: i64,
    field: &'static str,
) -> Result<DateTime<Utc>> {
    DateTime::<Utc>::from_timestamp_millis(value).ok_or_else(|| {
        CaptureError::InvalidPayload(format!(
            "{field} is outside representable timestamp range: {value}"
        ))
    })
}

pub(crate) fn provider_timestamp_value(
    value: Option<&Value>,
    fallback: DateTime<Utc>,
) -> DateTime<Utc> {
    match value {
        Some(Value::String(raw)) => parse_rfc3339_utc(raw)
            .or_else(|| {
                raw.parse::<f64>()
                    .ok()
                    .map(|ts| provider_timestamp_seconds(Some(ts), fallback))
            })
            .unwrap_or(fallback),
        Some(Value::Number(number)) => number
            .as_f64()
            .map(|ts| provider_timestamp_seconds(Some(ts), fallback))
            .unwrap_or(fallback),
        _ => fallback,
    }
}

#[derive(Debug, Clone)]
pub(crate) struct DeepAgentsThread {
    pub(crate) thread_id: String,
    pub(crate) agent_name: Option<String>,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) updated_at: DateTime<Utc>,
    pub(crate) latest_checkpoint_id: Option<String>,
    pub(crate) git_branch: Option<String>,
    pub(crate) cwd: Option<String>,
    pub(crate) checkpoint_times: BTreeMap<String, DateTime<Utc>>,
}

pub(crate) fn continue_history_item_timestamp(
    item: &Value,
    fallback: DateTime<Utc>,
) -> DateTime<Utc> {
    item.get("timestamp")
        .or_else(|| item.get("createdAt"))
        .or_else(|| item.pointer("/message/timestamp"))
        .map(|value| provider_timestamp_value(Some(value), fallback))
        .unwrap_or(fallback)
}

pub(crate) fn provider_timestamp_from_fields(
    value: &Value,
    fields: &[&str],
) -> Option<DateTime<Utc>> {
    fields.iter().find_map(|field| {
        let raw = value.get(*field)?;
        match raw {
            Value::String(text) => parse_rfc3339_utc(text).or_else(|| {
                text.parse::<f64>()
                    .ok()
                    .and_then(provider_timestamp_seconds_to_datetime)
            }),
            Value::Number(number) => number
                .as_f64()
                .and_then(provider_timestamp_seconds_to_datetime),
            _ => None,
        }
    })
}

pub(crate) fn datetime_field(value: &Value, field: &str) -> Result<Option<DateTime<Utc>>> {
    match value.get(field) {
        Some(Value::String(raw)) => {
            Ok(Some(DateTime::parse_from_rfc3339(raw)?.with_timezone(&Utc)))
        }
        Some(Value::Null) | None => Ok(None),
        Some(_) => Err(CaptureError::InvalidPayload(format!(
            "{field} must be an RFC3339 timestamp string"
        ))),
    }
}

pub(crate) fn timestamps(at: DateTime<Utc>) -> EntityTimestamps {
    EntityTimestamps {
        created_at: at,
        updated_at: at,
    }
}
