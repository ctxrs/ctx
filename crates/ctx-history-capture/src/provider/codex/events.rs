use chrono::{DateTime, Utc};
use ctx_history_core::{EventRole, EventType, Fidelity, ProviderArtifactDescriptor};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::provider::normalization::capped_text;
use crate::PROVIDER_MAX_TEXT_CHARS;

mod retention;
mod tool;

use retention::codex_event_role;
pub(crate) use retention::{
    codex_command_preview, codex_command_text, codex_content_text, codex_is_command_tool,
    codex_local_preview, codex_tool_arguments_preview, codex_tool_arguments_text, codex_tool_name,
    CodexExitCodeParser, CodexWallTimeParser,
};
#[cfg(test)]
pub(crate) use tool::codex_tool_output_outcome;
pub(crate) use tool::{codex_result_content, CodexToolCallContext};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct CodexNativeEvent {
    pub(crate) provider_event_index: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) provider_event_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cursor: Option<String>,
    #[serde(default)]
    pub(crate) event_type: EventType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) role: Option<EventRole>,
    pub(crate) occurred_at: DateTime<Utc>,
    #[serde(default)]
    pub(crate) fidelity: Fidelity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) idempotency_key: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) artifacts: Vec<ProviderArtifactDescriptor>,
    #[serde(default = "crate::common::json::default_metadata")]
    pub(crate) payload: Value,
    #[serde(default = "crate::common::json::default_metadata")]
    pub(crate) metadata: Value,
}

pub(crate) fn codex_message_body(payload: &Value) -> Option<(EventRole, Value)> {
    let role_text = payload
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    if !matches!(role_text, "user" | "assistant" | "developer" | "system") {
        return None;
    }
    let text = payload.get("content").and_then(codex_content_text)?;
    let (text, truncated) = capped_text(&text, PROVIDER_MAX_TEXT_CHARS);
    Some((
        codex_event_role(role_text),
        json!({
            "item_type": "message",
            "message_role": role_text,
            "phase": payload.get("phase").and_then(Value::as_str),
            "text": text,
            "truncated": truncated,
        }),
    ))
}

pub(crate) fn codex_provider_event(
    line_number: usize,
    occurred_at: DateTime<Utc>,
    event_type: EventType,
    role: Option<EventRole>,
    payload: Value,
    metadata: Value,
) -> CodexNativeEvent {
    CodexNativeEvent {
        provider_event_index: (line_number - 1) as u64,
        provider_event_hash: None,
        cursor: Some(format!("line:{line_number}")),
        event_type,
        role,
        occurred_at,
        fidelity: Fidelity::Imported,
        idempotency_key: Some(format!("provider-event:codex-session:{line_number}")),
        artifacts: Vec::new(),
        payload,
        metadata,
    }
}
