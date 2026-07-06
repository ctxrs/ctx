use std::{
    fs::{self},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventRole, EventType, Fidelity, ProviderCaptureEnvelope,
    ProviderCursorCheckpoint, ProviderCursorRange, ProviderEventEnvelope, ProviderRawRetention,
    ProviderRedactionBoundary, ProviderSessionEnvelope, ProviderSourceEnvelope,
    ProviderSourceTrust, RedactionState, SessionStatus, PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
};
use rusqlite::{limits::Limit, Connection, OpenFlags};
use serde_json::{json, Value};

use crate::common::io::{ensure_regular_provider_transcript_file, read_text_file_limited};
use crate::common::time::parse_rfc3339_utc;
use crate::provider::importer::provider_cursor_stream;
use crate::{
    fnv1a64, CaptureError, ProviderAdapterContext, Result, MAX_PROVIDER_JSONL_LINE_BYTES,
    MAX_PROVIDER_SQLITE_VALUE_BYTES, PROVIDER_MAX_PREVIEW_CHARS, PROVIDER_MAX_TEXT_CHARS,
};

pub(crate) fn provider_capped_json(value: &Value, max_chars: usize) -> Value {
    match value {
        Value::Null => Value::Null,
        Value::String(text) => {
            let (text, truncated) = provider_local_preview(text, max_chars);
            json!({ "text": text, "truncated": truncated })
        }
        _ => {
            let rendered = serde_json::to_string(value).unwrap_or_else(|_| value.to_string());
            let (json_text, truncated) = provider_local_preview(&rendered, max_chars);
            json!({ "json": json_text, "truncated": truncated })
        }
    }
}

pub(crate) fn provider_capped_json_value(value: &Value, max_string_chars: usize) -> Value {
    match value {
        Value::String(text) => {
            let (text, truncated) = provider_local_preview(text, max_string_chars);
            if truncated {
                json!({ "text": text, "truncated": true })
            } else {
                Value::String(text)
            }
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| provider_capped_json_value(item, max_string_chars))
                .collect(),
        ),
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        provider_capped_json_value(value, max_string_chars),
                    )
                })
                .collect(),
        ),
        _ => value.clone(),
    }
}

pub(crate) fn antigravity_tool_call_text(value: &Value) -> Option<String> {
    value.as_array().and_then(|calls| {
        let names: Vec<&str> = calls
            .iter()
            .filter_map(|call| call.get("name").and_then(Value::as_str))
            .collect();
        if names.is_empty() {
            None
        } else {
            Some(format!("tool calls: {}", names.join(", ")))
        }
    })
}

