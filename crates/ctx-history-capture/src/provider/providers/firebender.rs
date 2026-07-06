use std::{
    collections::BTreeSet,
    fs::{self},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Duration, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventType, Fidelity, ProviderCaptureEnvelope,
    ProviderEventEnvelope, ProviderSourceTrust,
};
use rusqlite::Connection;
use serde_json::{json, Value};

use crate::provider::custom_history_jsonl::push_provider_import_failure;
use crate::provider::native::{
    native_event, native_provider_capture, open_provider_sqlite_readonly, provider_capped_json,
    provider_json_text, provider_line_from_index, provider_role, provider_timestamp_millis,
    provider_timestamp_value, provider_value_text, NativeEventDraft, NativeSessionDraft,
};
use crate::provider::sqlite::{
    ensure_sqlite_table_columns, opencode_schema_fingerprint, sqlite_table_columns,
    sqlite_table_exists,
};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderNormalizationResult, Result,
    FIREBENDER_SQLITE_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS,
};

pub(crate) struct FirebenderChatSessionRow {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
    pub(crate) messages_json: String,
    pub(crate) metadata_json: String,
    pub(crate) row_number: u64,
}

pub(crate) fn normalize_firebender_sqlite(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    let db_path = firebender_chat_history_db_path(path)?;
    let conn = open_provider_sqlite_readonly(&db_path)?;
    if !sqlite_table_exists(&conn, "chat_sessions")? {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: db_path,
            reason: "Firebender chat_history.db is missing required chat_sessions table",
        });
    }
    let columns = sqlite_table_columns(&conn, "chat_sessions")?;
    ensure_sqlite_table_columns(
        &columns,
        "Firebender chat_sessions table",
        &[
            "id",
            "name",
            "created_at",
            "updated_at",
            "messages_json",
            "metadata_json",
        ],
    )?;
    let schema_fingerprint = opencode_schema_fingerprint(&conn)?;
    let rows = firebender_chat_session_rows(&conn, &columns)?;
    let mut result = ProviderNormalizationResult::default();

    for row in rows {
        let line = provider_line_from_index(row.row_number);
        let started_at = provider_timestamp_millis(Some(row.created_at), context.imported_at);
        let ended_at = Some(provider_timestamp_millis(Some(row.updated_at), started_at));
        let metadata = provider_json_text(&row.metadata_json);
        let messages = match serde_json::from_str::<Value>(&row.messages_json) {
            Ok(Value::Array(messages)) => messages,
            Ok(_) => {
                push_provider_import_failure(
                    &mut result.summary,
                    line,
                    format!(
                        "Firebender session {} messages_json is not an array",
                        row.id
                    ),
                );
                Vec::new()
            }
            Err(err) => {
                push_provider_import_failure(
                    &mut result.summary,
                    line,
                    format!(
                        "Firebender session {} messages_json is invalid JSON: {err}",
                        row.id
                    ),
                );
                Vec::new()
            }
        };

        if messages.is_empty() {
            result.captures.push((
                line,
                firebender_capture(
                    &row,
                    &metadata,
                    &db_path,
                    started_at,
                    ended_at,
                    &schema_fingerprint,
                    context,
                    None,
                ),
            ));
            continue;
        }

        for (message_index, message) in messages.iter().enumerate() {
            let provider_event_index = message_index as u64;
            let occurred_at = firebender_message_time(
                message,
                started_at + Duration::milliseconds(message_index as i64),
            );
            let event = firebender_event(&row.id, provider_event_index, message, occurred_at);
            result.captures.push((
                line,
                firebender_capture(
                    &row,
                    &metadata,
                    &db_path,
                    started_at,
                    ended_at,
                    &schema_fingerprint,
                    context,
                    Some(event),
                ),
            ));
        }
    }

    Ok(result)
}

pub(crate) fn firebender_chat_history_db_path(path: &Path) -> Result<PathBuf> {
    let metadata = fs::symlink_metadata(path)?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "symlinked provider transcript roots are rejected",
        });
    }
    if file_type.is_file() {
        return Ok(path.to_path_buf());
    }
    if file_type.is_dir() {
        let db_path = path
            .join(".idea")
            .join("firebender")
            .join("chat_history.db");
        if db_path.exists() {
            return Ok(db_path);
        }
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Firebender project root is missing .idea/firebender/chat_history.db",
        });
    }
    Err(CaptureError::InvalidProviderTranscriptPath {
        path: path.to_path_buf(),
        reason: "Firebender import path must be chat_history.db or a project root",
    })
}

pub(crate) fn firebender_source_root(path: &Path) -> Result<PathBuf> {
    let db_path = firebender_chat_history_db_path(path)?;
    let Some(firebender_dir) = db_path.parent() else {
        return Ok(db_path);
    };
    if firebender_dir.file_name().and_then(|name| name.to_str()) != Some("firebender") {
        return Ok(db_path);
    }
    let Some(idea_dir) = firebender_dir.parent() else {
        return Ok(db_path);
    };
    if idea_dir.file_name().and_then(|name| name.to_str()) != Some(".idea") {
        return Ok(db_path);
    }
    Ok(idea_dir.parent().unwrap_or(&db_path).to_path_buf())
}

