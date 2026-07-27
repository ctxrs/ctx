use ctx_history_core::EventRole;
use serde_json::Value;

pub(crate) fn pi_event_role(role: &str) -> EventRole {
    match role {
        "user" => EventRole::User,
        "assistant" => EventRole::Assistant,
        "toolResult" | "bashExecution" => EventRole::Tool,
        "system" => EventRole::System,
        _ => EventRole::Unknown,
    }
}

pub(crate) fn pi_message_has_tool_call(message: &Value) -> bool {
    message
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .any(|block| block.get("type").and_then(Value::as_str) == Some("toolCall"))
        })
        .unwrap_or(false)
}

pub(crate) fn pi_entry_text(entry: &Value, message: Option<&Value>) -> Option<String> {
    if let Some(text) = message.and_then(pi_message_text) {
        return Some(text);
    }
    match entry
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
    {
        "compaction" | "branch_summary" => entry
            .get("summary")
            .and_then(Value::as_str)
            .map(str::to_owned),
        "custom_message" => entry.get("content").and_then(pi_content_text),
        "session_info" => entry.get("name").and_then(Value::as_str).map(str::to_owned),
        "label" => entry
            .get("label")
            .and_then(Value::as_str)
            .map(str::to_owned),
        "model_change" => {
            let provider = entry.get("provider").and_then(Value::as_str).unwrap_or("");
            let model = entry.get("modelId").and_then(Value::as_str).unwrap_or("");
            let label = [provider, model]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("/");
            (!label.is_empty()).then_some(label)
        }
        "thinking_level_change" => entry
            .get("thinkingLevel")
            .and_then(Value::as_str)
            .map(str::to_owned),
        "custom" => entry
            .get("customType")
            .and_then(Value::as_str)
            .map(str::to_owned),
        _ => None,
    }
}

pub(crate) fn pi_message_text(message: &Value) -> Option<String> {
    if let Some(command) = message.get("command").and_then(Value::as_str) {
        let output = message.get("output").and_then(Value::as_str).unwrap_or("");
        return Some(if output.is_empty() {
            command.to_owned()
        } else {
            format!("{command}\n{output}")
        });
    }
    if let Some(summary) = message
        .get("summary")
        .or_else(|| message.get("content"))
        .and_then(Value::as_str)
    {
        return Some(summary.to_owned());
    }
    message.get("content").and_then(pi_content_text)
}

pub(crate) fn pi_content_text(content: &Value) -> Option<String> {
    if let Some(text) = content.as_str() {
        return Some(text.to_owned());
    }
    let blocks = content.as_array()?;
    let mut parts = Vec::new();
    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    parts.push(text.to_owned());
                }
            }
            Some("thinking") => {
                if let Some(text) = block.get("thinking").and_then(Value::as_str) {
                    parts.push(text.to_owned());
                }
            }
            Some("toolCall") => {
                let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
                parts.push(format!("tool call: {name}"));
            }
            _ => {}
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}
