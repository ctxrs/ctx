use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use ctx_history_core::{CaptureProvider, EventRole, EventType, ProviderEventEnvelope};
use ctx_history_store::Store;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::provider::codex::events::{
    codex_command_preview, codex_local_preview, codex_provider_event, codex_session_event,
    codex_tool_arguments_preview, codex_tool_name, codex_tool_output_projection,
    CodexProjectedEvent, CodexToolCallContext,
};
use crate::provider::importer::provider_scoped_source_uuid;
use crate::{
    ProviderAdapterContext, Result, CODEX_SESSION_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS,
};

use super::header::CodexSessionHeader;
use super::{
    CODEX_MAX_TOOL_CALL_ID_BYTES, CODEX_MAX_TOOL_CONTEXTS, CODEX_MAX_TOOL_NAME_BYTES,
    CODEX_MAX_TOOL_PREVIEW_BYTES,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CodexToolCallCheckpoint {
    #[serde(rename = "t", alias = "tool_name")]
    tool_name: String,
}

impl CodexToolCallCheckpoint {
    fn from_context(context: &CodexToolCallContext) -> Self {
        Self {
            tool_name: truncate_utf8(&context.tool_name, CODEX_MAX_TOOL_NAME_BYTES),
        }
    }

    fn into_context(self) -> CodexToolCallContext {
        CodexToolCallContext {
            tool_name: self.tool_name,
            command_preview: None,
            arguments_preview: None,
        }
    }
}

pub(super) struct CodexToolCorrelation {
    contexts: BTreeMap<String, CodexToolCallContext>,
}

impl CodexToolCorrelation {
    pub(super) fn fresh() -> Self {
        Self {
            contexts: BTreeMap::new(),
        }
    }

    pub(super) fn from_checkpoint(contexts: BTreeMap<String, CodexToolCallCheckpoint>) -> Self {
        Self {
            contexts: contexts
                .into_iter()
                .map(|(call_id, context)| (call_id, context.into_context()))
                .collect(),
        }
    }

    pub(super) fn checkpoint(&self) -> BTreeMap<String, CodexToolCallCheckpoint> {
        self.contexts
            .iter()
            .map(|(call_id, context)| {
                (
                    call_id.clone(),
                    CodexToolCallCheckpoint::from_context(context),
                )
            })
            .collect()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.contexts.is_empty()
    }

    pub(super) fn clear(&mut self) {
        self.contexts.clear();
    }

    pub(super) fn event(
        &mut self,
        value: &Value,
        line_number: usize,
        occurred_at: DateTime<Utc>,
    ) -> Option<CodexProjectedEvent> {
        let Some(payload) = value
            .get("payload")
            .filter(|_| value.get("type").and_then(Value::as_str) == Some("response_item"))
        else {
            return codex_session_event(value, line_number, occurred_at)
                .map(CodexProjectedEvent::without_result);
        };
        match payload.get("type").and_then(Value::as_str) {
            Some("function_call" | "custom_tool_call" | "web_search_call" | "tool_search_call") => {
                codex_tool_call_event(payload, line_number, occurred_at, &mut self.contexts)
                    .map(CodexProjectedEvent::without_result)
            }
            Some("function_call_output" | "custom_tool_call_output" | "tool_search_output") => {
                let completed_call_id = payload
                    .get("call_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let event =
                    codex_tool_output_projection(payload, line_number, occurred_at, &self.contexts);
                if let Some(call_id) = completed_call_id {
                    self.contexts.remove(&call_id);
                }
                event
            }
            _ => codex_session_event(value, line_number, occurred_at)
                .map(CodexProjectedEvent::without_result),
        }
    }

    pub(super) fn bound_retained_contexts(&mut self) {
        self.contexts
            .retain(|call_id, _| call_id.len() <= CODEX_MAX_TOOL_CALL_ID_BYTES);
        for context in self.contexts.values_mut() {
            context.tool_name = truncate_utf8(&context.tool_name, CODEX_MAX_TOOL_NAME_BYTES);
            context.command_preview = context
                .command_preview
                .as_deref()
                .map(|value| truncate_utf8(value, CODEX_MAX_TOOL_PREVIEW_BYTES));
            context.arguments_preview = context
                .arguments_preview
                .as_deref()
                .map(|value| truncate_utf8(value, CODEX_MAX_TOOL_PREVIEW_BYTES));
        }
        while self.contexts.len() > CODEX_MAX_TOOL_CONTEXTS {
            if !self.drop_oldest() {
                break;
            }
        }
    }

    pub(super) fn drop_oldest(&mut self) -> bool {
        let Some(call_id) = self.contexts.keys().next().cloned() else {
            return false;
        };
        self.contexts.remove(&call_id);
        true
    }

    pub(super) fn hydrate_from_store(
        &mut self,
        store: &Store,
        context: &ProviderAdapterContext,
        header: &CodexSessionHeader,
    ) -> Result<()> {
        if self.contexts.is_empty() {
            return Ok(());
        }
        let raw_source_path = context
            .source_path
            .as_ref()
            .map(|path| path.display().to_string());
        let source_id = provider_scoped_source_uuid(
            CaptureProvider::Codex,
            &header.id,
            CODEX_SESSION_SOURCE_FORMAT,
            raw_source_path.as_deref(),
        );
        let Some(session) = store.session_by_capture_source_and_external_session(
            source_id,
            CaptureProvider::Codex,
            &header.id,
        )?
        else {
            return Ok(());
        };
        for (call_id, call_context) in &mut self.contexts {
            let Some(event) = store.event_for_session_by_type_and_payload_string(
                session.id,
                EventType::ToolCall,
                "$.body.call_id",
                call_id,
            )?
            else {
                continue;
            };
            call_context.command_preview = event
                .payload
                .pointer("/body/command")
                .and_then(Value::as_str)
                .map(|value| truncate_utf8(value, CODEX_MAX_TOOL_PREVIEW_BYTES));
            call_context.arguments_preview = event
                .payload
                .pointer("/body/arguments_preview")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(|value| truncate_utf8(value, CODEX_MAX_TOOL_PREVIEW_BYTES));
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn insert_for_test(&mut self, call_id: String, context: CodexToolCallContext) {
        self.contexts.insert(call_id, context);
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.contexts.len()
    }
}

fn codex_tool_call_event(
    payload: &Value,
    line_number: usize,
    occurred_at: DateTime<Utc>,
    call_contexts: &mut BTreeMap<String, CodexToolCallContext>,
) -> Option<ProviderEventEnvelope> {
    let item_type = payload
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("tool_call");
    let tool_name = codex_tool_name(payload, item_type);
    let call_id = payload.get("call_id").and_then(Value::as_str);
    let argument_value = payload
        .get("arguments")
        .or_else(|| payload.get("input"))
        .or_else(|| payload.get("action"))
        .or_else(|| payload.get("execution"));
    let command_preview = codex_command_preview(&tool_name, argument_value);
    let (arguments_preview, arguments_truncated, raw_arguments_retained) = argument_value
        .map(codex_tool_arguments_preview)
        .unwrap_or_else(|| (String::new(), false, false));
    let text = command_preview
        .as_ref()
        .map(|command| format!("{tool_name}: {command}"))
        .unwrap_or_else(|| {
            if arguments_preview.is_empty() {
                format!("{tool_name} tool call")
            } else {
                format!("{tool_name}: {arguments_preview}")
            }
        });
    let (text, text_truncated) = codex_local_preview(&text, PROVIDER_MAX_PREVIEW_CHARS);

    if let Some(call_id) = call_id {
        call_contexts.insert(
            call_id.to_owned(),
            CodexToolCallContext {
                tool_name: tool_name.clone(),
                command_preview: command_preview.clone(),
                arguments_preview: (!arguments_preview.is_empty())
                    .then_some(arguments_preview.clone()),
            },
        );
    }

    Some(codex_provider_event(
        line_number,
        occurred_at,
        EventType::ToolCall,
        Some(EventRole::Assistant),
        json!({
            "item_type": item_type,
            "tool": tool_name,
            "name": tool_name,
            "call_id": call_id,
            "command": command_preview,
            "arguments_preview": arguments_preview,
            "arguments_truncated": arguments_truncated,
            "raw_arguments_retained": raw_arguments_retained,
            "text": text,
            "truncated": text_truncated || arguments_truncated,
        }),
        json!({
            "source": "codex_session",
            "source_format": CODEX_SESSION_SOURCE_FORMAT,
            "line": line_number,
            "item_type": item_type,
            "tool": tool_name,
        }),
    ))
}
fn truncate_utf8(value: &str, maximum_bytes: usize) -> String {
    if value.len() <= maximum_bytes {
        return value.to_owned();
    }
    let mut boundary = maximum_bytes;
    while !value.is_char_boundary(boundary) {
        boundary = boundary.saturating_sub(1);
    }
    value[..boundary].to_owned()
}
