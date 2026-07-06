use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use chrono::{DateTime, NaiveDateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventType, Fidelity, ProviderCaptureEnvelope,
    ProviderEventEnvelope, ProviderSourceTrust,
};
use rusqlite::Connection;
use serde_json::{json, Value};

use crate::common::time::parse_rfc3339_utc;
use crate::provider::custom_history_jsonl::push_provider_import_failure;
use crate::provider::file_touches::provider_file_touches_from_raw_value;
use crate::provider::native::{
    native_event, native_provider_capture, open_provider_sqlite_readonly, provider_json_text,
    provider_line_from_index, provider_role, provider_timestamp_seconds, provider_value_text,
    sqlite_bool, text_id_index, NativeEventDraft, NativeSessionDraft,
};
use crate::provider::sqlite::{
    ensure_sqlite_table_columns, opencode_schema_fingerprint, optional_column_expr,
    sqlite_table_columns, sqlite_table_exists,
};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderNormalizationResult, Result,
    GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT, PROVIDER_MAX_TEXT_CHARS,
};

pub(crate) struct GooseSessionRow {
    pub(crate) id: String,
    pub(crate) name: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) user_set_name: bool,
    pub(crate) session_type: Option<String>,
    pub(crate) working_dir: Option<String>,
    pub(crate) created_at: Option<String>,
    pub(crate) updated_at: Option<String>,
    pub(crate) extension_data: Option<String>,
    pub(crate) total_tokens: Option<i64>,
    pub(crate) input_tokens: Option<i64>,
    pub(crate) output_tokens: Option<i64>,
    pub(crate) accumulated_total_tokens: Option<i64>,
    pub(crate) accumulated_input_tokens: Option<i64>,
    pub(crate) accumulated_output_tokens: Option<i64>,
    pub(crate) accumulated_cost: Option<f64>,
    pub(crate) provider_name: Option<String>,
    pub(crate) model_config_json: Option<String>,
    pub(crate) goose_mode: Option<String>,
    pub(crate) archived_at: Option<String>,
    pub(crate) project_id: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct GooseMessageRow {
    pub(crate) rowid: i64,
    pub(crate) id: i64,
    pub(crate) message_id: Option<String>,
    pub(crate) session_id: String,
    pub(crate) role: String,
    pub(crate) content_json: String,
    pub(crate) created_timestamp: Option<i64>,
    pub(crate) timestamp: Option<String>,
    pub(crate) tokens: Option<String>,
    pub(crate) metadata_json: Option<String>,
}

pub(crate) fn normalize_goose_sessions_sqlite(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    let conn = open_provider_sqlite_readonly(path)?;
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let schema_fingerprint = opencode_schema_fingerprint(&conn)?;
    let schema_version = goose_schema_version(&conn)?;
    let sessions = goose_sessions(&conn)?;
    let messages = goose_messages(&conn)?;
    let sessions_by_id = sessions
        .iter()
        .map(|session| (session.id.clone(), session))
        .collect::<BTreeMap<_, _>>();
    let raw_source_path = path.display().to_string();
    let mut seen_message_sessions = BTreeSet::new();
    let mut result = ProviderNormalizationResult::default();

    for message in messages {
        let provider_event_index = goose_event_index(&message);
        let line = provider_line_from_index(provider_event_index);
        let Some(session) = sessions_by_id.get(&message.session_id) else {
            push_provider_import_failure(
                &mut result.summary,
                line,
                format!(
                    "Goose message {} references missing session {}",
                    goose_message_identity(&message),
                    message.session_id
                ),
            );
            continue;
        };
        let content: Value = match serde_json::from_str(&message.content_json) {
            Ok(content) => content,
            Err(err) => {
                push_provider_import_failure(
                    &mut result.summary,
                    line,
                    format!(
                        "invalid JSON in Goose message {} content_json: {err}",
                        goose_message_identity(&message)
                    ),
                );
                continue;
            }
        };
        let metadata = message
            .metadata_json
            .as_deref()
            .map(provider_json_text)
            .unwrap_or(Value::Null);
        seen_message_sessions.insert(message.session_id.clone());
        let started_at = goose_timestamp(session.created_at.as_deref(), context.imported_at);
        let occurred_at = goose_message_timestamp(&message, started_at);
        let ended_at = session
            .updated_at
            .as_deref()
            .map(|timestamp| goose_timestamp(Some(timestamp), occurred_at));
        let event_type = goose_event_type(&message.role, &content);
        let text = goose_content_text(&content)
            .unwrap_or_else(|| format!("Goose {} message", message.role));
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
        result
            .files_touched
            .extend(provider_file_touches_from_raw_value(
                CaptureProvider::Goose,
                &session.id,
                GOOSE_SESSIONS_SQLITE_SOURCE_FORMAT,
                Some(raw_source_path.as_str()),
                &event.payload,
                &event,
                line,
            ));
        result.captures.push((
            line,
            goose_capture(
                session,
                GooseCaptureContext {
                    started_at,
                    ended_at,
                    raw_source_path: &raw_source_path,
                    user_version,
                    schema_version,
                    schema_fingerprint: &schema_fingerprint,
                    event: Some(event),
                },
                context,
            ),
        ));
    }

    for session in &sessions {
        if seen_message_sessions.contains(&session.id) {
            continue;
        }
        let started_at = goose_timestamp(session.created_at.as_deref(), context.imported_at);
        let ended_at = session
            .updated_at
            .as_deref()
            .map(|timestamp| goose_timestamp(Some(timestamp), started_at));
        result.captures.push((
            0,
            goose_capture(
                session,
                GooseCaptureContext {
                    started_at,
                    ended_at,
                    raw_source_path: &raw_source_path,
                    user_version,
                    schema_version,
                    schema_fingerprint: &schema_fingerprint,
                    event: None,
                },
                context,
            ),
        ));
    }
    Ok(result)
}

