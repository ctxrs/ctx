use std::borrow::Cow;

use serde_json::Value;

use super::retention::codex_content_text;
#[cfg(test)]
use super::retention::{codex_exit_code, codex_wall_time_ms};
#[cfg(test)]
use crate::provider::normalization::provider_output_event_is_failure;
#[cfg(test)]
use crate::{OutputOutcome, OutputOutcomeMetadata};

#[derive(Debug, Clone, Default)]
pub(crate) struct CodexToolCallContext {
    pub(crate) tool_name: String,
    pub(crate) command_preview: Option<String>,
    pub(crate) arguments_preview: Option<String>,
    pub(crate) exact_command: Option<String>,
    pub(crate) session_cwd: Option<String>,
    pub(crate) declared_workdir: Option<String>,
    pub(crate) continuation_cell_id: Option<String>,
    pub(crate) origin_call_id: Option<String>,
    pub(crate) origin_event_sequence: Option<u64>,
    pub(crate) continuation_call_id_sha256: Vec<[u8; 32]>,
    pub(crate) continuation_capacity_exceeded: bool,
    pub(crate) correlation_ambiguous: bool,
}

#[cfg(test)]
pub(crate) fn codex_tool_output_outcome(payload: &Value) -> OutputOutcomeMetadata {
    let timed_out = codex_output_contains_timeout(payload);
    let exit_code = codex_output_exit_code(payload);
    let structured_failure = provider_output_event_is_failure(payload);
    let outcome = if timed_out {
        OutputOutcome::Timeout
    } else if exit_code.is_some_and(|code| code != 0) || structured_failure {
        OutputOutcome::Failure
    } else if exit_code == Some(0) || codex_output_indicates_success(payload) {
        OutputOutcome::Success
    } else {
        OutputOutcome::Unknown
    };
    OutputOutcomeMetadata {
        outcome,
        exit_code,
        duration_ms: codex_output_duration_ms(payload),
    }
}

#[cfg(test)]
fn codex_output_contains_timeout(value: &Value) -> bool {
    match value {
        Value::Object(object) => {
            object
                .get("timed_out")
                .or_else(|| object.get("timedOut"))
                .or_else(|| object.get("timeout"))
                .and_then(Value::as_bool)
                .unwrap_or(false)
                || object.values().any(codex_output_contains_timeout)
        }
        Value::Array(items) => items.iter().any(codex_output_contains_timeout),
        Value::String(text) => {
            text.contains("timed out")
                || text.contains("Timed out")
                || text.contains("TIMED OUT")
                || text.contains("timed_out=true")
        }
        _ => false,
    }
}

#[cfg(test)]
fn codex_output_duration_ms(value: &Value) -> Option<u64> {
    match value {
        Value::Object(object) => {
            for key in ["duration_ms", "durationMs"] {
                if let Some(duration) = object.get(key).and_then(Value::as_u64) {
                    return Some(duration);
                }
            }
            object.values().find_map(codex_output_duration_ms)
        }
        Value::Array(items) => items.iter().find_map(codex_output_duration_ms),
        Value::String(text) => codex_wall_time_ms(text).and_then(|value| u64::try_from(value).ok()),
        _ => None,
    }
}

#[cfg(test)]
fn codex_output_indicates_success(value: &Value) -> bool {
    match value {
        Value::Object(object) => {
            object
                .get("success")
                .or_else(|| object.get("ok"))
                .and_then(Value::as_bool)
                .unwrap_or(false)
                || ["status", "state", "outcome"].iter().any(|key| {
                    object
                        .get(*key)
                        .and_then(Value::as_str)
                        .is_some_and(|status| {
                            matches!(
                                status.trim().to_ascii_lowercase().as_str(),
                                "success"
                                    | "succeeded"
                                    | "complete"
                                    | "completed"
                                    | "ok"
                                    | "passed"
                            )
                        })
                })
                || object.values().any(codex_output_indicates_success)
        }
        Value::Array(items) => items.iter().any(codex_output_indicates_success),
        Value::String(text) => text.starts_with("Script completed\n") || text == "Script completed",
        _ => false,
    }
}

pub(crate) fn codex_result_content(payload: &Value) -> Option<Cow<'_, str>> {
    codex_result_value(payload).map(codex_output_text)
}

pub(crate) fn codex_result_value(payload: &Value) -> Option<&Value> {
    let item_type = payload.get("type").and_then(Value::as_str)?;
    if !matches!(
        item_type,
        "function_call_output"
            | "custom_tool_call_output"
            | "tool_search_output"
            | "tool_result"
            | "tool_output"
    ) {
        return None;
    }
    payload
        .get("output")
        .or_else(|| payload.get("tools"))
        .or_else(|| payload.get("result"))
}

pub(crate) fn codex_output_text(value: &Value) -> Cow<'_, str> {
    match value {
        Value::String(text) => Cow::Borrowed(text),
        Value::Null => Cow::Borrowed(""),
        other => {
            Cow::Owned(codex_content_text(other).unwrap_or_else(|| {
                serde_json::to_string(other).unwrap_or_else(|_| other.to_string())
            }))
        }
    }
}

#[cfg(test)]
fn codex_output_exit_code(value: &Value) -> Option<i32> {
    match value {
        Value::Object(object) => {
            for key in ["exit_code", "exitCode"] {
                if let Some(code) = object
                    .get(key)
                    .and_then(Value::as_i64)
                    .and_then(|code| i32::try_from(code).ok())
                {
                    return Some(code);
                }
            }
            object.values().find_map(codex_output_exit_code)
        }
        Value::Array(items) => items.iter().find_map(codex_output_exit_code),
        Value::String(text) => codex_exit_code(text),
        _ => None,
    }
}
