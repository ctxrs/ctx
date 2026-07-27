use std::{borrow::Cow, collections::BTreeMap};

use chrono::{DateTime, Utc};
#[cfg(test)]
use ctx_history_core::ProviderEventEnvelope;
use ctx_history_core::{ContentRef, EventRole, EventType};
use serde_json::{json, Value};

use super::retention::{
    codex_content_text, codex_exit_code, codex_is_command_tool, codex_local_preview,
    codex_timed_out, codex_tool_name, codex_wall_time_ms,
};
use super::{codex_provider_event, CodexProjectedEvent};
use crate::provider::normalization::{
    provider_output_event_is_failure, provider_result_identifier_evidence,
    provider_result_outcome_evidence,
};
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
) -> Option<ProviderEventEnvelope> {
    codex_tool_output_projection(payload, line_number, occurred_at, call_contexts)
        .map(|projected| projected.event)
}

pub(crate) fn codex_tool_output_projection(
    payload: &Value,
    line_number: usize,
    occurred_at: DateTime<Utc>,
    call_contexts: &BTreeMap<String, CodexToolCallContext>,
) -> Option<CodexProjectedEvent> {
    let item_type = payload
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("tool_output");
    let call_id = payload.get("call_id").and_then(Value::as_str);
    let context = call_id.and_then(|call_id| call_contexts.get(call_id));
    let tool_name = context
        .map(|context| context.tool_name.clone())
        .unwrap_or_else(|| codex_tool_name(payload, item_type));
    let output_text = codex_result_content(payload);
    let command_preview = context.and_then(|context| context.command_preview.clone());
    let output_text_ref = output_text.as_deref();
    let exit_code = output_text_ref
        .and_then(codex_exit_code)
        .or_else(|| codex_output_exit_code(payload));
    let duration_ms = output_text_ref.and_then(codex_wall_time_ms);
    let result_content_ref =
        output_text_ref.and_then(|output| ContentRef::from_bytes(output.as_bytes()));
    let output_bytes = result_content_ref
        .as_ref()
        .map(ContentRef::byte_len)
        .unwrap_or(0);
    let timed_out = codex_timed_out(payload).unwrap_or(false);
    let structured_failure = provider_output_event_is_failure(payload);
    let failed = timed_out || exit_code.is_some_and(|code| code != 0) || structured_failure;
    let event_type = if codex_is_command_tool(&tool_name) {
        EventType::CommandOutput
    } else {
        EventType::ToolOutput
    };
    let command = command_preview
        .as_deref()
        .map(|command| format!(" for `{command}`"))
        .unwrap_or_default();
    let status = exit_code
        .map(|code| format!("exit_code={code}"))
        .unwrap_or_else(|| "exit_code=unknown".to_owned());
    let duration = duration_ms
        .map(|ms| format!(", duration_ms={ms}"))
        .unwrap_or_default();
    let timeout = if timed_out { ", timed_out=true" } else { "" };
    let text = format!(
        "{tool_name} output{command}: {status}{duration}, output_bytes={output_bytes}{timeout}"
    );
    let (text, text_truncated) = codex_local_preview(&text, PROVIDER_MAX_PREVIEW_CHARS);
    let result_body = json!({
        "call_id": call_id,
        "exit_code": exit_code,
        "timed_out": timed_out,
        "success": !failed,
    });
    let result_text = output_text_ref.unwrap_or_default();
    let result_evidence =
        provider_result_identifier_evidence(event_type, result_text, &result_body);
    let result_outcome = provider_result_outcome_evidence(event_type, &result_body);
    let event = codex_provider_event(
        line_number,
        occurred_at,
        event_type,
        Some(EventRole::Tool),
        json!({
            "item_type": item_type,
            "tool": tool_name,
            "name": tool_name,
            "call_id": call_id,
            "command": command_preview,
            "arguments_preview": context.and_then(|context| context.arguments_preview.clone()),
            "output_bytes": output_bytes,
            "exit_code": exit_code,
            "duration_ms": duration_ms,
            "timed_out": timed_out,
            "text": text,
            "truncated": text_truncated,
            "result_evidence": result_evidence,
            "result_outcome": result_outcome,
            "result_content_ref": result_content_ref.as_ref(),
        }),
        json!({
            "source": "codex_session",
            "source_format": CODEX_SESSION_SOURCE_FORMAT,
            "line": line_number,
            "item_type": item_type,
            "tool": tool_name,
        }),
    );

    Some(CodexProjectedEvent {
        event,
        result_content_ref,
    })
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
        _ => None,
    }
}
