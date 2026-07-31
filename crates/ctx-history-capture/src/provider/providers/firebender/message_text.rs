use serde_json::Value;

use crate::provider::normalization::provider_value_text;

pub(crate) fn firebender_message_text(message: &Value) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(text) = message.get("content").and_then(firebender_content_text) {
        parts.push(text);
    }
    if let Some(tool_calls) = message
        .get("tool_calls")
        .or_else(|| message.get("toolCalls"))
        .and_then(Value::as_array)
    {
        for call in tool_calls {
            let function = call.get("function");
            let name = function
                .and_then(|function| function.get("name"))
                .or_else(|| call.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("tool");
            parts.push(format!("tool call: {name}"));
            if let Some(arguments) = function
                .and_then(|function| function.get("arguments"))
                .or_else(|| call.get("arguments"))
                .or_else(|| call.get("input"))
                .and_then(provider_value_text)
                .filter(|text| !text.trim().is_empty())
            {
                parts.push(format!("tool input: {arguments}"));
            }
        }
    }
    if !parts.is_empty() {
        return Some(parts.join("\n"));
    }
    message
        .get("name")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(str::to_owned)
}

fn firebender_content_text(content: &Value) -> Option<String> {
    let text = match content {
        Value::Object(object) => object
            .get("text")
            .or_else(|| object.get("content"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        _ => provider_value_text(content),
    }?;
    (!text.trim().is_empty()).then_some(text)
}