#[derive(Debug, Clone)]
pub(crate) struct OpenCodeSessionRow {
    pub(crate) id: String,
    pub(crate) parent_id: Option<String>,
    pub(crate) title: String,
    pub(crate) directory: String,
    pub(crate) model: Option<String>,
    pub(crate) agent: Option<String>,
    pub(crate) time_created: i64,
    pub(crate) time_updated: i64,
    pub(crate) tokens_input: i64,
    pub(crate) tokens_output: i64,
    pub(crate) tokens_reasoning: i64,
    pub(crate) tokens_cache_read: i64,
    pub(crate) tokens_cache_write: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct OpenCodeMessageRow {
    pub(crate) id: String,
    pub(crate) session_id: String,
    pub(crate) entry_type: String,
    pub(crate) seq: i64,
    pub(crate) time_created: i64,
    pub(crate) time_updated: i64,
    pub(crate) data: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ShelleyConversationRow {
    pub(crate) conversation_id: String,
    pub(crate) slug: Option<String>,
    pub(crate) user_initiated: bool,
    pub(crate) created_at: Option<String>,
    pub(crate) updated_at: Option<String>,
    pub(crate) cwd: Option<String>,
    pub(crate) archived: bool,
    pub(crate) parent_conversation_id: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) conversation_options: Option<String>,
    pub(crate) current_generation: Option<i64>,
    pub(crate) agent_working: bool,
    pub(crate) tags: Option<String>,
    pub(crate) is_draft: bool,
    pub(crate) draft: Option<String>,
    pub(crate) queued_messages: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ShelleyMessageRow {
    pub(crate) rowid: i64,
    pub(crate) message_id: String,
    pub(crate) conversation_id: String,
    pub(crate) sequence_id: i64,
    pub(crate) entry_type: String,
    pub(crate) llm_data: Option<String>,
    pub(crate) user_data: Option<String>,
    pub(crate) usage_data: Option<String>,
    pub(crate) created_at: Option<String>,
    pub(crate) display_data: Option<String>,
    pub(crate) excluded_from_context: bool,
    pub(crate) generation: Option<i64>,
    pub(crate) llm_api_url: Option<String>,
    pub(crate) model_name: Option<String>,
    pub(crate) forked_from_message_id: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct OpenHandsEventFile {
    pub(crate) path: PathBuf,
    pub(crate) line_number: usize,
    pub(crate) session_id: String,
    pub(crate) user_id: Option<String>,
    pub(crate) event_id: String,
    pub(crate) timestamp: DateTime<Utc>,
    pub(crate) value: Value,
}

#[derive(Clone)]
pub(crate) struct NativeSessionDraft {
    pub(crate) provider: CaptureProvider,
    pub(crate) source_format: &'static str,
    pub(crate) provider_session_id: String,
    pub(crate) parent_provider_session_id: Option<String>,
    pub(crate) root_provider_session_id: Option<String>,
    pub(crate) external_agent_id: Option<String>,
    pub(crate) agent_type: AgentType,
    pub(crate) role_hint: Option<String>,
    pub(crate) is_primary: bool,
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) ended_at: Option<DateTime<Utc>>,
    pub(crate) cwd: Option<String>,
    pub(crate) fidelity: Fidelity,
    pub(crate) raw_source_path: String,
    pub(crate) trust: ProviderSourceTrust,
    pub(crate) source_metadata: Value,
    pub(crate) session_metadata: Value,
}

pub(crate) fn native_provider_capture(
    draft: NativeSessionDraft,
    context: &ProviderAdapterContext,
    event: Option<ProviderEventEnvelope>,
) -> ProviderCaptureEnvelope {
    ProviderCaptureEnvelope {
        schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
        provider: draft.provider,
        source: ProviderSourceEnvelope {
            source_format: draft.source_format.to_owned(),
            machine_id: context.machine_id.clone(),
            observed_at: context.imported_at,
            raw_source_path: Some(draft.raw_source_path),
            source_root: context.source_root_display(),
            raw_retention: ProviderRawRetention::PathReference,
            redaction_boundary: ProviderRedactionBoundary::BeforeExport,
            trust: draft.trust,
            fidelity: draft.fidelity,
            cursor: event.as_ref().and_then(|event| {
                event.cursor.as_ref().map(|cursor| ProviderCursorRange {
                    before: None,
                    after: Some(ProviderCursorCheckpoint {
                        stream: provider_cursor_stream(draft.provider, draft.source_format),
                        cursor: cursor.clone(),
                        observed_at: event.occurred_at,
                    }),
                })
            }),
            idempotency_key: Some(format!(
                "provider-source:{}:{}:{}",
                draft.provider.as_str(),
                draft.source_format,
                draft.provider_session_id
            )),
            metadata: draft.source_metadata,
        },
        session: ProviderSessionEnvelope {
            provider_session_id: draft.provider_session_id.clone(),
            parent_provider_session_id: draft.parent_provider_session_id,
            root_provider_session_id: draft.root_provider_session_id,
            external_agent_id: draft.external_agent_id,
            agent_type: draft.agent_type,
            role_hint: draft.role_hint,
            is_primary: draft.is_primary,
            status: SessionStatus::Imported,
            started_at: draft.started_at,
            ended_at: draft.ended_at,
            cwd: draft.cwd,
            fidelity: draft.fidelity,
            idempotency_key: Some(format!(
                "provider-session:{}:{}",
                draft.provider.as_str(),
                draft.provider_session_id
            )),
            artifacts: Vec::new(),
            metadata: draft.session_metadata,
        },
        event,
    }
}

pub(crate) fn open_provider_sqlite_readonly(path: &Path) -> Result<Connection> {
    ensure_regular_provider_transcript_file(path)?;
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let value_limit = i32::try_from(MAX_PROVIDER_SQLITE_VALUE_BYTES).map_err(|_| {
        CaptureError::InvalidPayload(format!(
            "provider SQLite value byte limit is unrepresentable: {MAX_PROVIDER_SQLITE_VALUE_BYTES}"
        ))
    })?;
    conn.set_limit(Limit::SQLITE_LIMIT_LENGTH, value_limit);
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.pragma_update(None, "query_only", true)?;
    Ok(conn)
}

pub(crate) fn provider_nonnegative_i64_to_u64(value: i64, field: &'static str) -> Result<u64> {
    u64::try_from(value).map_err(|_| {
        CaptureError::InvalidPayload(format!("{field} must be nonnegative, got {value}"))
    })
}

pub(crate) fn provider_line_from_index(index: u64) -> usize {
    index.min(usize::MAX as u64) as usize
}

pub(crate) fn provider_timestamp_seconds_to_datetime(value: f64) -> Option<DateTime<Utc>> {
    if !value.is_finite() {
        return None;
    }
    let millis = if value.abs() > 1_000_000_000_000.0 {
        value.round()
    } else {
        (value * 1000.0).round()
    };
    if millis < i64::MIN as f64 || millis > i64::MAX as f64 {
        return None;
    }
    DateTime::<Utc>::from_timestamp_millis(millis as i64)
}

pub(crate) fn provider_timestamp_seconds(
    value: Option<f64>,
    fallback: DateTime<Utc>,
) -> DateTime<Utc> {
    value
        .and_then(provider_timestamp_seconds_to_datetime)
        .unwrap_or(fallback)
}

pub(crate) fn provider_required_timestamp_seconds(
    value: f64,
    field: &'static str,
) -> Result<DateTime<Utc>> {
    provider_timestamp_seconds_to_datetime(value).ok_or_else(|| {
        CaptureError::InvalidPayload(format!(
            "{field} is outside representable timestamp range: {value}"
        ))
    })
}

pub(crate) fn provider_timestamp_millis(
    value: Option<i64>,
    fallback: DateTime<Utc>,
) -> DateTime<Utc> {
    value
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        .unwrap_or(fallback)
}

pub(crate) fn provider_required_timestamp_millis(
    value: i64,
    field: &'static str,
) -> Result<DateTime<Utc>> {
    DateTime::<Utc>::from_timestamp_millis(value).ok_or_else(|| {
        CaptureError::InvalidPayload(format!(
            "{field} is outside representable timestamp range: {value}"
        ))
    })
}

pub(crate) fn provider_timestamp_value(
    value: Option<&Value>,
    fallback: DateTime<Utc>,
) -> DateTime<Utc> {
    match value {
        Some(Value::String(raw)) => parse_rfc3339_utc(raw)
            .or_else(|| {
                raw.parse::<f64>()
                    .ok()
                    .map(|ts| provider_timestamp_seconds(Some(ts), fallback))
            })
            .unwrap_or(fallback),
        Some(Value::Number(number)) => number
            .as_f64()
            .map(|ts| provider_timestamp_seconds(Some(ts), fallback))
            .unwrap_or(fallback),
        _ => fallback,
    }
}

pub(crate) fn text_id_index(seed: &str, offset: u64) -> u64 {
    offset.saturating_add(fnv1a64(seed.as_bytes()) & 0x0fff_ffff)
}

pub(crate) fn provider_json_text(raw: &str) -> Value {
    serde_json::from_str::<Value>(raw).unwrap_or_else(|_| Value::String(raw.to_owned()))
}

pub(crate) fn hermes_decode_content(raw: Option<&str>) -> Value {
    let Some(raw) = raw else {
        return Value::Null;
    };
    if let Some(json) = raw.strip_prefix("\0json:") {
        return provider_json_text(json);
    }
    Value::String(raw.to_owned())
}

pub(crate) struct NativeEventDraft {
    pub(crate) provider: CaptureProvider,
    pub(crate) source_format: &'static str,
    pub(crate) provider_session_id: String,
    pub(crate) provider_event_index: u64,
    pub(crate) provider_event_hash: Option<String>,
    pub(crate) cursor: String,
    pub(crate) event_type: EventType,
    pub(crate) role: Option<EventRole>,
    pub(crate) occurred_at: DateTime<Utc>,
    pub(crate) text: String,
    pub(crate) body: Value,
    pub(crate) metadata: Value,
}

pub(crate) fn native_event(draft: NativeEventDraft) -> ProviderEventEnvelope {
    let (text, truncated) = provider_local_preview(&draft.text, PROVIDER_MAX_TEXT_CHARS);
    ProviderEventEnvelope {
        provider_event_index: draft.provider_event_index,
        provider_event_hash: draft.provider_event_hash,
        cursor: Some(draft.cursor),
        event_type: draft.event_type,
        role: draft.role,
        occurred_at: draft.occurred_at,
        fidelity: Fidelity::Imported,
        redaction_state: RedactionState::LocalPreview,
        idempotency_key: Some(format!(
            "provider-event:{}:{}:{}",
            draft.provider.as_str(),
            draft.provider_session_id,
            draft.provider_event_index
        )),
        artifacts: Vec::new(),
        payload: json!({
            "text": text,
            "truncated": truncated,
            "source_format": draft.source_format,
            "body": provider_capped_json(&draft.body, PROVIDER_MAX_PREVIEW_CHARS),
        }),
        metadata: draft.metadata,
    }
}

pub(crate) fn provider_value_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => {
            let mut parts = Vec::new();
            for block in blocks {
                if let Some(text) = block
                    .get("text")
                    .or_else(|| block.get("content"))
                    .or_else(|| block.get("output"))
                    .or_else(|| block.get("summary"))
                    .and_then(Value::as_str)
                {
                    parts.push(text.to_owned());
                    continue;
                }
                if let Some(kind) = block.get("type").and_then(Value::as_str) {
                    if matches!(
                        kind,
                        "tool_use" | "tool" | "toolCall" | "function_call" | "agent"
                    ) {
                        let name = block
                            .get("name")
                            .or_else(|| block.get("tool"))
                            .and_then(Value::as_str)
                            .unwrap_or("tool");
                        parts.push(format!("tool call: {name}"));
                    } else if kind == "tool_result" {
                        parts.push("tool result".to_owned());
                    }
                }
            }
            (!parts.is_empty()).then(|| parts.join("\n"))
        }
        Value::Object(_) => serde_json::to_string(value).ok(),
        Value::Number(_) | Value::Bool(_) => Some(value.to_string()),
        Value::Null => None,
    }
}

