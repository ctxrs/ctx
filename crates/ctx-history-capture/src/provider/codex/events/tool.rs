use std::borrow::Cow;

use chrono::{DateTime, Utc};
use ctx_history_core::{EventRole, EventType};
use serde_json::{json, Value};

#[cfg(test)]
use std::collections::BTreeMap;

use super::retention::{codex_content_text, codex_is_command_tool, codex_local_preview};
#[cfg(test)]
use super::retention::{codex_exit_code, codex_tool_name, codex_wall_time_ms};
use super::{codex_provider_event, CodexNativeEvent};
#[cfg(test)]
use crate::provider::normalization::provider_output_event_is_failure;
use crate::{OutputOutcome, OutputOutcomeMetadata};
use crate::{CODEX_SESSION_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS};

#[derive(Debug, Clone, Default)]
pub(crate) struct CodexToolCallContext {
    pub(crate) tool_name: String,
    pub(crate) command_preview: Option<String>,
    pub(crate) arguments_preview: Option<String>,
}

#[cfg(test)]
pub(crate) fn codex_tool_output_event(
    payload: &Value,
    line_number: usize,
    occurred_at: DateTime<Utc>,
    call_contexts: &BTreeMap<String, CodexToolCallContext>,
) -> Option<CodexNativeEvent> {
    let outcome = codex_tool_output_outcome(payload);
    let context = payload
        .get("call_id")
        .and_then(Value::as_str)
        .and_then(|call_id| call_contexts.get(call_id));
    let item_type = payload
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("tool_output");
    let fallback_tool_name = codex_tool_name(payload, item_type);
    codex_sparse_tool_output_event(
        item_type,
        &fallback_tool_name,
        payload.get("call_id").and_then(Value::as_str),
        line_number,
        occurred_at,
        context,
        &outcome,
        codex_direct_output_bytes(payload),
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn codex_sparse_tool_output_event(
    item_type: &str,
    fallback_tool_name: &str,
    call_id: Option<&str>,
    line_number: usize,
    occurred_at: DateTime<Utc>,
    context: Option<&CodexToolCallContext>,
    outcome: &OutputOutcomeMetadata,
    output_bytes: Option<usize>,
) -> Option<CodexNativeEvent> {
    if !matches!(
        outcome.outcome,
        OutputOutcome::Failure | OutputOutcome::Timeout
    ) {
        return None;
    }

    let tool_name = context
        .map(|context| context.tool_name.clone())
        .unwrap_or_else(|| fallback_tool_name.to_owned());
    let command_preview = context.and_then(|context| context.command_preview.clone());
    let event_type = if codex_is_command_tool(&tool_name) {
        EventType::CommandOutput
    } else {
        EventType::ToolOutput
    };
    let status = outcome
        .exit_code
        .map(|code| format!("exit_code={code}"))
        .unwrap_or_else(|| "exit_code=unknown".to_owned());
    let duration = outcome
        .duration_ms
        .map(|ms| format!(", duration_ms={ms}"))
        .unwrap_or_default();
    let timeout = if outcome.outcome == OutputOutcome::Timeout {
        ", timed_out=true"
    } else {
        ""
    };
    let retain_failure_outcome = outcome.outcome == OutputOutcome::Failure
        && !outcome.exit_code.is_some_and(|code| code != 0);
    let command = command_preview
        .as_deref()
        .map(|command| format!(" for `{command}`"))
        .unwrap_or_default();
    let text = format!("{tool_name} output{command}: {status}{duration}{timeout}");
    let (text, text_truncated) = codex_local_preview(&text, PROVIDER_MAX_PREVIEW_CHARS);
    let mut body = json!({
        "item_type": item_type,
        "tool": tool_name,
        "name": tool_name,
        "call_id": call_id,
        "command": command_preview,
        "arguments_preview": context.and_then(|context| context.arguments_preview.clone()),
        "output_bytes": output_bytes,
        "exit_code": outcome.exit_code,
        "duration_ms": outcome.duration_ms,
        "timed_out": outcome.outcome == OutputOutcome::Timeout,
        "text": text,
        "truncated": text_truncated,
    });
    if retain_failure_outcome {
        if let Some(object) = body.as_object_mut() {
            object.insert(
                "result_outcome".to_owned(),
                Value::String("failure".to_owned()),
            );
        }
    }
    Some(codex_provider_event(
        line_number,
        occurred_at,
        event_type,
        Some(EventRole::Tool),
        body,
        json!({
            "source": "codex_session",
            "source_format": CODEX_SESSION_SOURCE_FORMAT,
            "line": line_number,
            "item_type": item_type,
            "tool": tool_name,
            "output_retention": "bounded_failure_diagnostic",
        }),
    ))
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
fn codex_direct_output_bytes(payload: &Value) -> Option<usize> {
    payload
        .get("output")
        .or_else(|| payload.get("tools"))
        .or_else(|| payload.get("result"))
        .and_then(Value::as_str)
        .map(str::len)
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
        .map(codex_output_text)
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
