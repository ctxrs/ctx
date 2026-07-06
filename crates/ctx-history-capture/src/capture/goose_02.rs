#[allow(unused_imports)]
use super::*;

pub(crate) fn goose_collect_text(value: &Value, parts: &mut Vec<String>) {
    match value {
        Value::String(text) => parts.push(text.clone()),
        Value::Array(items) => {
            for item in items {
                goose_collect_text(item, parts);
                if parts.iter().map(|part| part.chars().count()).sum::<usize>()
                    >= PROVIDER_MAX_TEXT_CHARS
                {
                    break;
                }
            }
        }
        Value::Object(object) => {
            let kind = object.get("type").and_then(Value::as_str);
            match kind {
                Some("text") => {
                    if let Some(text) = object.get("text").and_then(Value::as_str) {
                        parts.push(text.to_owned());
                    }
                }
                Some("thinking") => {
                    if let Some(text) = object.get("thinking").and_then(Value::as_str) {
                        parts.push(text.to_owned());
                    }
                }
                Some("redactedThinking") => {
                    parts.push("redacted thinking".to_owned());
                }
                Some("toolRequest") | Some("frontendToolRequest") => {
                    let call = object.get("toolCall").unwrap_or(value);
                    let name = call
                        .get("name")
                        .or_else(|| object.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or("tool");
                    parts.push(format!("tool call: {name}"));
                    if let Some(input) = call
                        .get("arguments")
                        .or_else(|| call.get("input"))
                        .and_then(provider_value_text)
                    {
                        parts.push(format!("tool input: {input}"));
                    }
                }
                Some("toolResponse") => {
                    parts.push("tool response".to_owned());
                    for key in ["toolResult", "content", "result"] {
                        if let Some(text) = object.get(key).and_then(provider_value_text) {
                            parts.push(text);
                            break;
                        }
                    }
                }
                Some("toolConfirmationRequest") => {
                    parts.push("tool confirmation request".to_owned());
                }
                Some("systemNotification") | Some("actionRequired") => {
                    for key in ["message", "text", "content"] {
                        if let Some(text) = object.get(key).and_then(provider_value_text) {
                            parts.push(text);
                            break;
                        }
                    }
                }
                _ => {
                    for key in ["text", "content", "message"] {
                        if let Some(text) = object.get(key).and_then(provider_value_text) {
                            parts.push(text);
                            return;
                        }
                    }
                }
            }
        }
        Value::Number(_) | Value::Bool(_) => parts.push(value.to_string()),
        Value::Null => {}
    }
}
