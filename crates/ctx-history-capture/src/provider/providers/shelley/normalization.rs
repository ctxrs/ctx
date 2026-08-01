use chrono::{DateTime, NaiveDateTime, Utc};
use ctx_history_core::EventType;
use serde_json::Value;

use crate::common::time::parse_rfc3339_utc;
use crate::OutputOutcome;

use super::relationships::{shelley_event_type, shelley_message_body, ShelleyMessageRow};

pub(super) struct ShelleyOutputClassification {
    pub(super) outcome: OutputOutcome,
}

pub(super) fn shelley_output_classification(
    message: &ShelleyMessageRow,
) -> Option<ShelleyOutputClassification> {
    let body = shelley_message_body(message);
    if !matches!(
        shelley_event_type(message, &body),
        EventType::ToolOutput | EventType::CommandOutput
    ) {
        return None;
    }
    let mut evidence = ShelleyOutputEvidence::default();
    let mut remaining = 4_096;
    shelley_collect_result_evidence(&body, false, &mut remaining, &mut evidence);
    if message.entry_type == "tool" && !evidence.found_result {
        let mut remaining = 4_096;
        shelley_collect_output_fields(&body, &mut remaining, &mut evidence);
    }
    let outcome = if evidence.timeout {
        OutputOutcome::Timeout
    } else if evidence.failure {
        OutputOutcome::Failure
    } else if evidence.success {
        OutputOutcome::Success
    } else {
        OutputOutcome::Unknown
    };
    Some(ShelleyOutputClassification { outcome })
}

#[derive(Default)]
struct ShelleyOutputEvidence {
    found_result: bool,
    success: bool,
    failure: bool,
    timeout: bool,
}

fn shelley_collect_result_evidence(
    value: &Value,
    inside_result: bool,
    remaining: &mut usize,
    evidence: &mut ShelleyOutputEvidence,
) {
    if *remaining == 0 {
        return;
    }
    *remaining -= 1;
    match value {
        Value::Array(values) => {
            for value in values {
                shelley_collect_result_evidence(value, inside_result, remaining, evidence);
            }
        }
        Value::Object(values) => {
            let is_result = shelley_result_content_type(value).is_some_and(|kind| {
                matches!(
                    kind.as_str(),
                    "tool_result" | "web_search_tool_result" | "web_search_result"
                )
            });
            let inside_result = inside_result || is_result;
            evidence.found_result |= is_result;
            if inside_result {
                shelley_collect_output_fields(value, remaining, evidence);
            } else {
                for value in values.values() {
                    shelley_collect_result_evidence(value, false, remaining, evidence);
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn shelley_collect_output_fields(
    value: &Value,
    remaining: &mut usize,
    evidence: &mut ShelleyOutputEvidence,
) {
    if *remaining == 0 {
        return;
    }
    *remaining -= 1;
    match value {
        Value::Array(values) => {
            for value in values {
                shelley_collect_output_fields(value, remaining, evidence);
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                let normalized = key
                    .chars()
                    .filter(|ch| ch.is_ascii_alphanumeric())
                    .flat_map(char::to_lowercase)
                    .collect::<String>();
                match normalized.as_str() {
                    "exitcode" => {
                        if let Some(code) = value.as_i64() {
                            evidence.success |= code == 0;
                            evidence.failure |= code != 0;
                        }
                    }
                    "success" | "ok" => {
                        if let Some(success) = value.as_bool() {
                            evidence.success |= success;
                            evidence.failure |= !success;
                        }
                    }
                    "iserror" => evidence.failure |= value.as_bool().unwrap_or(false),
                    "timedout" | "timeout" => {
                        evidence.timeout |= value.as_bool().unwrap_or(false);
                    }
                    "status" | "state" | "outcome" => {
                        if let Some(status) = value.as_str() {
                            shelley_classify_status(status, evidence);
                        }
                    }
                    "error" if shelley_error_value_is_present(value) => evidence.failure = true,
                    _ => {}
                }
                shelley_collect_output_fields(value, remaining, evidence);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn shelley_result_content_type(value: &Value) -> Option<String> {
    let raw = value.get("Type")?;
    if let Some(text) = raw.as_str() {
        let normalized = text.trim().to_ascii_lowercase();
        return match normalized.as_str() {
            "contenttypetoolresult" => Some("tool_result".to_owned()),
            "contenttypewebsearchtoolresult" => Some("web_search_tool_result".to_owned()),
            "contenttypewebsearchresult" => Some("web_search_result".to_owned()),
            _ => Some(normalized),
        };
    }
    raw.as_i64().and_then(|kind| {
        match kind {
            6 => Some("tool_result"),
            8 => Some("web_search_tool_result"),
            9 => Some("web_search_result"),
            _ => None,
        }
        .map(str::to_owned)
    })
}

fn shelley_classify_status(status: &str, evidence: &mut ShelleyOutputEvidence) {
    match status.trim().to_ascii_lowercase().as_str() {
        "success" | "succeeded" | "complete" | "completed" | "ok" | "passed" => {
            evidence.success = true;
        }
        "timeout" | "timed_out" | "timedout" => evidence.timeout = true,
        "failed" | "failure" | "error" | "errored" | "cancelled" | "canceled" => {
            evidence.failure = true;
        }
        _ => {}
    }
}

fn shelley_error_value_is_present(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::String(value) => !value.trim().is_empty(),
        Value::Number(value) => value.as_i64().is_some_and(|value| value != 0),
        Value::Array(values) => !values.is_empty(),
        Value::Object(values) => !values.is_empty(),
    }
}

pub(super) fn shelley_timestamp(raw: Option<&str>, fallback: DateTime<Utc>) -> DateTime<Utc> {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return fallback;
    };
    parse_rfc3339_utc(raw)
        .or_else(|| {
            NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S%.f")
                .ok()
                .map(|naive| DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc))
        })
        .unwrap_or(fallback)
}
