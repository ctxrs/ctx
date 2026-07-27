use chrono::{DateTime, Utc};
use ctx_history_core::{
    compute_payload_hash, CaptureProvider, Confidence, EventRole, EventType, FileChangeKind,
};
use serde::{ser::SerializeStruct, Deserialize, Serialize, Serializer};
use serde_json::{json, Value};

use super::record::{CodexDecodedRecord, CodexResultKind, CodexRetainedKind};
use crate::provider::codex::events::{
    codex_command_preview, codex_content_text, codex_local_preview, codex_message_body,
    codex_provider_event, codex_sparse_tool_output_event, codex_tool_arguments_preview,
    codex_tool_name, CodexNativeEvent, CodexToolCallContext,
};
use crate::OutputOutcomeMetadata;
use crate::{CODEX_SESSION_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS, PROVIDER_MAX_TEXT_CHARS};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CodexSessionRow {
    pub(crate) native_session_id: String,
    pub(crate) parent_native_session_id: Option<String>,
    pub(crate) root_native_session_id: Option<String>,
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) cwd: Option<String>,
    pub(crate) originator: Option<String>,
    pub(crate) cli_version: Option<String>,
    pub(crate) source_kind: Option<String>,
    pub(crate) external_agent_id: Option<String>,
    pub(crate) role_hint: Option<String>,
    pub(crate) model_provider: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CodexEventRow {
    pub(crate) raw_ordinal: u64,
    pub(crate) normalized_body_hash: String,
    pub(crate) provider_event: CodexNativeEvent,
    pub(crate) file_touches: Vec<CodexFileTouch>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct CodexFileTouch {
    pub(crate) provider: CaptureProvider,
    pub(crate) provider_session_id: String,
    pub(crate) provider_touch_index: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) provider_event_index: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) raw_source_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) source_root: Option<String>,
    pub(crate) path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) change_kind: Option<FileChangeKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) old_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) line_count_delta: Option<i64>,
    #[serde(default)]
    pub(crate) confidence: Confidence,
    pub(crate) occurred_at: DateTime<Utc>,
    pub(crate) source_format: String,
    #[serde(default = "crate::common::json::default_metadata")]
    pub(crate) metadata: Value,
}

impl Serialize for CodexEventRow {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let item_type = self
            .provider_event
            .metadata
            .get("item_type")
            .or_else(|| self.provider_event.payload.get("item_type"))
            .and_then(Value::as_str)
            .ok_or_else(|| serde::ser::Error::custom("Codex event row has no item type"))?;
        let mut row = serializer.serialize_struct("CodexEventRow", 9)?;
        row.serialize_field("raw_ordinal", &self.raw_ordinal)?;
        row.serialize_field("occurred_at", &self.provider_event.occurred_at)?;
        row.serialize_field("event_type", &self.provider_event.event_type)?;
        row.serialize_field("role", &self.provider_event.role)?;
        row.serialize_field("normalized_body", &self.provider_event.payload)?;
        row.serialize_field("normalized_body_hash", &self.normalized_body_hash)?;
        row.serialize_field("item_type", item_type)?;
        row.serialize_field("provider_event", &self.provider_event)?;
        row.serialize_field("file_touches", &self.file_touches)?;
        row.end()
    }
}

impl CodexEventRow {
    pub(crate) fn mutation_units(&self) -> usize {
        1_usize
            .saturating_add(usize::from(
                self.provider_event.event_type == EventType::CommandOutput,
            ))
            .saturating_add(self.file_touches.len())
    }
}

pub(super) fn build_event_row(
    raw_ordinal: u64,
    kind: CodexRetainedKind,
    retained: &CodexDecodedRecord,
) -> Option<CodexEventRow> {
    let built = match kind {
        CodexRetainedKind::Message => build_message(&retained.payload),
        CodexRetainedKind::Reasoning => build_reasoning(&retained.payload),
        CodexRetainedKind::Compacted => build_compacted(&retained.payload),
        CodexRetainedKind::ToolCall => build_tool_call(&retained.payload),
    }?;
    let line_number = raw_ordinal
        .checked_add(1)
        .and_then(|line| usize::try_from(line).ok())?;
    let provider_event = codex_provider_event(
        line_number,
        retained.occurred_at,
        built.event_type,
        built.role,
        built.body.clone(),
        json!({
            "source": "codex_session",
            "source_format": CODEX_SESSION_SOURCE_FORMAT,
            "line": line_number,
            "item_type": built.item_type,
            "tool": built.body.get("tool").and_then(Value::as_str),
        }),
    );
    let normalized_body_hash = compute_payload_hash(&provider_event.payload).ok()?;
    Some(CodexEventRow {
        raw_ordinal,
        normalized_body_hash,
        provider_event,
        file_touches: Vec::new(),
    })
}

