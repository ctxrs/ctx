use chrono::{DateTime, Utc};
use ctx_history_core::{
    compute_payload_hash, CaptureProvider, Confidence, ContentRef, EventRole, EventType,
    FileChangeKind,
};
use serde::{ser::SerializeStruct, Deserialize, Serialize, Serializer};
use serde_json::{json, Value};

use super::record::{CodexDecodedRecord, CodexResultKind, CodexRetainedKind};
use crate::complete_content::{
    attach_verified_content_locator, jsonl::JSONL_COMPLETE_CONTENT_LOCATOR_KIND,
    verified_content_profile, CompleteContentBodyDigest, CompleteContentSourceFamily,
    VerifiedContentLocatorV1, VerifiedContentRole, COMPLETE_CONTENT_MAX_BODY_BYTES,
};
use crate::provider::codex::events::{
    codex_command_preview, codex_content_text, codex_local_preview, codex_message_body,
    codex_provider_event, codex_sparse_tool_output_event, codex_tool_arguments_preview,
    codex_tool_name, CodexNativeEvent, CodexToolCallContext,
};
use crate::{
    CaptureError, OutputOutcomeMetadata, Result as CaptureResult, CODEX_SESSION_SOURCE_FORMAT,
    PROVIDER_MAX_PREVIEW_CHARS, PROVIDER_MAX_TEXT_CHARS,
};

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

pub(super) enum CodexRetainedNonMaterialized {
    ValidUnmaterializable,
    Malformed(&'static str),
}

pub(super) type CodexRetainedProjection =
    std::result::Result<CodexEventRow, CodexRetainedNonMaterialized>;

pub(super) fn build_event_row(
    raw_ordinal: u64,
    kind: CodexRetainedKind,
    retained: &CodexDecodedRecord,
) -> CaptureResult<CodexRetainedProjection> {
    let built = match kind {
        CodexRetainedKind::Message => build_message(&retained.payload),
        CodexRetainedKind::Reasoning => build_reasoning(&retained.payload),
        CodexRetainedKind::Compacted => build_compacted(&retained.payload),
        CodexRetainedKind::ToolCall => build_tool_call(&retained.payload),
    };
    let built = match built {
        BuiltBodyProjection::Materialized(built) => built,
        BuiltBodyProjection::ValidUnmaterializable => {
            return Ok(Err(CodexRetainedNonMaterialized::ValidUnmaterializable));
        }
        BuiltBodyProjection::Malformed(reason) => {
            return Ok(Err(CodexRetainedNonMaterialized::Malformed(reason)));
        }
    };
    let line_number = raw_ordinal
        .checked_add(1)
        .and_then(|line| usize::try_from(line).ok())
        .ok_or(CaptureError::SystemInvariant(
            "Codex NativePath raw ordinal exceeds platform limits",
        ))?;
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
            "source_record_ordinal": raw_ordinal,
            "source_record_subrecord_index": 0,
        }),
    );
    let normalized_body_hash = compute_payload_hash(&provider_event.payload)?;
    Ok(Ok(CodexEventRow {
        raw_ordinal,
        normalized_body_hash,
        provider_event,
        file_touches: Vec::new(),
    }))
}

