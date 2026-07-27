use serde_json::Value;

use crate::provider::normalization::provider_value_text;

pub(crate) fn firebender_message_text(message: &Value) -> Option<String> {
    if let Some(content) = message.get("content") {
        match content {
            Value::Object(object) => {
                if let Some(text) = object
                    .get("text")
                    .or_else(|| object.get("content"))
                    .and_then(Value::as_str)
                    .filter(|text| !text.trim().is_empty())
                {
                    return Some(text.to_owned());
                }
            }
            _ => {
                if let Some(text) =
                    provider_value_text(content).filter(|text| !text.trim().is_empty())
                {
                    return Some(text);
                }
            }
        }
    }
    if let Some(tool_calls) = message
        .get("tool_calls")
        .or_else(|| message.get("toolCalls"))
        .and_then(Value::as_array)
    {
        let names = tool_calls
            .iter()
            .filter_map(|call| {
                call.get("function")
                    .and_then(|function| function.get("name"))
                    .or_else(|| call.get("name"))
                    .and_then(Value::as_str)
            })
            .collect::<Vec<_>>();
        if !names.is_empty() {
            return Some(format!("tool call: {}", names.join(", ")));
        }
    }
    message
        .get("name")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(str::to_owned)
}
