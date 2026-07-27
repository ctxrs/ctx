use chrono::{DateTime, NaiveDateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventType, Fidelity, ProviderCaptureEnvelope,
    ProviderEventEnvelope, ProviderSourceTrust,
};
use serde_json::{json, Value};

use crate::common::time::parse_rfc3339_utc;
use crate::provider::normalization::{
    native_event, native_provider_capture, provider_json_text, provider_line_from_index,
    provider_normalized_result_value, provider_role, provider_timestamp_seconds,
    provider_value_text, text_id_index, NativeEventDraft, NativeSessionDraft,
};
use crate::{
    ProviderAdapterContext, ProviderNormalizationResult, GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
    PROVIDER_MAX_TEXT_CHARS,
};

use super::schema::{GooseMessageRow, GooseSessionRow};

pub(super) fn goose_message_normalization(
    message: GooseMessageRow,
    session: Option<&GooseSessionRow>,
    raw_source_path: &str,
    user_version: i64,
    schema_version: Option<i64>,
    schema_fingerprint: &str,
    context: &ProviderAdapterContext,
) -> std::result::Result<GooseMessageProjection, GooseMessageRejection> {
    let provider_event_index = goose_event_index(&message);
    let line = provider_line_from_index(provider_event_index);
    let Some(session) = session else {
        return Err(GooseMessageRejection {
            line,
            reason: format!(
                "Goose message {} references missing session {}",
                goose_message_identity(&message),
                message.session_id
            ),
        });
    };
    let content: Value = match serde_json::from_str(&message.content_json) {
        Ok(content) => content,
        Err(err) => {
            return Err(GooseMessageRejection {
                line,
                reason: format!(
                    "invalid JSON in Goose message {} content_json: {err}",
                    goose_message_identity(&message)
                ),
            });
        }
    };
    let metadata = message
        .metadata_json
        .as_deref()
        .map(provider_json_text)
        .unwrap_or(Value::Null);
    let started_at = goose_timestamp(session.created_at.as_deref(), context.imported_at);
    let occurred_at = goose_message_timestamp(&message, started_at);
    let ended_at = session
        .updated_at
        .as_deref()
        .map(|timestamp| goose_timestamp(Some(timestamp), occurred_at));
    let event_type = goose_event_type(&message.role, &content);
    let complete_text = goose_complete_content_text(&content)
        .unwrap_or_else(|| format!("Goose {} message", message.role));
    let text =
        goose_content_text(&content).unwrap_or_else(|| format!("Goose {} message", message.role));
    let event = native_event(NativeEventDraft {
        provider: CaptureProvider::Goose,
        source_format: GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
        provider_session_id: message.session_id.clone(),
        provider_event_index,
        provider_event_hash: Some(goose_message_identity(&message)),
        cursor: format!(
            "session:{}:message:{}:rowid:{}",
            message.session_id,
            goose_message_identity(&message),
            message.rowid
        ),
        event_type,
        role: Some(provider_role(Some(&message.role))),
        occurred_at,
        text,
        body: json!({
            "message_id": message.message_id,
            "row_id": message.id,
            "role": message.role,
            "content": content,
            "metadata": metadata,
            "tokens": message.tokens.as_deref().map(provider_json_text),
            "created_timestamp": message.created_timestamp,
            "timestamp": message.timestamp,
        }),
        metadata: json!({
            "source": "goose_messages",
            "source_format": GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
            "message_id": message.message_id,
            "row_id": message.id,
            "session_id": message.session_id,
            "rowid": message.rowid,
        }),
    });
    let capture = goose_capture(
        session,
        GooseCaptureContext {
            started_at,
            ended_at,
            raw_source_path,
            user_version,
            schema_version,
            schema_fingerprint,
            event: Some(event.clone()),
        },
        context,
    );
    Ok(GooseMessageProjection {
        line,
        provider_session_id: session.id.clone(),
        event,
        raw_content: content,
        capture,
        complete_text,
    })
}

pub(super) struct GooseMessageProjection {
    pub(super) line: usize,
    pub(super) provider_session_id: String,
    pub(super) event: ProviderEventEnvelope,
    pub(super) raw_content: Value,
    pub(super) capture: ProviderCaptureEnvelope,
    pub(super) complete_text: String,
}

pub(super) struct GooseMessageRejection {
    pub(super) line: usize,
    pub(super) reason: String,
}

