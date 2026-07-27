use serde_json::Value;

use crate::provider::normalization::provider_value_text;

use super::{continue_context_items_text, continue_tool_states_text};

pub(crate) fn continue_history_item_text(item: &Value) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(text) = item
        .pointer("/message/content")
        .and_then(provider_value_text)
        .or_else(|| item.get("editorState").and_then(provider_value_text))
    {
        parts.push(text);
    }
    if let Some(text) = item
        .get("contextItems")
        .and_then(continue_context_items_text)
    {
        parts.push(text);
    }
    if let Some(text) = item
        .get("toolCallStates")
        .and_then(continue_tool_states_text)
    {
        parts.push(text);
    }
    if let Some(text) = item.get("conversationSummary").and_then(Value::as_str) {
        parts.push(text.to_owned());
    }
    let text = parts
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (!text.trim().is_empty()).then_some(text)
}
