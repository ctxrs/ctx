use std::path::Path;

use serde_json::Value;

use crate::provider::normalization::provider_value_text;

pub(crate) mod native_path;

pub(crate) fn continue_session_json_path(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()) == Some("json")
        && path.file_name().and_then(|name| name.to_str()) != Some("sessions.json")
}

pub(crate) fn continue_context_items_text(value: &Value) -> Option<String> {
    let items = value.as_array()?;
    let mut parts = Vec::new();
    for item in items {
        if let Some(content) = item.get("content").and_then(provider_value_text) {
            parts.push(content);
        } else if let Some(name) = item.get("name").and_then(Value::as_str) {
            parts.push(name.to_owned());
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

pub(crate) fn continue_tool_states_text(value: &Value) -> Option<String> {
    let states = value.as_array()?;
    let mut parts = Vec::new();
    for state in states {
        let name = state
            .pointer("/toolCall/function/name")
            .or_else(|| state.pointer("/toolCall/name"))
            .and_then(Value::as_str)
            .unwrap_or("tool");
        let status = state
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        parts.push(format!("tool: {name} | status: {status}"));
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}
