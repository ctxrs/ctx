use chrono::{DateTime, Utc};
use ctx_history_core::EventType;
use serde_json::{json, Value};

use crate::provider::normalization::{
    provider_normalized_result_value, provider_required_timestamp_millis, provider_value_text,
};
use crate::{CaptureError, Result};

use super::schema::OpenCodeSqliteDialect;

pub(super) const OPENCODE_MESSAGE_PART_DEFAULT_ROLE: &str = "assistant";

pub(super) fn opencode_entry_type_from_data(fallback: &str, data: &str) -> String {
    if !fallback.trim().is_empty() && fallback != "message" {
        return fallback.to_owned();
    }
    serde_json::from_str::<Value>(data)
        .ok()
        .and_then(|value| opencode_message_type_from_data(&value))
        .unwrap_or_else(|| fallback.to_owned())
}

pub(super) fn opencode_part_type(column_type: Option<&str>, data: &Value) -> String {
    column_type
        .filter(|value| !value.trim().is_empty())
        .or_else(|| data.get("type").and_then(Value::as_str))
        .unwrap_or("part")
        .to_owned()
}

pub(super) fn opencode_message_part_role(data: &Value) -> String {
    data.get("role")
        .or_else(|| data.pointer("/message/role"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|role| matches!(*role, "assistant" | "user" | "system"))
        .unwrap_or(OPENCODE_MESSAGE_PART_DEFAULT_ROLE)
        .to_owned()
}

pub(super) fn opencode_text_part_text(part_type: &str, data: &Value) -> Option<String> {
    (part_type == "text")
        .then(|| data.get("text").and_then(Value::as_str))
        .flatten()
        .map(str::to_owned)
}

pub(super) fn opencode_tool_part_event_data(
    message_id: &str,
    part_id: &str,
    part_type: &str,
    time_created: i64,
    data: &Value,
) -> Option<Value> {
    if !matches!(part_type, "tool" | "tool_result" | "result") {
        return None;
    }
    let tool_name = opencode_tool_part_name(data);
    let status = opencode_tool_part_status(data);
    let exit_code = opencode_tool_part_exit_code(data);
    let is_error = opencode_tool_part_is_error(data, status.as_deref(), exit_code);
    let call_id = [
        "/call_id",
        "/callId",
        "/callID",
        "/tool_call_id",
        "/state/call_id",
        "/state/callId",
    ]
    .iter()
    .find_map(|pointer| data.pointer(pointer))
    .and_then(Value::as_str);
    Some(json!({
        "role": "tool",
        "time": { "created": time_created },
        "source_table": "message+part",
        "message_id": message_id,
        "part_id": part_id,
        "part_type": part_type,
        "tool_name": tool_name,
        "call_id": call_id,
        "status": status,
        "exit_code": exit_code,
        "is_error": is_error,
        "output_retention": "metadata_only",
    }))
}

/// Returns the complete normalized result body shared by OpenCode, Kilo, and
/// MiMo Code SQLite profiles.
///
/// The supported generations use either a direct result field or the modern
/// `state` envelope. Precedence is explicit and the function never searches
/// arbitrary descendants. The caller owns any byte bound.
pub(crate) fn opencode_normalized_result_content(entry_type: &str, data: &Value) -> Option<String> {
    let candidates: &[&str] = match entry_type {
        "tool" | "tool_result" | "result" => &[
            "/state/output",
            "/state/result",
            "/state/content",
            "/output",
            "/result",
            "/content",
            "/text",
        ],
        "shell" => &[
            "/output",
            "/state/output",
            "/stdout",
            "/stderr",
            "/result",
            "/content",
            "/text",
        ],
        _ => return None,
    };
    candidates
        .iter()
        .find_map(|pointer| data.pointer(pointer))
        .map(provider_normalized_result_value)
}

pub(super) fn opencode_tool_part_name(data: &Value) -> String {
    data.get("tool")
        .or_else(|| data.get("tool_name"))
        .or_else(|| data.get("name"))
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("tool")
        .to_owned()
}

pub(super) fn opencode_tool_part_status(data: &Value) -> Option<String> {
    data.pointer("/state/status")
        .or_else(|| data.get("status"))
        .and_then(Value::as_str)
        .filter(|status| !status.trim().is_empty())
        .map(str::to_owned)
}

pub(super) fn opencode_tool_part_exit_code(data: &Value) -> Option<i64> {
    data.pointer("/state/metadata/exit")
        .or_else(|| data.pointer("/state/metadata/exit_code"))
        .or_else(|| data.pointer("/state/metadata/exitCode"))
        .or_else(|| data.get("exit_code"))
        .or_else(|| data.get("exitCode"))
        .and_then(Value::as_i64)
}

pub(super) fn opencode_tool_part_is_error(
    data: &Value,
    status: Option<&str>,
    exit_code: Option<i64>,
) -> bool {
    data.get("is_error")
        .or_else(|| data.get("isError"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || exit_code.is_some_and(|code| code != 0)
        || status.is_some_and(|status| {
            matches!(
                status.trim().to_ascii_lowercase().as_str(),
                "failed" | "failure" | "error" | "errored" | "timeout" | "timed_out" | "timedout"
            )
        })
}

pub(super) fn opencode_message_type_from_data(data: &Value) -> Option<String> {
    data.get("role")
        .or_else(|| data.get("type"))
        .or_else(|| data.pointer("/message/role"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

pub(super) fn opencode_event_type(entry_type: &str, data: &Value) -> EventType {
    match entry_type {
        "assistant" if opencode_content_has_tool(data) => EventType::ToolCall,
        "assistant" | "user" | "system" => EventType::Message,
        "tool" | "tool_result" => EventType::ToolOutput,
        "shell" => EventType::CommandOutput,
        _ => EventType::Notice,
    }
}

pub(super) fn opencode_event_text(
    entry_type: &str,
    data: &Value,
    event_type: EventType,
    dialect: &OpenCodeSqliteDialect,
) -> String {
    if let Some(text) = data.get("text").and_then(Value::as_str) {
        return text.to_owned();
    }
    if entry_type == "shell" {
        let command = data
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("shell");
        let output = data.get("output").and_then(Value::as_str).unwrap_or("");
        return format!("{command}\n{output}");
    }
    if matches!(entry_type, "tool" | "tool_result") {
        let tool_name = data
            .get("tool_name")
            .and_then(Value::as_str)
            .unwrap_or("tool");
        let status = data
            .get("status")
            .and_then(Value::as_str)
            .map(|status| format!("\nstatus: {status}"))
            .unwrap_or_default();
        return format!("tool result: {tool_name}{status}");
    }
    if let Some(content) = data.get("content") {
        if let Some(text) = provider_value_text(content) {
            return text;
        }
    }
    if event_type == EventType::Notice {
        format!("{} event: {entry_type}", dialect.display_name)
    } else {
        String::new()
    }
}

pub(super) fn opencode_content_has_tool(data: &Value) -> bool {
    data.get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks.iter().any(|block| {
                matches!(
                    block.get("type").and_then(Value::as_str),
                    Some("tool" | "tool_use" | "toolCall")
                )
            })
        })
        .unwrap_or(false)
}

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
