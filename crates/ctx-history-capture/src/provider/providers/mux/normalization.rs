use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, ContentRef, EventType, Fidelity, ProviderCaptureEnvelope,
    ProviderEventEnvelope, ProviderSourceTrust,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::provider::normalization::{
    native_event, native_provider_capture, provider_capped_json_value, provider_local_preview,
    provider_role, provider_value_text, NativeEventDraft, NativeSessionDraft,
};
use crate::{ProviderAdapterContext, MUX_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS};

use super::metadata::{mux_string_pointer, mux_value_timestamp};
use super::source::MuxSessionSource;

#[derive(Debug, Clone)]
pub(super) struct MuxMessageRow {
    pub(super) line_number: usize,
    pub(super) source_path: PathBuf,
    pub(super) value: Value,
    pub(super) is_partial: bool,
}

pub(super) struct MuxCaptureDraft<'a> {
    pub(super) provider_session_id: String,
    pub(super) parent_provider_session_id: Option<String>,
    pub(super) root_provider_session_id: Option<String>,
    pub(super) agent_type: AgentType,
    pub(super) role_hint: String,
    pub(super) is_primary: bool,
    pub(super) started_at: DateTime<Utc>,
    pub(super) ended_at: Option<DateTime<Utc>>,
    pub(super) cwd: Option<String>,
    pub(super) model: Option<String>,
    pub(super) metadata: &'a Value,
    pub(super) message_count: usize,
    pub(super) source: &'a MuxSessionSource,
    pub(super) raw_source_path: &'a Path,
    pub(super) event: Option<ProviderEventEnvelope>,
}

pub(super) struct MuxProjectedEvent {
    pub(super) event: ProviderEventEnvelope,
    pub(super) result_content_ref: Option<ContentRef>,
}

pub(super) fn mux_capture(
    draft: MuxCaptureDraft<'_>,
    context: &ProviderAdapterContext,
) -> ProviderCaptureEnvelope {
    let primary_path = draft.raw_source_path;
    native_provider_capture(
        NativeSessionDraft {
            provider: CaptureProvider::Mux,
            source_format: MUX_SOURCE_FORMAT,
            provider_session_id: draft.provider_session_id.clone(),
            parent_provider_session_id: draft.parent_provider_session_id.clone(),
            root_provider_session_id: draft.root_provider_session_id,
            external_agent_id: mux_string_pointer(draft.metadata, &["/agentId", "/agent_id"]),
            agent_type: draft.agent_type,
            role_hint: Some(draft.role_hint),
            is_primary: draft.is_primary,
            started_at: draft.started_at,
            ended_at: draft.ended_at,
            cwd: draft.cwd,
            fidelity: Fidelity::Imported,
            raw_source_path: primary_path.display().to_string(),
            trust: ProviderSourceTrust::ProviderNative,
            source_metadata: json!({
                "adapter": MUX_SOURCE_FORMAT,
                "source_path": primary_path.display().to_string(),
                "chat_path": draft.source.chat_path.as_ref().map(|path| path.display().to_string()),
                "partial_path": draft.source.partial_path.as_ref().map(|path| path.display().to_string()),
                "metadata_path": draft.source.metadata_path.as_ref().map(|path| path.display().to_string()),
                "session_dir": draft.source.session_dir.display().to_string(),
            }),
            session_metadata: json!({
                "source_format": MUX_SOURCE_FORMAT,
                "provider": CaptureProvider::Mux.as_str(),
                "workspace_id": draft.provider_session_id,
                "parent_workspace_id": draft.parent_provider_session_id,
                "model": draft.model,
                "message_count": draft.message_count,
                "has_partial": draft.source.partial_path.is_some(),
                "metadata": provider_capped_json_value(draft.metadata, PROVIDER_MAX_PREVIEW_CHARS),
            }),
        },
        context,
        draft.event,
    )
}

