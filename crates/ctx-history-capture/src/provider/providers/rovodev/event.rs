use ctx_history_core::EventType;
use serde_json::Value;

use crate::provider::normalization::provider_block_event_type;

pub(super) fn rovodev_event_type(message: &Value, role_text: Option<&str>) -> EventType {
    if role_text.is_some_and(|role| {
        matches!(
            role.trim().to_ascii_lowercase().as_str(),
            "tool" | "tool_result" | "tool-result" | "tool_use_result" | "function_result"
        )
    }) {
        EventType::ToolOutput
    } else {
        provider_block_event_type(message, role_text)
    }
}
