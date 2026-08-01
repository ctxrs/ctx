use chrono::{DateTime, Utc};
use ctx_history_core::{EventRole, EventType};
use serde_json::Value;

use crate::common::time::parse_rfc3339_utc;
use crate::provider::normalization::{
    provider_explicit_result_value_text, provider_role, provider_timestamp_seconds_to_datetime,
    provider_value_text,
};

pub(super) fn kimi_legacy_provider_event_hash(
    record_type: &str,
    value: &Value,
    line_number: usize,
) -> String {
    format!(
        "{}:{}",
        record_type,
        value
            .get("time")
            .and_then(Value::as_i64)
            .map(|time| time.to_string())
            .unwrap_or_else(|| line_number.to_string())
    )
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

/// Returns explicit Kimi loop-event output content without the tool-name
/// fallback used for display text.
pub(super) fn kimi_output_content(value: &Value) -> Option<String> {
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
mod output_content_tests {
    use serde_json::json;

    use super::kimi_output_content;

    #[test]
    fn extraction_omits_tool_name_fallback_and_keeps_explicit_output() {
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
            kimi_output_content(&result).as_deref(),
            Some("first\nsecond")
        );

        let label_only = json!({
            "type": "context.append_loop_event",
            "event": {"type": "tool.finish", "toolName": "shell"}
        });
        assert_eq!(kimi_output_content(&label_only), None);

        let call = json!({
            "type": "context.append_loop_event",
            "event": {"type": "tool.call", "content": "arguments"}
        });
        assert_eq!(kimi_output_content(&call), None);
    }
}