pub(super) fn goose_session_normalization(
    session: &GooseSessionRow,
    raw_source_path: &str,
    user_version: i64,
    schema_version: Option<i64>,
    schema_fingerprint: &str,
    context: &ProviderAdapterContext,
) -> ProviderNormalizationResult {
    let started_at = goose_timestamp(session.created_at.as_deref(), context.imported_at);
    let ended_at = session
        .updated_at
        .as_deref()
        .map(|timestamp| goose_timestamp(Some(timestamp), started_at));
    ProviderNormalizationResult {
        captures: vec![(
            0,
            goose_capture(
                session,
                GooseCaptureContext {
                    started_at,
                    ended_at,
                    raw_source_path,
                    user_version,
                    schema_version,
                    schema_fingerprint,
                    event: None,
                },
                context,
            ),
        )],
        ..ProviderNormalizationResult::default()
    }
}

struct GooseCaptureContext<'a> {
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    raw_source_path: &'a str,
    user_version: i64,
    schema_version: Option<i64>,
    schema_fingerprint: &'a str,
    event: Option<ProviderEventEnvelope>,
}

fn goose_capture(
    session: &GooseSessionRow,
    draft: GooseCaptureContext<'_>,
    context: &ProviderAdapterContext,
) -> ProviderCaptureEnvelope {
    native_provider_capture(
        NativeSessionDraft {
            provider: CaptureProvider::Goose,
            source_format: GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
            provider_session_id: session.id.clone(),
            parent_provider_session_id: None,
            root_provider_session_id: None,
            external_agent_id: session.provider_name.clone(),
            agent_type: AgentType::Primary,
            role_hint: session
                .session_type
                .clone()
                .or_else(|| Some("primary".to_owned())),
            is_primary: true,
            started_at: draft.started_at,
            ended_at: draft.ended_at,
            cwd: session.working_dir.clone(),
            fidelity: Fidelity::Imported,
            raw_source_path: draft.raw_source_path.to_owned(),
            trust: ProviderSourceTrust::ProviderNative,
            source_metadata: json!({
                "adapter": GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
                "sqlite_user_version": draft.user_version,
                "goose_schema_version": draft.schema_version,
                "schema_fingerprint": draft.schema_fingerprint,
                "source_path": draft.raw_source_path,
            }),
            session_metadata: json!({
                "source_format": GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
                "session_id": session.id,
                "name": session.name,
                "description": session.description,
                "user_set_name": session.user_set_name,
                "session_type": session.session_type,
                "extension_data": session.extension_data.as_deref().map(provider_json_text),
                "provider_name": session.provider_name,
                "model_config": session.model_config_json.as_deref().map(provider_json_text),
                "goose_mode": session.goose_mode,
                "archived_at": session.archived_at,
                "project_id": session.project_id,
                "tokens": {
                    "total": session.total_tokens,
                    "input": session.input_tokens,
                    "output": session.output_tokens,
                    "accumulated_total": session.accumulated_total_tokens,
                    "accumulated_input": session.accumulated_input_tokens,
                    "accumulated_output": session.accumulated_output_tokens,
                },
                "accumulated_cost": session.accumulated_cost,
            }),
        },
        context,
        draft.event,
    )
}

fn goose_event_index(message: &GooseMessageRow) -> u64 {
    let base = message.created_timestamp.unwrap_or(message.rowid).max(0) as u64;
    base.saturating_mul(4_096)
        .saturating_add(text_id_index(&goose_message_identity(message), 0) % 4_096)
}

pub(super) fn goose_message_identity(message: &GooseMessageRow) -> String {
    message
        .message_id
        .clone()
        .unwrap_or_else(|| format!("row-{}", message.id))
}

fn goose_message_timestamp(message: &GooseMessageRow, fallback: DateTime<Utc>) -> DateTime<Utc> {
    if let Some(timestamp) = message.created_timestamp {
        return provider_timestamp_seconds(Some(timestamp as f64), fallback);
    }
    goose_timestamp(message.timestamp.as_deref(), fallback)
}

pub(super) fn goose_timestamp(raw: Option<&str>, fallback: DateTime<Utc>) -> DateTime<Utc> {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return fallback;
    };
    parse_rfc3339_utc(raw)
        .or_else(|| {
            NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S%.f")
                .ok()
                .map(|naive| DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc))
        })
        .or_else(|| {
            raw.parse::<f64>()
                .ok()
                .map(|timestamp| provider_timestamp_seconds(Some(timestamp), fallback))
        })
        .unwrap_or(fallback)
}

fn goose_event_type(role: &str, content: &Value) -> EventType {
    if goose_content_has_type(content, "toolResponse") {
        EventType::ToolOutput
    } else if goose_content_has_type(content, "toolRequest")
        || goose_content_has_type(content, "frontendToolRequest")
    {
        EventType::ToolCall
    } else if matches!(role, "user" | "assistant" | "system") {
        EventType::Message
    } else {
        EventType::Notice
    }
}