pub(super) fn mux_event(
    provider_session_id: &str,
    event_index: u64,
    row: &MuxMessageRow,
    occurred_at: DateTime<Utc>,
    model: Option<&str>,
) -> MuxProjectedEvent {
    let role = row
        .value
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let event_type = mux_event_type(&row.value);
    let model_value = model
        .map(str::to_owned)
        .or_else(|| mux_message_model(&row.value));
    let result_content_ref = matches!(event_type, EventType::ToolOutput | EventType::CommandOutput)
        .then(|| mux_result_content(&row.value))
        .flatten()
        .filter(|content| content.len() <= crate::complete_content::COMPLETE_CONTENT_MAX_BODY_BYTES)
        .and_then(|content| ContentRef::from_bytes(content.as_bytes()));
    let event = native_event(NativeEventDraft {
        provider: CaptureProvider::Mux,
        source_format: MUX_SOURCE_FORMAT,
        provider_session_id: provider_session_id.to_owned(),
        provider_event_index: event_index,
        provider_event_hash: Some(mux_event_id(
            &row.value,
            row.line_number,
            role,
            row.is_partial,
        )),
        cursor: format!("{}:line:{}", row.source_path.display(), row.line_number),
        event_type,
        role: Some(provider_role(Some(role))),
        occurred_at,
        text: mux_event_text(&row.value, event_type),
        body: row.value.clone(),
        metadata: json!({
            "source": MUX_SOURCE_FORMAT,
            "source_format": MUX_SOURCE_FORMAT,
            "line": row.line_number,
            "is_partial": row.is_partial,
            "role": role,
            "message_id": row.value.get("id").and_then(Value::as_str),
            "workspace_id": row.value.get("workspaceId").and_then(Value::as_str),
            "history_sequence": mux_history_sequence(&row.value),
            "model": model_value,
            "usage": row.value.pointer("/metadata/usage").map(|usage| provider_capped_json_value(usage, PROVIDER_MAX_PREVIEW_CHARS)),
            "provider_metadata": row.value.pointer("/metadata/providerMetadata").map(|metadata| provider_capped_json_value(metadata, PROVIDER_MAX_PREVIEW_CHARS)),
            "mux_metadata": row.value.pointer("/metadata/muxMetadata").map(|metadata| provider_capped_json_value(metadata, PROVIDER_MAX_PREVIEW_CHARS)),
            "partial": row.value.pointer("/metadata/partial").and_then(Value::as_bool),
        }),
    });
    MuxProjectedEvent {
        event,
        result_content_ref,
    }
}

pub(crate) fn mux_event_type(value: &Value) -> EventType {
    if mux_is_summary_message(value) {
        return EventType::Summary;
    }
    if value.get("role").and_then(Value::as_str) == Some("system") {
        return EventType::Notice;
    }
    let mut saw_tool_call = false;
    if let Some(parts) = value.get("parts").and_then(Value::as_array) {
        for part in parts {
            if part.get("type").and_then(Value::as_str) != Some("dynamic-tool") {
                continue;
            }
            let state = part.get("state").and_then(Value::as_str);
            if matches!(state, Some("output-available" | "output-redacted"))
                || part.get("output").is_some()
            {
                return EventType::ToolOutput;
            }
            saw_tool_call = true;
        }
    }
    if saw_tool_call {
        EventType::ToolCall
    } else {
        EventType::Message
    }
}

fn mux_is_summary_message(value: &Value) -> bool {
    value
        .pointer("/metadata/compacted")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || value
            .pointer("/metadata/compactionBoundary")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        || value.pointer("/metadata/contextBoundaryKind").is_some()
        || value
            .pointer("/metadata/muxMetadata/type")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind.contains("compaction") || kind.contains("summary"))
}

pub(crate) fn mux_event_text(value: &Value, event_type: EventType) -> String {
    let mut rendered = Vec::new();
    if let Some(parts) = value.get("parts").and_then(Value::as_array) {
        for part in parts {
            match part.get("type").and_then(Value::as_str) {
                Some("text" | "reasoning") => {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        rendered.push(text.to_owned());
                    }
                }
                Some("dynamic-tool") => rendered.push(mux_tool_part_text(part)),
                Some("file") => {
                    if let Some(text) = mux_file_part_text(part) {
                        rendered.push(text);
                    }
                }
                _ => {
                    if let Some(text) = part.get("text").and_then(Value::as_str) {
                        rendered.push(text.to_owned());
                    }
                }
            }
        }
    }
    if !rendered.is_empty() {
        return rendered.join("\n");
    }
    if let Some(text) = value
        .get("content")
        .or_else(|| value.get("message"))
        .and_then(provider_value_text)
    {
        return text;
    }
    match event_type {
        EventType::ToolOutput => "Mux tool output".to_owned(),
        EventType::ToolCall => "Mux tool call".to_owned(),
        EventType::Summary => "Mux summary".to_owned(),
        EventType::Notice => "Mux notice".to_owned(),
        _ => "Mux message".to_owned(),
    }
}

