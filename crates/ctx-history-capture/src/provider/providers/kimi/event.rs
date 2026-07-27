use std::path::Path;

use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, EventRole, EventType, ProviderEventEnvelope};
use serde_json::{json, Value};

use crate::common::time::parse_rfc3339_utc;
use crate::provider::normalization::{
    native_event, provider_explicit_result_value_text, provider_role,
    provider_timestamp_seconds_to_datetime, provider_value_text, NativeEventDraft,
};
use crate::KIMI_CODE_CLI_SOURCE_FORMAT;

pub(crate) fn kimi_event(
    provider_session_id: &str,
    line_number: usize,
    value: &Value,
    occurred_at: DateTime<Utc>,
    path: &Path,
) -> ProviderEventEnvelope {
    let record_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let event_type = kimi_event_type(record_type, value);
    let role = kimi_event_role(record_type, value, event_type);
    let text = kimi_event_text(record_type, value, event_type);
    native_event(NativeEventDraft {
        provider: CaptureProvider::KimiCodeCli,
        source_format: KIMI_CODE_CLI_SOURCE_FORMAT,
        provider_session_id: provider_session_id.to_owned(),
        provider_event_index: (line_number - 1) as u64,
        provider_event_hash: Some(format!(
            "{}:{}",
            record_type,
            value
                .get("time")
                .and_then(Value::as_i64)
                .map(|time| time.to_string())
                .unwrap_or_else(|| line_number.to_string())
        )),
        cursor: format!("{}:line:{line_number}", path.display()),
        event_type,
        role: Some(role),
        occurred_at,
        text,
        body: value.clone(),
        metadata: json!({
            "source": "kimi_code_cli_wire_jsonl",
            "source_format": KIMI_CODE_CLI_SOURCE_FORMAT,
            "line": line_number,
            "record_type": record_type,
            "model": value.get("model").cloned(),
            "usage": value.get("usage").cloned(),
        }),
    })
}

pub(crate) fn kimi_event_type(record_type: &str, value: &Value) -> EventType {
    match record_type {
        "turn.prompt" | "turn.steer" | "context.append_message" => EventType::Message,
        "context.append_loop_event" => {
            let loop_type = value.pointer("/event/type").and_then(Value::as_str);
            match loop_type {
                Some(kind) if kind.contains("tool.call") || kind.contains("tool.start") => {
                    EventType::ToolCall
                }
                Some(kind) if kind.contains("tool.result") || kind.contains("tool.finish") => {
                    EventType::ToolOutput
                }
                Some(kind) if kind.contains("message") => EventType::Message,
                _ if value.pointer("/event/toolName").is_some()
                    || value.pointer("/event/tool_name").is_some() =>
                {
                    EventType::ToolCall
                }
                _ => EventType::Notice,
            }
        }
        "context.apply_compaction" | "full_compaction.complete" => EventType::Summary,
        _ => EventType::Notice,
    }
}

pub(crate) fn kimi_event_role(
    record_type: &str,
    value: &Value,
    event_type: EventType,
) -> EventRole {
    match record_type {
        "turn.prompt" | "turn.steer" => EventRole::User,
        "context.append_message" => provider_role(
            value
                .pointer("/message/role")
                .or_else(|| value.pointer("/message/source"))
                .and_then(Value::as_str),
        ),
        "context.append_loop_event"
            if matches!(event_type, EventType::ToolCall | EventType::ToolOutput) =>
        {
            EventRole::Tool
        }
        "context.append_loop_event" => provider_role(
            value
                .pointer("/event/role")
                .or_else(|| value.pointer("/event/source"))
                .and_then(Value::as_str),
        ),
        _ => EventRole::System,
    }
}

pub(crate) fn kimi_event_text(record_type: &str, value: &Value, event_type: EventType) -> String {
    match record_type {
        "turn.prompt" | "turn.steer" => value
            .get("input")
            .and_then(provider_value_text)
            .unwrap_or_default(),
        "context.append_message" => value
            .pointer("/message/content")
            .or_else(|| value.get("message"))
            .and_then(provider_value_text)
            .unwrap_or_default(),
        "context.append_loop_event" => value
            .pointer("/event/content")
            .or_else(|| value.pointer("/event/text"))
            .or_else(|| value.pointer("/event/output"))
            .or_else(|| value.pointer("/event/result"))
            .or_else(|| value.pointer("/event/message"))
            .and_then(provider_value_text)
            .or_else(|| {
                value
                    .pointer("/event/toolName")
                    .or_else(|| value.pointer("/event/tool_name"))
                    .and_then(Value::as_str)
                    .map(|tool| match event_type {
                        EventType::ToolOutput => format!("tool result: {tool}"),
                        EventType::ToolCall => format!("tool call: {tool}"),
                        _ => format!("tool: {tool}"),
                    })
            })
            .unwrap_or_default(),
        "usage.record" => String::new(),
        "tools.set_active_tools" => value
            .get("names")
            .and_then(Value::as_array)
            .map(|names| {
                let names = names
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("active tools: {names}")
            })
            .unwrap_or_else(|| "active tools updated".to_owned()),
        "permission.set_mode" => value
            .get("mode")
            .and_then(Value::as_str)
            .map(|mode| format!("permission mode: {mode}"))
            .unwrap_or_else(|| "permission mode updated".to_owned()),
        _ => String::new(),
    }
}

/// Returns explicit Kimi loop-event result content without the tool-name
/// fallback used for display text.
pub(crate) fn kimi_result_content(value: &Value) -> Option<String> {
    let record_type = value.get("type").and_then(Value::as_str)?;
    if kimi_event_type(record_type, value) != EventType::ToolOutput {
        return None;
    }
    value
        .pointer("/event/content")
        .or_else(|| value.pointer("/event/text"))
        .or_else(|| value.pointer("/event/output"))
        .or_else(|| value.pointer("/event/result"))
        .or_else(|| value.pointer("/event/message"))
        .and_then(provider_explicit_result_value_text)
}

pub(crate) fn kimi_record_timestamp(
    value: &Value,
    fallback: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    value
        .get("time")
        .and_then(Value::as_i64)
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        .or_else(|| {
            value
                .get("timestamp")
                .and_then(|timestamp| match timestamp {
                    Value::String(raw) => parse_rfc3339_utc(raw),
                    Value::Number(number) => number
                        .as_f64()
                        .and_then(provider_timestamp_seconds_to_datetime),
                    _ => None,
                })
        })
        .or_else(|| {
            value
                .get("created_at")
                .and_then(Value::as_i64)
                .and_then(DateTime::<Utc>::from_timestamp_millis)
        })
        .or(Some(fallback))
}

#[cfg(test)]
mod result_content_tests {
    use serde_json::json;

    use super::kimi_result_content;

    #[test]
    fn profile_omits_tool_name_fallback_and_keeps_explicit_result() {
        let result = json!({
            "type": "context.append_loop_event",
            "event": {
                "type": "tool.result",
                "toolName": "shell",
                "output": [
                    {"text": "first"},
                    {"result": "second"}
                ]
            }
        });
        assert_eq!(
            kimi_result_content(&result).as_deref(),
            Some("first\nsecond")
        );

        let label_only = json!({
            "type": "context.append_loop_event",
            "event": {"type": "tool.finish", "toolName": "shell"}
        });
        assert_eq!(kimi_result_content(&label_only), None);

        let call = json!({
            "type": "context.append_loop_event",
            "event": {"type": "tool.call", "content": "arguments"}
        });
        assert_eq!(kimi_result_content(&call), None);
    }
}