pub(super) fn tool_context_from_row(row: &CodexEventRow) -> Option<(String, CodexToolCallContext)> {
    (row.provider_event.event_type == EventType::ToolCall).then_some(())?;
    let call_id = row
        .provider_event
        .payload
        .get("call_id")
        .and_then(Value::as_str)?
        .to_owned();
    let tool_name = row
        .provider_event
        .payload
        .get("tool")
        .and_then(Value::as_str)
        .unwrap_or("tool")
        .to_owned();
    let command_preview = row
        .provider_event
        .payload
        .get("command")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let arguments_preview = row
        .provider_event
        .payload
        .get("arguments_preview")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Some((
        call_id,
        CodexToolCallContext {
            tool_name,
            command_preview,
            arguments_preview,
        },
    ))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn build_sparse_output_row(
    raw_ordinal: u64,
    occurred_at: DateTime<Utc>,
    result_kind: CodexResultKind,
    call_id: Option<&str>,
    context: Option<&CodexToolCallContext>,
    outcome: &OutputOutcomeMetadata,
    output_bytes: Option<usize>,
) -> Option<CodexEventRow> {
    let item_type = result_kind.item_type();
    let fallback_tool_name = context
        .map(|context| context.tool_name.as_str())
        .unwrap_or(item_type);
    let line_number = raw_ordinal
        .checked_add(1)
        .and_then(|line| usize::try_from(line).ok())?;
    let provider_event = codex_sparse_tool_output_event(
        item_type,
        fallback_tool_name,
        call_id,
        line_number,
        occurred_at,
        context,
        outcome,
        output_bytes,
    )?;
    let normalized_body_hash = compute_payload_hash(&provider_event.payload).ok()?;
    Some(CodexEventRow {
        raw_ordinal,
        normalized_body_hash,
        provider_event,
        file_touches: Vec::new(),
    })
}

struct BuiltBody {
    event_type: EventType,
    role: Option<EventRole>,
    body: Value,
    item_type: String,
}

fn build_message(payload: &Value) -> Option<BuiltBody> {
    let (role, body) = codex_message_body(payload)?;
    Some(BuiltBody {
        event_type: EventType::Message,
        role: Some(role),
        body,
        item_type: "message".to_owned(),
    })
}

fn build_reasoning(payload: &Value) -> Option<BuiltBody> {
    let summary = payload
        .get("summary")
        .and_then(codex_content_text)
        .or_else(|| {
            payload
                .get("summary_text")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })?;
    let (summary, truncated) = codex_local_preview(&summary, PROVIDER_MAX_TEXT_CHARS);
    Some(BuiltBody {
        event_type: EventType::Summary,
        role: Some(EventRole::Assistant),
        body: json!({
            "item_type": "reasoning",
            "summary": summary,
            "text": summary,
            "truncated": truncated,
            "encrypted_content_present": payload.get("encrypted_content").is_some(),
        }),
        item_type: "reasoning".to_owned(),
    })
}

fn build_compacted(payload: &Value) -> Option<BuiltBody> {
    let text = codex_content_text(payload)?;
    let (text, truncated) = codex_local_preview(&text, PROVIDER_MAX_TEXT_CHARS);
    Some(BuiltBody {
        event_type: EventType::Summary,
        role: Some(EventRole::System),
        body: json!({
            "entry_type": "compacted",
            "text": text,
            "truncated": truncated,
        }),
        item_type: "compacted".to_owned(),
    })
}

fn build_tool_call(payload: &Value) -> Option<BuiltBody> {
    let item_type = payload.get("type").and_then(Value::as_str)?;
    let tool_name = codex_tool_name(payload, item_type);
    let call_id = payload.get("call_id").and_then(Value::as_str);
    let arguments = payload
        .get("arguments")
        .or_else(|| payload.get("input"))
        .or_else(|| payload.get("action"))
        .or_else(|| payload.get("execution"));
    let command_preview = codex_command_preview(&tool_name, arguments);
    let (arguments_preview, arguments_truncated, raw_arguments_retained) = arguments
        .map(codex_tool_arguments_preview)
        .unwrap_or_else(|| (String::new(), false, false));
    let text = command_preview
        .as_deref()
        .map(|command| format!("{tool_name}: {command}"))
        .unwrap_or_else(|| {
            if arguments_preview.is_empty() {
                format!("{tool_name} tool call")
            } else {
                format!("{tool_name}: {arguments_preview}")
            }
        });
    let (text, text_truncated) = codex_local_preview(&text, PROVIDER_MAX_PREVIEW_CHARS);
    Some(BuiltBody {
        event_type: EventType::ToolCall,
        role: Some(EventRole::Assistant),
        body: json!({
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
        item_type: item_type.to_owned(),
    })
}