fn mux_tool_part_text(part: &Value) -> String {
    let name = part
        .get("toolName")
        .or_else(|| part.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("tool");
    let state = part.get("state").and_then(Value::as_str);
    let prefix = if matches!(state, Some("output-available" | "output-redacted"))
        || part.get("output").is_some()
    {
        "tool output"
    } else {
        "tool call"
    };
    let mut text = format!("{prefix}: {name}");
    if let Some(input) = part.get("input") {
        text.push('\n');
        text.push_str("input: ");
        text.push_str(&mux_value_preview(input));
    }
    if let Some(output) = part.get("output") {
        text.push('\n');
        text.push_str("output: ");
        text.push_str(&mux_value_preview(output));
    }
    if let Some(nested) = part.get("nestedCalls").and_then(Value::as_array) {
        let names = nested
            .iter()
            .filter_map(|call| {
                call.get("toolName")
                    .or_else(|| call.get("name"))
                    .and_then(Value::as_str)
            })
            .collect::<Vec<_>>();
        if !names.is_empty() {
            text.push('\n');
            text.push_str("nested tools: ");
            text.push_str(&names.join(", "));
        }
    }
    text
}

fn mux_file_part_text(part: &Value) -> Option<String> {
    let label = part
        .get("filename")
        .or_else(|| part.get("name"))
        .or_else(|| part.get("mediaType"))
        .or_else(|| part.get("mimeType"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| {
            part.get("url")
                .and_then(Value::as_str)
                .filter(|url| !url.starts_with("data:") && url.len() < 256)
                .map(str::to_owned)
        })?;
    Some(format!("file: {label}"))
}

fn mux_value_preview(value: &Value) -> String {
    let raw = provider_value_text(value)
        .or_else(|| serde_json::to_string(value).ok())
        .unwrap_or_else(|| value.to_string());
    provider_local_preview(&raw, PROVIDER_MAX_PREVIEW_CHARS).0
}

pub(crate) fn mux_event_id(
    value: &Value,
    line_number: usize,
    role: &str,
    is_partial: bool,
) -> String {
    let prefix = if is_partial { "partial:" } else { "" };
    value
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(|id| format!("{prefix}{id}"))
        .or_else(|| {
            mux_history_sequence(value)
                .map(|sequence| format!("{prefix}historySequence:{sequence}"))
        })
        .unwrap_or_else(|| format!("{prefix}{role}:line-{line_number}"))
}

/// Exact normalized result body for a Mux dynamic-tool record.
///
/// A record containing any redacted output is deliberately ineligible. A
/// single result preserves its string bytes or canonical JSON serialization;
/// multiple results use a JSON array so their boundaries cannot be confused.
pub(crate) fn mux_result_content(value: &Value) -> Option<String> {
    let parts = value.get("parts")?.as_array()?;
    let mut outputs = Vec::new();
    for part in parts {
        if part.get("type").and_then(Value::as_str) != Some("dynamic-tool") {
            continue;
        }
        if part.get("state").and_then(Value::as_str) == Some("output-redacted") {
            return None;
        }
        let Some(output) = part.get("output").filter(|output| !output.is_null()) else {
            continue;
        };
        outputs.push(output);
    }
    match outputs.as_slice() {
        [] => None,
        [Value::String(text)] => Some((*text).clone()),
        [output] => serde_json::to_string(output).ok(),
        outputs => serde_json::to_string(outputs).ok(),
    }
}

pub(super) fn mux_partial_event_index(bytes: &[u8]) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(b"ctx-mux-partial-event-index-sha256-v1\0");
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut prefix = [0_u8; 8];
    prefix.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(prefix) | (1_u64 << 63)
}

pub(super) fn mux_history_sequence(value: &Value) -> Option<i64> {
    match value.pointer("/metadata/historySequence") {
        Some(Value::Number(number)) => number
            .as_i64()
            .or_else(|| number.as_u64().and_then(|value| i64::try_from(value).ok())),
        Some(Value::String(raw)) => raw.parse::<i64>().ok(),
        _ => None,
    }
}

pub(super) fn mux_message_model(value: &Value) -> Option<String> {
    mux_string_pointer(value, &["/metadata/model", "/model"])
}

pub(super) fn mux_message_timestamp_opt(value: &Value) -> Option<DateTime<Utc>> {
    value
        .get("createdAt")
        .and_then(mux_value_timestamp)
        .or_else(|| {
            value
                .pointer("/metadata/timestamp")
                .and_then(mux_value_timestamp)
        })
        .or_else(|| {
            value
                .get("parts")
                .and_then(Value::as_array)
                .and_then(|parts| {
                    parts
                        .iter()
                        .find_map(|part| part.get("timestamp").and_then(mux_value_timestamp))
                })
        })
}
