#[allow(unused_imports)]
use super::*;

pub(crate) fn crush_parts_text(parts: &Value) -> Option<String> {
    let mut text = Vec::new();
    if let Some(items) = parts.as_array() {
        for item in items {
            let kind = item.get("type").and_then(Value::as_str).unwrap_or("part");
            let data = item.get("data").unwrap_or(item);
            match kind {
                "text" => push_json_text(&mut text, data.get("text").unwrap_or(data)),
                "reasoning" => {
                    push_json_text(
                        &mut text,
                        data.get("thinking")
                            .or_else(|| data.get("text"))
                            .unwrap_or(data),
                    );
                }
                "tool_call" => {
                    let name = data.get("name").and_then(Value::as_str).unwrap_or("tool");
                    text.push(format!("tool call: {name}"));
                    if let Some(input) = data.get("input").and_then(provider_value_text) {
                        text.push(format!("tool input: {input}"));
                    }
                }
                "tool_result" => {
                    let name = data.get("name").and_then(Value::as_str).unwrap_or("tool");
                    text.push(format!("tool result: {name}"));
                    for key in ["content", "data", "output"] {
                        if let Some(value) = data.get(key).and_then(provider_value_text) {
                            text.push(value);
                            break;
                        }
                    }
                }
                "shell_command" => {
                    if let Some(command) = data.get("command").and_then(Value::as_str) {
                        text.push(command.to_owned());
                    }
                    if let Some(output) = data.get("output").and_then(Value::as_str) {
                        text.push(output.to_owned());
                    }
                }
                "finish" => {
                    if let Some(reason) = data.get("reason").and_then(Value::as_str) {
                        text.push(format!("finish: {reason}"));
                    }
                }
                _ => push_json_text(&mut text, data),
            }
            if text.iter().map(|part| part.chars().count()).sum::<usize>()
                >= PROVIDER_MAX_TEXT_CHARS
            {
                break;
            }
        }
    } else {
        push_json_text(&mut text, parts);
    }
    (!text.is_empty()).then(|| text.join("\n"))
}
