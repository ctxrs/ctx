#[allow(unused_imports)]
use super::*;

pub(crate) fn zed_tool_results_text(value: Option<&Value>) -> Option<String> {
    let object = value?.as_object()?;
    let mut parts = Vec::new();
    for result in object.values() {
        let name = result
            .get("tool_name")
            .and_then(Value::as_str)
            .unwrap_or("tool");
        parts.push(format!("tool result: {name}"));
        if result
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            parts.push("tool error".to_owned());
        }
        if let Some(content) = zed_tool_result_content_text(result.get("content")) {
            parts.push(content);
        }
        if let Some(output) = result.get("output").and_then(provider_value_text) {
            parts.push(output);
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

pub(crate) fn zed_tool_result_content_text(value: Option<&Value>) -> Option<String> {
    let value = value?;
    if let Some(text) = value.as_str() {
        return Some(text.to_owned());
    }
    if let Some(items) = value.as_array() {
        let mut parts = Vec::new();
        for item in items {
            if let Some((kind, body)) = zed_external_tag(item) {
                match kind {
                    "Text" => {
                        if let Some(text) = body.as_str() {
                            parts.push(text.to_owned());
                        }
                    }
                    "Image" => parts.push("<image />".to_owned()),
                    _ => {
                        if let Some(text) = provider_value_text(body) {
                            parts.push(text);
                        }
                    }
                }
            } else if let Some(text) = provider_value_text(item) {
                parts.push(text);
            }
        }
        return (!parts.is_empty()).then(|| parts.join("\n"));
    }
    provider_value_text(value)
}

pub(crate) fn zed_external_tag(value: &Value) -> Option<(&str, &Value)> {
    let object = value.as_object()?;
    if object.len() != 1 {
        return None;
    }
    object
        .iter()
        .next()
        .map(|(key, value)| (key.as_str(), value))
}

pub(crate) fn zed_has_tool_use(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.iter().any(zed_has_tool_use),
        Value::Object(object) => {
            object.contains_key("ToolUse")
                || object.get("content").is_some_and(zed_has_tool_use)
                || object.values().any(zed_has_tool_use)
        }
        _ => false,
    }
}

pub(crate) fn zed_has_tool_result(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.iter().any(zed_has_tool_result),
        Value::Object(object) => {
            object
                .get("tool_results")
                .and_then(Value::as_object)
                .is_some_and(|results| !results.is_empty())
                || object.contains_key("ToolResult")
                || object.values().any(zed_has_tool_result)
        }
        _ => false,
    }
}