pub(crate) fn provider_role(value: Option<&str>) -> EventRole {
    match value {
        Some("user") => EventRole::User,
        Some("assistant") => EventRole::Assistant,
        Some("system" | "developer") => EventRole::System,
        Some("tool" | "toolResult" | "bashExecution") => EventRole::Tool,
        _ => EventRole::Unknown,
    }
}

pub(crate) fn capped_text(value: &str, max_chars: usize) -> (String, bool) {
    let mut out = String::new();
    let mut truncated = false;
    for (index, ch) in value.chars().enumerate() {
        if index >= max_chars {
            truncated = true;
            break;
        }
        out.push(ch);
    }
    (out, truncated)
}

pub(crate) fn provider_local_preview(value: &str, max_chars: usize) -> (String, bool) {
    capped_text(value, max_chars)
}

pub(crate) fn parse_json_object_string(value: Option<&str>) -> Value {
    value
        .and_then(|value| serde_json::from_str::<Value>(value).ok())
        .unwrap_or(Value::Null)
}

pub(crate) fn sqlite_bool(value: Option<i64>) -> bool {
    value.unwrap_or(0) != 0
}

pub(crate) fn provider_optional_regular_file(path: &Path) -> Result<Option<PathBuf>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            ensure_regular_provider_transcript_file(path)?;
            Ok(Some(path.to_path_buf()))
        }
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(CaptureError::InvalidProviderTranscriptPath {
                path: path.to_path_buf(),
                reason: "symlinked provider transcript files are rejected",
            })
        }
        Ok(_) => Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "provider sidecar paths must be regular files",
        }),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