pub(crate) fn firebender_chat_session_rows(
    conn: &Connection,
    columns: &BTreeSet<String>,
) -> Result<Vec<FirebenderChatSessionRow>> {
    let deleted_filter = if columns.contains("deleted_at") {
        "where deleted_at is null"
    } else {
        ""
    };
    let sql = format!(
        "select id, name, created_at, updated_at, messages_json, metadata_json \
         from chat_sessions {deleted_filter} order by updated_at, id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
        ))
    })?;
    let mut out = Vec::new();
    for (index, row) in rows.enumerate() {
        let (id, name, created_at, updated_at, messages_json, metadata_json) = row?;
        out.push(FirebenderChatSessionRow {
            id,
            name,
            created_at,
            updated_at,
            messages_json,
            metadata_json,
            row_number: u64::try_from(index + 1).unwrap_or(u64::MAX),
        });
    }
    Ok(out)
}

pub(crate) fn firebender_message_time(message: &Value, fallback: DateTime<Utc>) -> DateTime<Utc> {
    provider_timestamp_value(
        message
            .get("timestamp")
            .or_else(|| message.get("created_at"))
            .or_else(|| message.get("updated_at")),
        fallback,
    )
}

pub(crate) fn firebender_event(
    provider_session_id: &str,
    provider_event_index: u64,
    message: &Value,
    occurred_at: DateTime<Utc>,
) -> ProviderEventEnvelope {
    let role = message.get("role").and_then(Value::as_str);
    let tool_calls = message
        .get("tool_calls")
        .or_else(|| message.get("toolCalls"));
    let event_type = if role == Some("tool") {
        EventType::ToolOutput
    } else if tool_calls.is_some_and(|value| {
        value
            .as_array()
            .map(|items| !items.is_empty())
            .unwrap_or(true)
    }) {
        EventType::ToolCall
    } else {
        EventType::Message
    };
    native_event(NativeEventDraft {
        provider: CaptureProvider::Firebender,
        source_format: FIREBENDER_SQLITE_SOURCE_FORMAT,
        provider_session_id: provider_session_id.to_owned(),
        provider_event_index,
        provider_event_hash: message
            .get("id")
            .or_else(|| message.get("tool_call_id"))
            .or_else(|| message.get("toolCallId"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        cursor: format!("chat_sessions:{provider_session_id}:message:{provider_event_index}"),
        event_type,
        role: Some(provider_role(role)),
        occurred_at,
        text: firebender_message_text(message)
            .unwrap_or_else(|| format!("Firebender {}", role.unwrap_or("message"))),
        body: message.clone(),
        metadata: json!({
            "source": "firebender_chat_sessions",
            "source_format": FIREBENDER_SQLITE_SOURCE_FORMAT,
            "role": role,
            "name": message.get("name").and_then(Value::as_str),
            "tool_call_id": message
                .get("tool_call_id")
                .or_else(|| message.get("toolCallId"))
                .and_then(Value::as_str),
            "content_type": message
                .get("content")
                .and_then(|content| content.get("type"))
                .and_then(Value::as_str),
        }),
    })
}

pub(crate) fn firebender_message_text(message: &Value) -> Option<String> {
    if let Some(content) = message.get("content") {
        match content {
            Value::Object(object) => {
                if let Some(text) = object
                    .get("text")
                    .or_else(|| object.get("content"))
                    .and_then(Value::as_str)
                    .filter(|text| !text.trim().is_empty())
                {
                    return Some(text.to_owned());
                }
            }
            _ => {
                if let Some(text) =
                    provider_value_text(content).filter(|text| !text.trim().is_empty())
                {
                    return Some(text);
                }
            }
        }
    }
    if let Some(tool_calls) = message
        .get("tool_calls")
        .or_else(|| message.get("toolCalls"))
        .and_then(Value::as_array)
    {
        let names = tool_calls
            .iter()
            .filter_map(|call| {
                call.get("function")
                    .and_then(|function| function.get("name"))
                    .or_else(|| call.get("name"))
                    .and_then(Value::as_str)
            })
            .collect::<Vec<_>>();
        if !names.is_empty() {
            return Some(format!("tool call: {}", names.join(", ")));
        }
    }
    message
        .get("name")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(str::to_owned)
}

pub(crate) fn firebender_capture(
    row: &FirebenderChatSessionRow,
    metadata: &Value,
    path: &Path,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    schema_fingerprint: &str,
    context: &ProviderAdapterContext,
    event: Option<ProviderEventEnvelope>,
) -> ProviderCaptureEnvelope {
    native_provider_capture(
        NativeSessionDraft {
            provider: CaptureProvider::Firebender,
            source_format: FIREBENDER_SQLITE_SOURCE_FORMAT,
            provider_session_id: row.id.clone(),
            parent_provider_session_id: None,
            root_provider_session_id: None,
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".to_owned()),
            is_primary: true,
            started_at,
            ended_at,
            cwd: None,
            fidelity: Fidelity::Imported,
            raw_source_path: path.display().to_string(),
            trust: ProviderSourceTrust::ProviderNative,
            source_metadata: json!({
                "adapter": FIREBENDER_SQLITE_SOURCE_FORMAT,
                "schema_fingerprint": schema_fingerprint,
                "storage": ".idea/firebender/chat_history.db",
            }),
            session_metadata: json!({
                "source_format": FIREBENDER_SQLITE_SOURCE_FORMAT,
                "title": row.name,
                "metadata": provider_capped_json(metadata, PROVIDER_MAX_PREVIEW_CHARS),
                "storage": ".idea/firebender/chat_history.db",
                "timestamp_note": "message rows do not carry durable per-message timestamps; ctx preserves session created_at/updated_at and import order",
            }),
        },
        context,
        event,
    )
}
