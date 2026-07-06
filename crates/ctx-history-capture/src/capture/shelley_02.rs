#[allow(unused_imports)]
use super::*;

pub(crate) fn shelley_collect_text(value: &Value, parts: &mut Vec<String>) {
    match value {
        Value::String(text) => shelley_push_text(parts, text),
        Value::Array(items) => {
            for item in items {
                if shelley_text_budget_remaining(parts) == 0 {
                    break;
                }
                shelley_collect_text(item, parts);
            }
        }
        Value::Object(object) => {
            if let Some(kind) = shelley_content_type(value) {
                let handled = match kind.as_str() {
                    "text" => {
                        if let Some(text) = object.get("Text").and_then(Value::as_str) {
                            shelley_push_text(parts, text);
                        }
                        true
                    }
                    "thinking" | "redacted_thinking" => {
                        if let Some(text) = object.get("Thinking").and_then(Value::as_str) {
                            shelley_push_text(parts, text);
                        }
                        true
                    }
                    "tool_use" | "server_tool_use" => {
                        let name = object
                            .get("ToolName")
                            .and_then(Value::as_str)
                            .unwrap_or("tool");
                        shelley_push_text(parts, &format!("tool call: {name}"));
                        if let Some(input) = object.get("ToolInput") {
                            if !input.is_null() {
                                let input = provider_capped_json(input, PROVIDER_MAX_PREVIEW_CHARS);
                                shelley_push_text(parts, &format!("tool input: {input}"));
                            }
                        }
                        true
                    }
                    "tool_result" | "web_search_tool_result" => {
                        shelley_push_text(parts, "tool result");
                        if let Some(results) = object.get("ToolResult") {
                            shelley_collect_text(results, parts);
                        }
                        if let Some(display) = object.get("Display") {
                            shelley_collect_text(display, parts);
                        }
                        true
                    }
                    "web_search_result" => {
                        for key in ["Title", "URL", "PageAge"] {
                            if let Some(text) = object.get(key).and_then(Value::as_str) {
                                shelley_push_text(parts, text);
                            }
                        }
                        true
                    }
                    _ => false,
                };
                if handled {
                    return;
                }
            }

            for key in [
                "Text",
                "text",
                "Thinking",
                "thinking",
                "content",
                "Content",
                "output",
                "Output",
                "summary",
                "Summary",
                "message",
                "Message",
                "error",
                "Error",
                "LLMContent",
                "ToolResult",
                "Display",
            ] {
                if shelley_text_budget_remaining(parts) == 0 {
                    break;
                }
                if let Some(child) = object.get(key) {
                    shelley_collect_text(child, parts);
                }
            }
        }
        Value::Number(_) | Value::Bool(_) | Value::Null => {}
    }
}

pub(crate) fn shelley_push_text(parts: &mut Vec<String>, text: &str) {
    let text = text.trim();
    if !text.is_empty() {
        let remaining = shelley_text_budget_remaining(parts);
        if remaining == 0 {
            return;
        }
        let separator_budget = usize::from(!parts.is_empty());
        if remaining <= separator_budget {
            return;
        }
        let (text, _) = capped_text(text, remaining - separator_budget);
        parts.push(text);
    }
}

pub(crate) fn shelley_text_budget_remaining(parts: &[String]) -> usize {
    let used = parts.iter().map(|part| part.chars().count()).sum::<usize>()
        + parts.len().saturating_sub(1);
    (PROVIDER_MAX_TEXT_CHARS + 1).saturating_sub(used)
}

pub(crate) fn shelley_content_type(value: &Value) -> Option<String> {
    let raw = value.get("Type")?;
    if let Some(text) = raw.as_str() {
        let normalized = text.trim().to_ascii_lowercase();
        return match normalized.as_str() {
            "contenttypetext" => Some("text".to_owned()),
            "contenttypethinking" => Some("thinking".to_owned()),
            "contenttyperedactedthinking" => Some("redacted_thinking".to_owned()),
            "contenttypetooluse" => Some("tool_use".to_owned()),
            "contenttypetoolresult" => Some("tool_result".to_owned()),
            "contenttypeservertooluse" => Some("server_tool_use".to_owned()),
            "contenttypewebsearchtoolresult" => Some("web_search_tool_result".to_owned()),
            "contenttypewebsearchresult" => Some("web_search_result".to_owned()),
            _ => Some(normalized),
        };
    }
    raw.as_i64().and_then(|kind| {
        match kind {
            2 => Some("text"),
            3 => Some("thinking"),
            4 => Some("redacted_thinking"),
            5 => Some("tool_use"),
            6 => Some("tool_result"),
            7 => Some("server_tool_use"),
            8 => Some("web_search_tool_result"),
            9 => Some("web_search_result"),
            _ => None,
        }
        .map(str::to_owned)
    })
}