fn goose_content_has_type(content: &Value, expected: &str) -> bool {
    match content {
        Value::Array(items) => items
            .iter()
            .any(|item| goose_content_has_type(item, expected)),
        Value::Object(object) => {
            object
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind == expected)
                || object
                    .values()
                    .any(|value| goose_content_has_type(value, expected))
        }
        _ => false,
    }
}

fn goose_content_text(content: &Value) -> Option<String> {
    let mut parts = Vec::new();
    goose_collect_text(content, &mut parts);
    (!parts.is_empty()).then(|| parts.join("\n"))
}

/// Returns complete normalized Goose tool-response bodies in native array
/// order. Only direct `toolResponse` blocks and their documented result fields
/// are accepted; arbitrary object descendants are not searched. The caller
/// owns any byte bound.
#[allow(dead_code)] // Activated by SQLite result-locator attachment.
pub(crate) fn goose_normalized_result_content(content: &Value) -> Option<String> {
    let mut parts = Vec::new();
    goose_collect_result_content(content, &mut parts);
    (!parts.is_empty()).then(|| parts.join("\n"))
}

pub(crate) fn goose_complete_content_text(content: &Value) -> Option<String> {
    let mut parts = Vec::new();
    goose_collect_complete_text(content, &mut parts);
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn goose_collect_complete_text(value: &Value, parts: &mut Vec<String>) {
    match value {
        Value::Array(items) => {
            for item in items {
                goose_collect_complete_text(item, parts);
            }
        }
        Value::Object(object) => {
            let before = parts.len();
            goose_collect_text(value, parts);
            if parts.len() == before {
                for child in object.values() {
                    goose_collect_complete_text(child, parts);
                }
            }
        }
        _ => goose_collect_text(value, parts),
    }
}

fn goose_collect_result_content(value: &Value, parts: &mut Vec<String>) {
    match value {
        Value::Array(items) => {
            for item in items {
                goose_collect_result_content(item, parts);
            }
        }
        Value::Object(object)
            if object.get("type").and_then(Value::as_str) == Some("toolResponse") =>
        {
            if let Some(value) = goose_tool_response_value(object) {
                parts.push(provider_normalized_result_value(value));
            }
        }
        _ => {}
    }
}

fn goose_collect_text(value: &Value, parts: &mut Vec<String>) {
    match value {
        Value::String(text) => parts.push(text.clone()),
        Value::Array(items) => {
            for item in items {
                goose_collect_text(item, parts);
                if parts.iter().map(|part| part.chars().count()).sum::<usize>()
                    >= PROVIDER_MAX_TEXT_CHARS
                {
                    break;
                }
            }
        }
        Value::Object(object) => {
            let kind = object.get("type").and_then(Value::as_str);
            match kind {
                Some("text") => {
                    if let Some(text) = object.get("text").and_then(Value::as_str) {
                        parts.push(text.to_owned());
                    }
                }
                Some("thinking") => {
                    if let Some(text) = object.get("thinking").and_then(Value::as_str) {
                        parts.push(text.to_owned());
                    }
                }
                Some("redactedThinking") => {
                    parts.push("redacted thinking".to_owned());
                }
                Some("toolRequest") | Some("frontendToolRequest") => {
                    let call = object.get("toolCall").unwrap_or(value);
                    let name = call
                        .get("name")
                        .or_else(|| object.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or("tool");
                    parts.push(format!("tool call: {name}"));
                    if let Some(input) = call
                        .get("arguments")
                        .or_else(|| call.get("input"))
                        .and_then(provider_value_text)
                    {
                        parts.push(format!("tool input: {input}"));
                    }
                }
                Some("toolResponse") => {
                    parts.push("tool response".to_owned());
                    if let Some(text) =
                        goose_tool_response_value(object).and_then(provider_value_text)
                    {
                        parts.push(text);
                    }
                }
                Some("toolConfirmationRequest") => {
                    parts.push("tool confirmation request".to_owned());
                }
                Some("systemNotification") | Some("actionRequired") => {
                    for key in ["message", "text", "content"] {
                        if let Some(text) = object.get(key).and_then(provider_value_text) {
                            parts.push(text);
                            break;
                        }
                    }
                }
                _ => {
                    for key in ["text", "content", "message"] {
                        if let Some(text) = object.get(key).and_then(provider_value_text) {
                            parts.push(text);
                            return;
                        }
                    }
                }
            }
        }
        Value::Number(_) | Value::Bool(_) => parts.push(value.to_string()),
        Value::Null => {}
    }
}

fn goose_tool_response_value(object: &serde_json::Map<String, Value>) -> Option<&Value> {
    ["toolResult", "content", "result"]
        .iter()
        .find_map(|key| object.get(*key))
}
