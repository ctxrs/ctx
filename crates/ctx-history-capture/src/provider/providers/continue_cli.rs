use std::path::Path;

use ctx_history_core::EventType;
use ctx_history_store::Store;
use serde_json::{json, Value};

use crate::provider::normalization::{
    provider_capped_json, provider_policy_body, provider_policy_event_text,
    provider_result_identifier_evidence, provider_result_outcome_evidence, provider_value_text,
};
use crate::{
    ProviderAdapterContext, ProviderImportOptions, ProviderImportSummary, Result,
    CONTINUE_CLI_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS,
};

mod message_text;
pub(crate) mod native_path;

pub(crate) use message_text::continue_history_item_text;

/// Production entrypoint retained under its released crate-private name until
/// the shared provider API registration is renamed. The implementation is
/// NativePath-only.
pub(crate) fn import_continue_cli_nativepath(
    path: &Path,
    store: &mut Store,
    context: ProviderAdapterContext,
    import_options: ProviderImportOptions,
) -> Result<ProviderImportSummary> {
    native_path::import_continue_nativepath_history(path, store, context, import_options)
}

pub(crate) fn continue_session_json_path(path: &Path) -> bool {
    path.extension().and_then(|ext| ext.to_str()) == Some("json")
        && path.file_name().and_then(|name| name.to_str()) != Some("sessions.json")
}

// This narrow projection reconstructs the canonical payload hash authority for
// complete-content locators emitted by released Continue imports. NativePath
// production never emits those locators or routes successful output bodies
// through Core.
pub(crate) fn continue_history_item_canonical_payload(item: &Value) -> (EventType, Value) {
    let has_tool_calls = item
        .get("toolCallStates")
        .and_then(Value::as_array)
        .is_some_and(|states| !states.is_empty());
    let event_type = if has_tool_calls {
        EventType::ToolCall
    } else {
        EventType::Message
    };
    let text = continue_history_item_text(item).unwrap_or_default();
    let retained_text = provider_policy_event_text(event_type, &text, item);
    let body = provider_policy_body(event_type, item);
    (
        event_type,
        json!({
            "text": retained_text.text,
            "text_retention": retained_text.retention.as_json(),
            "result_evidence": provider_result_identifier_evidence(event_type, &text, item),
            "result_outcome": provider_result_outcome_evidence(event_type, item),
            "source_format": CONTINUE_CLI_SOURCE_FORMAT,
            "body": provider_capped_json(&body, PROVIDER_MAX_PREVIEW_CHARS),
        }),
    )
}

pub(crate) fn continue_context_items_text(value: &Value) -> Option<String> {
    let items = value.as_array()?;
    let mut parts = Vec::new();
    for item in items {
        if let Some(content) = item.get("content").and_then(provider_value_text) {
            parts.push(content);
        } else if let Some(name) = item.get("name").and_then(Value::as_str) {
            parts.push(name.to_owned());
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

pub(crate) fn continue_tool_states_text(value: &Value) -> Option<String> {
    let states = value.as_array()?;
    let mut parts = Vec::new();
    for state in states {
        let name = state
            .pointer("/toolCall/function/name")
            .or_else(|| state.pointer("/toolCall/name"))
            .and_then(Value::as_str)
            .unwrap_or("tool");
        let status = state
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        parts.push(format!("tool: {name} | status: {status}"));
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}
