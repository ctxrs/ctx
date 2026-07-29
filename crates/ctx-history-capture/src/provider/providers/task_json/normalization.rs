use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::common::time::parse_rfc3339_utc;

pub(crate) fn task_json_string_field(value: &Value, fields: &[&str]) -> Option<String> {
    fields
        .iter()
        .find_map(|field| value.get(*field).and_then(Value::as_str))
        .filter(|text| !text.trim().is_empty())
        .map(str::to_owned)
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

fn task_json_timestamp_number(value: i64) -> Option<DateTime<Utc>> {
    if value > 10_000_000_000 {
        DateTime::<Utc>::from_timestamp_millis(value)
    } else {
        DateTime::<Utc>::from_timestamp(value, 0)
    }
}