pub(crate) fn read_provider_json_file(path: &Path, label: &str) -> Result<Value> {
    let raw = read_text_file_limited(path, MAX_PROVIDER_JSONL_LINE_BYTES, label)?;
    let value: Value = serde_json::from_str(&raw)?;
    if !value.is_object() {
        return Err(CaptureError::InvalidPayload(format!(
            "{label} must contain a JSON object"
        )));
    }
    Ok(value)
}

pub(crate) fn provider_string_field(value: &Value, fields: &[&str]) -> Option<String> {
    fields.iter().find_map(|field| {
        value
            .get(*field)
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .map(str::to_owned)
    })
}

pub(crate) fn provider_timestamp_from_fields(
    value: &Value,
    fields: &[&str],
) -> Option<DateTime<Utc>> {
    fields.iter().find_map(|field| {
        let raw = value.get(*field)?;
        match raw {
            Value::String(text) => parse_rfc3339_utc(text).or_else(|| {
                text.parse::<f64>()
                    .ok()
                    .and_then(provider_timestamp_seconds_to_datetime)
            }),
            Value::Number(number) => number
                .as_f64()
                .and_then(provider_timestamp_seconds_to_datetime),
            _ => None,
        }
    })
}

pub(crate) fn provider_message_id(value: &Value, fallback_index: u64) -> String {
    value
        .get("id")
        .or_else(|| value.get("message_id"))
        .or_else(|| value.get("messageId"))
        .or_else(|| value.get("request_id"))
        .or_else(|| value.get("requestId"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("message-{fallback_index}"))
}

pub(crate) fn provider_role_from_message(value: &Value, role_text: Option<&str>) -> EventRole {
    let role = role_text.or_else(|| value.get("kind").and_then(Value::as_str));
    match role {
        Some("user" | "human" | "user_prompt" | "user-prompt") => EventRole::User,
        Some("assistant" | "agent" | "ai" | "model") => EventRole::Assistant,
        Some("system" | "developer" | "system_prompt" | "system-prompt") => EventRole::System,
        Some("tool" | "tool_result" | "tool-result" | "tool_use_result") => EventRole::Tool,
        _ => EventRole::Unknown,
    }
}

pub(crate) fn provider_block_event_type(value: &Value, role_text: Option<&str>) -> EventType {
    let role = role_text.unwrap_or_default();
    if role.contains("tool_result")
        || role.contains("tool-result")
        || provider_message_has_part_kind(value, &["tool_result", "tool-result"])
    {
        EventType::ToolOutput
    } else if role.contains("tool_use")
        || role.contains("tool-use")
        || provider_message_has_part_kind(
            value,
            &["tool_use", "tool-use", "tool-call", "tool_call"],
        )
    {
        EventType::ToolCall
    } else if matches!(
        role,
        "system" | "developer" | "system_prompt" | "system-prompt"
    ) {
        EventType::Notice
    } else {
        EventType::Message
    }
}

pub(crate) fn provider_message_has_part_kind(value: &Value, kinds: &[&str]) -> bool {
    provider_message_parts(value)
        .map(|parts| {
            parts.iter().any(|part| {
                part.get("type")
                    .or_else(|| part.get("kind"))
                    .and_then(Value::as_str)
                    .is_some_and(|kind| kinds.contains(&kind))
            })
        })
        .unwrap_or(false)
}

pub(crate) fn provider_block_text(value: &Value) -> Option<String> {
    for key in [
        "text", "content", "message", "prompt", "response", "output", "summary",
    ] {
        if let Some(text) = value.get(key).and_then(provider_value_text) {
            if !text.trim().is_empty() {
                return Some(text);
            }
        }
    }
    let parts = provider_message_parts(value)?;
    let mut rendered = Vec::new();
    for part in parts {
        if let Some(text) = provider_part_text(part) {
            rendered.push(text);
        }
    }
    (!rendered.is_empty()).then(|| rendered.join("\n"))
}

pub(crate) fn provider_message_parts(value: &Value) -> Option<&Vec<Value>> {
    value
        .get("parts")
        .or_else(|| value.get("content"))
        .or_else(|| value.get("blocks"))
        .and_then(Value::as_array)
}

pub(crate) fn provider_part_text(part: &Value) -> Option<String> {
    let kind = part
        .get("type")
        .or_else(|| part.get("kind"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if matches!(
        kind,
        "tool_use" | "tool-use" | "tool_call" | "tool-call" | "function_call"
    ) {
        let name = part
            .get("name")
            .or_else(|| part.get("tool"))
            .or_else(|| part.get("tool_name"))
            .or_else(|| part.get("toolName"))
            .and_then(Value::as_str)
            .unwrap_or("tool");
        return Some(format!("tool call: {name}"));
    }
    if matches!(
        kind,
        "tool_result" | "tool-result" | "tool_use_result" | "function_result"
    ) {
        return part
            .get("content")
            .or_else(|| part.get("result"))
            .or_else(|| part.get("output"))
            .and_then(provider_value_text)
            .or_else(|| Some("tool result".to_owned()));
    }
    part.get("text")
        .or_else(|| part.get("content"))
        .or_else(|| part.get("thinking"))
        .or_else(|| part.get("summary"))
        .and_then(provider_value_text)
}

pub(crate) fn provider_json_without_keys(value: &Value, keys: &[&str]) -> Value {
    let Value::Object(object) = value else {
        return value.clone();
    };
    let mut object = object.clone();
    for key in keys {
        object.remove(*key);
    }
    Value::Object(object)
}

pub(crate) fn task_json_string_field(value: &Value, fields: &[&str]) -> Option<String> {
    fields
        .iter()
        .find_map(|field| value.get(*field).and_then(Value::as_str))
        .filter(|text| !text.trim().is_empty())
        .map(str::to_owned)
}

pub(crate) fn task_json_time_field(value: &Value, fields: &[&str]) -> Option<DateTime<Utc>> {
    for field in fields {
        let Some(value) = value.get(*field) else {
            continue;
        };
        if let Some(text) = value.as_str() {
            if let Some(parsed) = parse_rfc3339_utc(text) {
                return Some(parsed);
            }
            if let Ok(number) = text.parse::<i64>() {
                if let Some(parsed) = task_json_timestamp_number(number) {
                    return Some(parsed);
                }
            }
        }
        if let Some(number) = value.as_i64().and_then(task_json_timestamp_number) {
            return Some(number);
        }
    }
    None
}

pub(crate) fn task_json_timestamp_number(value: i64) -> Option<DateTime<Utc>> {
    if value > 10_000_000_000 {
        DateTime::<Utc>::from_timestamp_millis(value)
    } else {
        DateTime::<Utc>::from_timestamp(value, 0)
    }
}