pub(crate) struct GooseCaptureContext<'a> {
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) ended_at: Option<DateTime<Utc>>,
    pub(crate) raw_source_path: &'a str,
    pub(crate) user_version: i64,
    pub(crate) schema_version: Option<i64>,
    pub(crate) schema_fingerprint: &'a str,
    pub(crate) event: Option<ProviderEventEnvelope>,
}

pub(crate) fn goose_capture(
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

pub(crate) fn goose_schema_version(conn: &Connection) -> Result<Option<i64>> {
    if !sqlite_table_exists(conn, "schema_version")? {
        return Ok(None);
    }
    let columns = sqlite_table_columns(conn, "schema_version")?;
    let version_column = if columns.contains("version") {
        "version"
    } else if columns.contains("id") {
        "id"
    } else {
        return Ok(None);
    };
    let sql = format!("select max({version_column}) from schema_version");
    conn.query_row(&sql, [], |row| row.get::<_, Option<i64>>(0))
        .map_err(CaptureError::from)
}

pub(crate) fn goose_sessions(conn: &Connection) -> Result<Vec<GooseSessionRow>> {
    if !sqlite_table_exists(conn, "sessions")? {
        return Err(CaptureError::InvalidPayload(
            "Goose sessions.db is missing required sessions table".into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "sessions")?;
    ensure_sqlite_table_columns(&columns, "Goose sessions table", &["id"])?;
    let name = optional_column_expr(&columns, "name", "NULL");
    let description = optional_column_expr(&columns, "description", "NULL");
    let user_set_name = optional_column_expr(&columns, "user_set_name", "0");
    let session_type = optional_column_expr(&columns, "session_type", "NULL");
    let working_dir = optional_column_expr(&columns, "working_dir", "NULL");
    let created_at = optional_column_expr(&columns, "created_at", "NULL");
    let updated_at = optional_column_expr(&columns, "updated_at", "NULL");
    let extension_data = optional_column_expr(&columns, "extension_data", "NULL");
    let total_tokens = optional_column_expr(&columns, "total_tokens", "NULL");
    let input_tokens = optional_column_expr(&columns, "input_tokens", "NULL");
    let output_tokens = optional_column_expr(&columns, "output_tokens", "NULL");
    let accumulated_total_tokens =
        optional_column_expr(&columns, "accumulated_total_tokens", "NULL");
    let accumulated_input_tokens =
        optional_column_expr(&columns, "accumulated_input_tokens", "NULL");
    let accumulated_output_tokens =
        optional_column_expr(&columns, "accumulated_output_tokens", "NULL");
    let accumulated_cost = optional_column_expr(&columns, "accumulated_cost", "NULL");
    let provider_name = optional_column_expr(&columns, "provider_name", "NULL");
    let model_config_json = optional_column_expr(&columns, "model_config_json", "NULL");
    let goose_mode = optional_column_expr(&columns, "goose_mode", "NULL");
    let archived_at = optional_column_expr(&columns, "archived_at", "NULL");
    let project_id = optional_column_expr(&columns, "project_id", "NULL");
    let order_by = if columns.contains("created_at") {
        "created_at, id"
    } else {
        "id"
    };
    let sql = format!(
        "select CAST(id AS TEXT), {name}, {description}, {user_set_name}, {session_type}, \
         {working_dir}, {created_at}, {updated_at}, {extension_data}, {total_tokens}, \
         {input_tokens}, {output_tokens}, {accumulated_total_tokens}, \
         {accumulated_input_tokens}, {accumulated_output_tokens}, {accumulated_cost}, \
         {provider_name}, {model_config_json}, {goose_mode}, {archived_at}, {project_id} \
         from sessions order by {order_by}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok(GooseSessionRow {
            id: row.get(0)?,
            name: row.get(1)?,
            description: row.get(2)?,
            user_set_name: sqlite_bool(row.get::<_, Option<i64>>(3)?),
            session_type: row.get(4)?,
            working_dir: row.get(5)?,
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
            extension_data: row.get(8)?,
            total_tokens: row.get(9)?,
            input_tokens: row.get(10)?,
            output_tokens: row.get(11)?,
            accumulated_total_tokens: row.get(12)?,
            accumulated_input_tokens: row.get(13)?,
            accumulated_output_tokens: row.get(14)?,
            accumulated_cost: row.get(15)?,
            provider_name: row.get(16)?,
            model_config_json: row.get(17)?,
            goose_mode: row.get(18)?,
            archived_at: row.get(19)?,
            project_id: row.get(20)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CaptureError::from)
}

pub(crate) fn goose_messages(conn: &Connection) -> Result<Vec<GooseMessageRow>> {
    if !sqlite_table_exists(conn, "messages")? {
        return Err(CaptureError::InvalidPayload(
            "Goose sessions.db is missing required messages table".into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "messages")?;
    ensure_sqlite_table_columns(
        &columns,
        "Goose messages table",
        &["session_id", "role", "content_json"],
    )?;
    let id = if columns.contains("id") {
        "id"
    } else {
        "rowid"
    };
    let message_id = optional_column_expr(&columns, "message_id", "NULL");
    let created_timestamp = optional_column_expr(&columns, "created_timestamp", "NULL");
    let timestamp = optional_column_expr(&columns, "timestamp", "NULL");
    let tokens = if columns.contains("tokens") {
        "CAST(tokens AS TEXT)"
    } else {
        "NULL"
    };
    let metadata_json = optional_column_expr(&columns, "metadata_json", "NULL");
    let order_by = if columns.contains("created_timestamp") {
        "session_id, created_timestamp, rowid"
    } else {
        "session_id, rowid"
    };
    let sql = format!(
        "select rowid, {id}, {message_id}, CAST(session_id AS TEXT), role, content_json, \
         {created_timestamp}, {timestamp}, {tokens}, {metadata_json} \
         from messages order by {order_by}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok(GooseMessageRow {
            rowid: row.get(0)?,
            id: row.get(1)?,
            message_id: row.get(2)?,
            session_id: row.get(3)?,
            role: row.get(4)?,
            content_json: row.get(5)?,
            created_timestamp: row.get(6)?,
            timestamp: row.get(7)?,
            tokens: row.get(8)?,
            metadata_json: row.get(9)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CaptureError::from)
}

pub(crate) fn goose_event_index(message: &GooseMessageRow) -> u64 {
    let base = message.created_timestamp.unwrap_or(message.rowid).max(0) as u64;
    base.saturating_mul(4_096)
        .saturating_add(text_id_index(&goose_message_identity(message), 0) % 4_096)
}

pub(crate) fn goose_message_identity(message: &GooseMessageRow) -> String {
    message
        .message_id
        .clone()
        .unwrap_or_else(|| format!("row-{}", message.id))
}

pub(crate) fn goose_message_timestamp(
    message: &GooseMessageRow,
    fallback: DateTime<Utc>,
) -> DateTime<Utc> {
    if let Some(timestamp) = message.created_timestamp {
        return provider_timestamp_seconds(Some(timestamp as f64), fallback);
    }
    goose_timestamp(message.timestamp.as_deref(), fallback)
}

pub(crate) fn goose_timestamp(raw: Option<&str>, fallback: DateTime<Utc>) -> DateTime<Utc> {
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

pub(crate) fn goose_event_type(role: &str, content: &Value) -> EventType {
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

pub(crate) fn goose_content_has_type(content: &Value, expected: &str) -> bool {
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

pub(crate) fn goose_content_text(content: &Value) -> Option<String> {
    let mut parts = Vec::new();
    goose_collect_text(content, &mut parts);
    (!parts.is_empty()).then(|| parts.join("\n"))
}

pub(crate) fn goose_collect_text(value: &Value, parts: &mut Vec<String>) {
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
                    for key in ["toolResult", "content", "result"] {
                        if let Some(text) = object.get(key).and_then(provider_value_text) {
                            parts.push(text);
                            break;
                        }
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