pub(super) fn attach_complete_message_locator(
    row: &mut CodexEventRow,
    retained: &CodexDecodedRecord,
    record_bytes: &[u8],
    byte_start: u64,
    byte_end_exclusive: u64,
) -> CaptureResult<()> {
    if row.provider_event.event_type != EventType::Message
        || row
            .provider_event
            .payload
            .get("truncated")
            .and_then(Value::as_bool)
            != Some(true)
    {
        return Ok(());
    }
    let text = retained
        .payload
        .get("content")
        .and_then(codex_content_text)
        .ok_or(CaptureError::SystemInvariant(
            "truncated Codex message has no complete provider body",
        ))?;
    if text.chars().count() <= PROVIDER_MAX_TEXT_CHARS {
        return Err(CaptureError::SystemInvariant(
            "truncated Codex message does not exceed the indexed body limit",
        ));
    }
    if text.len() > COMPLETE_CONTENT_MAX_BODY_BYTES {
        return Ok(());
    }
    if byte_start >= byte_end_exclusive {
        return Err(CaptureError::SystemInvariant(
            "Codex verified-content locator range is empty",
        ));
    }
    let profile = verified_content_profile(
        CaptureProvider::Codex,
        CODEX_SESSION_SOURCE_FORMAT,
        CompleteContentSourceFamily::Jsonl,
        VerifiedContentRole::MessageBody,
    )
    .ok_or(CaptureError::SystemInvariant(
        "Codex JSONL complete-content route has no verified profile",
    ))?;
    let content_ref = ContentRef::from_bytes(text.as_bytes()).ok_or(
        CaptureError::SystemInvariant("Codex complete-content body length exceeds u64"),
    )?;
    let mut range = [0_u8; 16];
    range[..8].copy_from_slice(&byte_start.to_be_bytes());
    range[8..].copy_from_slice(&byte_end_exclusive.to_be_bytes());
    let line_number = row
        .raw_ordinal
        .checked_add(1)
        .ok_or(CaptureError::SystemInvariant(
            "Codex verified-content line number exceeds u64",
        ))?;
    let locator = VerifiedContentLocatorV1::new(
        VerifiedContentRole::MessageBody,
        profile,
        content_ref,
        CompleteContentSourceFamily::Jsonl,
        JSONL_COMPLETE_CONTENT_LOCATOR_KIND,
        &range,
        format!("line-{line_number}"),
        CompleteContentBodyDigest::from_bytes(record_bytes),
    )
    .ok_or(CaptureError::SystemInvariant(
        "Codex verified-content locator exceeds its bounded schema",
    ))?;
    attach_verified_content_locator(&mut row.provider_event.metadata, locator).ok_or(
        CaptureError::SystemInvariant("Codex verified-content locator collection is malformed"),
    )
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

enum BuiltBodyProjection {
    Materialized(BuiltBody),
    ValidUnmaterializable,
    Malformed(&'static str),
}

fn build_message(payload: &Value) -> BuiltBodyProjection {
    let Some((role, body)) = codex_message_body(payload) else {
        return BuiltBodyProjection::Malformed("malformed retained Codex message");
    };
    BuiltBodyProjection::Materialized(BuiltBody {
        event_type: EventType::Message,
        role: Some(role),
        body,
        item_type: "message".to_owned(),
    })
}

fn build_reasoning(payload: &Value) -> BuiltBodyProjection {
    let summary = payload
        .get("summary")
        .and_then(codex_content_text)
        .or_else(|| {
            payload
                .get("summary_text")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    let Some(summary) = summary else {
        return if is_encrypted_reasoning_without_plaintext(payload) {
            BuiltBodyProjection::ValidUnmaterializable
        } else {
            BuiltBodyProjection::Malformed("malformed retained Codex reasoning")
        };
    };
    let (summary, truncated) = codex_local_preview(&summary, PROVIDER_MAX_TEXT_CHARS);
    BuiltBodyProjection::Materialized(BuiltBody {
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

fn is_encrypted_reasoning_without_plaintext(payload: &Value) -> bool {
    let Some(object) = payload.as_object() else {
        return false;
    };
    if object.get("type").and_then(Value::as_str) != Some("reasoning")
        || !object
            .get("encrypted_content")
            .and_then(Value::as_str)
            .is_some_and(|content| !content.is_empty())
    {
        return false;
    }
    let empty_summary = match object.get("summary") {
        None | Some(Value::Null) => true,
        Some(Value::Array(parts)) => parts.is_empty(),
        _ => false,
    };
    let empty_summary_text = matches!(object.get("summary_text"), None | Some(Value::Null));
    empty_summary && empty_summary_text
}

fn build_compacted(payload: &Value) -> BuiltBodyProjection {
    let Some(text) = codex_content_text(payload) else {
        return if is_source_only_compacted(payload) {
            BuiltBodyProjection::ValidUnmaterializable
        } else {
            BuiltBodyProjection::Malformed("malformed retained Codex compacted record")
        };
    };
    let (text, truncated) = codex_local_preview(&text, PROVIDER_MAX_TEXT_CHARS);
    BuiltBodyProjection::Materialized(BuiltBody {
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

fn is_source_only_compacted(payload: &Value) -> bool {
    let Some(object) = payload.as_object() else {
        return false;
    };
    object.get("message").is_some_and(Value::is_string)
        && object
            .get("replacement_history")
            .is_some_and(Value::is_array)
}

fn build_tool_call(payload: &Value) -> BuiltBodyProjection {
    let Some(item_type) = payload.get("type").and_then(Value::as_str) else {
        return BuiltBodyProjection::Malformed("malformed retained Codex tool call");
    };
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
    BuiltBodyProjection::Materialized(BuiltBody {
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
