use ctx_history_core::{EventRole, EventType, RedactionState};

pub(super) fn event_search_preview_from_payload(
    event_type: EventType,
    role: Option<EventRole>,
    payload: &serde_json::Value,
    redaction_state: RedactionState,
) -> String {
    if matches!(
        redaction_state,
        RedactionState::Raw | RedactionState::Withheld
    ) {
        return String::new();
    }
    let preview = match event_type {
        EventType::Message if event_role_is_searchable_conversation(role) => {
            event_payload_text_preview(payload)
        }
        EventType::Summary => event_payload_text_preview(payload),
        EventType::ToolCall | EventType::CommandStarted | EventType::CommandFinished => {
            event_tool_call_preview(payload)
        }
        EventType::ToolOutput | EventType::CommandOutput => None,
        _ => None,
    }
    .unwrap_or_default();
    local_preview(&preview, 2048)
}

pub(super) fn local_preview(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

fn event_role_is_searchable_conversation(role: Option<EventRole>) -> bool {
    matches!(
        role,
        Some(EventRole::User | EventRole::Assistant | EventRole::System) | None
    )
}

fn event_payload_text_preview(payload: &serde_json::Value) -> Option<String> {
    if let Some(body) = payload.get("body") {
        if let Some(preview) = event_text_value_preview(body) {
            return Some(preview);
        }
    }
    event_text_value_preview(payload)
}

fn event_text_value_preview(value: &serde_json::Value) -> Option<String> {
    if let Some(value) = value.as_str() {
        return non_blank(value);
    }
    let object = value.as_object()?;
    for key in ["text", "preview", "summary", "message"] {
        if let Some(value) = object.get(key).and_then(event_preview_fragment) {
            return Some(value);
        }
    }
    None
}

fn event_tool_call_preview(payload: &serde_json::Value) -> Option<String> {
    if let Some(body) = payload.get("body") {
        if let Some(preview) = event_tool_call_preview_fields(body) {
            return Some(preview);
        }
    }
    event_tool_call_preview_fields(payload)
}

fn event_tool_call_preview_fields(payload: &serde_json::Value) -> Option<String> {
    let object = payload.as_object()?;
    if let Some(command) = object.get("command").and_then(event_preview_fragment) {
        return Some(command);
    }
    if let Some(text) = object.get("text").and_then(event_preview_fragment) {
        return Some(text);
    }
    let structured = ["tool", "name", "arguments_preview", "status"]
        .into_iter()
        .filter_map(|key| {
            object
                .get(key)
                .and_then(event_preview_fragment)
                .map(|value| format!("{key}: {value}"))
        })
        .collect::<Vec<_>>();
    if structured.is_empty() {
        None
    } else {
        Some(structured.join(" | "))
    }
}

fn event_preview_fragment(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => non_blank(value),
        serde_json::Value::Number(_) | serde_json::Value::Bool(_) => Some(value.to_string()),
        _ => None,
    }
}

fn non_blank(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn failed_output_preview_is_not_searchable() {
        let payload = json!({
            "result_outcome": "failure",
            "exit_code": 1,
            "output_preview": "private failed output"
        });
        assert!(event_search_preview_from_payload(
            EventType::ToolOutput,
            Some(EventRole::Tool),
            &payload,
            RedactionState::Redacted,
        )
        .is_empty());
    }
}
