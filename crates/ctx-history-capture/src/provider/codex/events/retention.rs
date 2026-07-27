use ctx_history_core::EventRole;
use serde_json::{json, Value};

use crate::provider::file_touches::visit_all_file_touch_drafts;
use crate::provider::normalization::capped_text;
use crate::provider::tool_input;
use crate::PROVIDER_MAX_PREVIEW_CHARS;

pub(crate) fn codex_tool_name(payload: &Value, item_type: &str) -> String {
    payload
        .get("name")
        .or_else(|| payload.get("tool"))
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(item_type)
        .to_owned()
}
pub(crate) use tool_input::is_command_tool as codex_is_command_tool;
pub(crate) fn codex_command_preview(
    tool_name: &str,
    argument_value: Option<&Value>,
) -> Option<String> {
    if !codex_is_command_tool(tool_name) {
        return None;
    }
    let value = argument_value?;
    let command = tool_input::command(value)?;
    Some(codex_local_preview(&command, PROVIDER_MAX_PREVIEW_CHARS).0)
}
pub(crate) fn codex_value_preview(value: &Value, max_chars: usize) -> (String, bool) {
    let rendered = match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        _ => serde_json::to_string(value).unwrap_or_else(|_| value.to_string()),
    };
    codex_local_preview(&rendered, max_chars)
}
pub(crate) fn codex_tool_arguments_preview(value: &Value) -> (String, bool, bool) {
    let parsed = codex_parse_embedded_json(value);
    let parsed = parsed.as_ref().unwrap_or(value);
    let mut retained_paths = Vec::with_capacity(12);
    let mut file_touch_count = 0_usize;
    let visit_result: std::result::Result<(), std::convert::Infallible> =
        visit_all_file_touch_drafts(parsed, |touch| {
            file_touch_count = file_touch_count.saturating_add(1);
            if retained_paths.len() < 12 {
                retained_paths.push(match touch.change_kind {
                    Some(kind) => format!("{}:{}", kind.as_str(), touch.path),
                    None => touch.path,
                });
            }
            Ok(())
        });
    match visit_result {
        Ok(()) => {}
        Err(never) => match never {},
    }
    if file_touch_count != 0 {
        return codex_file_touch_arguments_preview(retained_paths, file_touch_count);
    }
    let (retained, fields_omitted) = codex_tool_argument_value_with_omissions(parsed, None);
    let (preview, truncated) = codex_value_preview(&retained, PROVIDER_MAX_PREVIEW_CHARS);
    (preview, truncated, !fields_omitted)
}
fn codex_file_touch_arguments_preview(
    retained_paths: Vec<String>,
    file_touch_count: usize,
) -> (String, bool, bool) {
    let paths = retained_paths.join(", ");
    let omitted = file_touch_count.saturating_sub(12);
    let suffix = if omitted == 0 {
        String::new()
    } else {
        format!(", +{omitted} more")
    };
    (format!("file touches: {paths}{suffix}"), omitted > 0, false)
}
pub(crate) fn codex_tool_argument_value_with_omissions(
    value: &Value,
    key: Option<&str>,
) -> (Value, bool) {
    if key.is_some_and(|key| codex_tool_argument_key_should_omit(key, value)) {
        return (codex_omitted_argument_value(value), true);
    }
    match value {
        Value::Array(items) => {
            let mut fields_omitted = false;
            let items = items
                .iter()
                .map(|item| {
                    let (item, item_omitted) = codex_tool_argument_value_with_omissions(item, key);
                    fields_omitted |= item_omitted;
                    item
                })
                .collect();
            (Value::Array(items), fields_omitted)
        }
        Value::Object(object) => {
            let mut fields_omitted = false;
            let object = object
                .iter()
                .map(|(key, value)| {
                    let (value, value_omitted) =
                        codex_tool_argument_value_with_omissions(value, Some(key));
                    fields_omitted |= value_omitted;
                    (key.clone(), value)
                })
                .collect();
            (Value::Object(object), fields_omitted)
        }
        _ => (value.clone(), false),
    }
}
pub(crate) fn codex_tool_argument_key_should_omit(key: &str, value: &Value) -> bool {
    let key = codex_normalized_key(key);
    matches!(
        key.as_str(),
        "content"
            | "text"
            | "body"
            | "diff"
            | "patch"
            | "oldstring"
            | "newstring"
            | "oldcontent"
            | "newcontent"
            | "beforecontent"
            | "aftercontent"
            | "beforetext"
            | "aftertext"
            | "replacement"
            | "oldstr"
            | "newstr"
            | "inputtext"
            | "outputtext"
    ) || (matches!(key.as_str(), "input" | "arguments" | "args" | "params")
        && codex_value_contains_patch_or_diff(value))
}
pub(crate) fn codex_omitted_argument_value(value: &Value) -> Value {
    json!({
        "field_retention": {
            "mode": "omitted",
            "original_bytes": codex_value_approx_bytes(value),
            "contained_patch_or_diff": codex_value_contains_patch_or_diff(value),
        },
    })
}
pub(crate) fn codex_normalized_key(key: &str) -> String {
    key.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
pub(crate) fn codex_value_approx_bytes(value: &Value) -> usize {
    match value {
        Value::String(text) => text.len(),
        _ => serde_json::to_string(value)
            .map(|text| text.len())
            .unwrap_or_default(),
    }
}
pub(crate) fn codex_value_contains_patch_or_diff(value: &Value) -> bool {
    match value {
        Value::String(text) => codex_text_contains_patch_or_diff(text),
        Value::Array(items) => items.iter().any(codex_value_contains_patch_or_diff),
        Value::Object(object) => object.values().any(codex_value_contains_patch_or_diff),
        _ => false,
    }
}
pub(crate) fn codex_text_contains_patch_or_diff(text: &str) -> bool {
    text.contains("*** Begin Patch")
        || text.contains("diff --git ")
        || text.starts_with("@@")
        || text.starts_with("+++ ")
        || text.starts_with("--- ")
        || text.contains("\n@@")
        || text.contains("\n+++ ")
        || text.contains("\n--- ")
}
pub(crate) fn codex_local_preview(value: &str, max_chars: usize) -> (String, bool) {
    capped_text(value, max_chars)
}
pub(crate) fn codex_parse_embedded_json(value: &Value) -> Option<Value> {
    match value {
        Value::String(text) => serde_json::from_str::<Value>(text).ok(),
        Value::Object(_) | Value::Array(_) => Some(value.clone()),
        _ => None,
    }
}
pub(crate) fn codex_timed_out(payload: &Value) -> Option<bool> {
    payload
        .get("timed_out")
        .and_then(Value::as_bool)
        .or_else(|| {
            payload
                .get("output")
                .and_then(codex_parse_embedded_json)
                .and_then(|value| {
                    value
                        .get("timed_out")
                        .and_then(Value::as_bool)
                        .or_else(|| value.pointer("/status/timed_out").and_then(Value::as_bool))
                })
        })
}
pub(crate) fn codex_exit_code(text: &str) -> Option<i32> {
    if text.starts_with("Script completed\n") || text == "Script completed" {
        return Some(0);
    }
    if text.starts_with("Script failed\n") || text == "Script failed" {
        return Some(1);
    }
    let marker = "Process exited with code ";
    let index = text.find(marker)? + marker.len();
    let tail = &text[index..];
    let digits = tail
        .chars()
        .take_while(|ch| ch.is_ascii_digit() || *ch == '-')
        .collect::<String>();
    digits.parse().ok()
}
pub(crate) fn codex_wall_time_ms(text: &str) -> Option<i64> {
    let (index, marker_len) = ["Wall time: ", "Wall time "]
        .iter()
        .find_map(|marker| text.find(marker).map(|index| (index, marker.len())))?;
    let index = index + marker_len;
    let tail = &text[index..];
    let seconds_text = tail
        .chars()
        .take_while(|ch| ch.is_ascii_digit() || *ch == '.')
        .collect::<String>();
    let seconds = seconds_text.parse::<f64>().ok()?;
    Some((seconds * 1000.0).round() as i64)
}
pub(crate) fn codex_event_role(role: &str) -> EventRole {
    match role {
        "user" => EventRole::User,
        "assistant" => EventRole::Assistant,
        "tool" => EventRole::Tool,
        "system" | "developer" => EventRole::System,
        _ => EventRole::Unknown,
    }
}
pub(crate) fn codex_content_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => {
            let mut parts = Vec::new();
            for block in blocks {
                if let Some(text) = block
                    .get("text")
                    .or_else(|| block.get("input_text"))
                    .or_else(|| block.get("output_text"))
                    .or_else(|| block.get("summary_text"))
                    .and_then(Value::as_str)
                {
                    parts.push(text.to_owned());
                    continue;
                }
                if let Some(text) = block.get("content").and_then(Value::as_str) {
                    parts.push(text.to_owned());
                    continue;
                }
            }
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n"))
            }
        }
        Value::Object(object) => {
            for key in [
                "text",
                "input_text",
                "output_text",
                "summary_text",
                "content",
            ] {
                if let Some(text) = object.get(key).and_then(Value::as_str) {
                    return Some(text.to_owned());
                }
                if let Some(text) = object.get(key).and_then(codex_content_text) {
                    return Some(text);
                }
            }
            None
        }
        _ => None,
    }
}
pub(crate) fn codex_json_text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        Value::Array(_) | Value::Object(_) => serde_json::to_string(value).ok(),
        _ => Some(value.to_string()),
    }
}
pub(crate) fn codex_capped_json(value: &Value, max_chars: usize) -> Value {
    match value {
        Value::String(text) => {
            let (text, truncated) = capped_text(text, max_chars);
            json!({ "text": text, "truncated": truncated })
        }
        _ => {
            let rendered = serde_json::to_string(value).unwrap_or_else(|_| "null".to_owned());
            let (text, truncated) = capped_text(&rendered, max_chars);
            json!({ "json": text, "truncated": truncated })
        }
    }
}
