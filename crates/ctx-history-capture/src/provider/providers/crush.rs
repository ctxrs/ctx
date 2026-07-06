use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, Confidence, EventType, Fidelity, FileChangeKind,
    ProviderCaptureEnvelope, ProviderEventEnvelope, ProviderSourceTrust,
};
use rusqlite::Connection;
use serde_json::{json, Value};

use crate::provider::custom_history_jsonl::push_provider_import_failure;
use crate::provider::file_touches::provider_file_touches_from_raw_value;
use crate::provider::native::{
    native_event, native_provider_capture, open_provider_sqlite_readonly, provider_line_from_index,
    provider_role, provider_timestamp_millis, provider_timestamp_seconds, provider_value_text,
    sqlite_bool, text_id_index, NativeEventDraft, NativeSessionDraft,
};
use crate::provider::sqlite::{
    ensure_sqlite_table_columns, opencode_schema_fingerprint, optional_column_expr,
    sqlite_table_columns, sqlite_table_exists,
};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderFileTouchedEnvelope, ProviderNormalizationResult,
    Result, CRUSH_SQLITE_SOURCE_FORMAT, PROVIDER_MAX_TEXT_CHARS,
};

pub(crate) struct CrushSessionRow {
    pub(crate) id: String,
    pub(crate) parent_session_id: Option<String>,
    pub(crate) title: Option<String>,
    pub(crate) created_at: Option<i64>,
    pub(crate) updated_at: Option<i64>,
    pub(crate) prompt_tokens: Option<i64>,
    pub(crate) completion_tokens: Option<i64>,
    pub(crate) cost: Option<f64>,
    pub(crate) summary_message_id: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct CrushMessageRow {
    pub(crate) rowid: i64,
    pub(crate) id: String,
    pub(crate) session_id: String,
    pub(crate) role: String,
    pub(crate) parts: String,
    pub(crate) created_at: Option<i64>,
    pub(crate) updated_at: Option<i64>,
    pub(crate) provider: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) is_summary_message: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct CrushFileRow {
    pub(crate) rowid: i64,
    pub(crate) session_id: Option<String>,
    pub(crate) path: String,
    pub(crate) version: Option<String>,
    pub(crate) created_at: Option<i64>,
    pub(crate) updated_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub(crate) struct CrushReadFileRow {
    pub(crate) rowid: i64,
    pub(crate) session_id: String,
    pub(crate) path: String,
    pub(crate) read_at: Option<i64>,
}

pub(crate) fn normalize_crush_sqlite(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    let conn = open_provider_sqlite_readonly(path)?;
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let schema_fingerprint = opencode_schema_fingerprint(&conn)?;
    let sessions = crush_sessions(&conn)?;
    let messages = crush_messages(&conn)?;
    let files = crush_files(&conn)?;
    let read_files = crush_read_files(&conn)?;
    let sessions_by_id = sessions
        .iter()
        .map(|session| (session.id.clone(), session))
        .collect::<BTreeMap<_, _>>();
    let mut seen_message_sessions = BTreeSet::new();
    let mut result = ProviderNormalizationResult::default();
    let raw_source_path = path.display().to_string();

    for message in messages {
        let provider_event_index = crush_event_index(&message);
        let line = provider_line_from_index(provider_event_index);
        let Some(session) = sessions_by_id.get(&message.session_id) else {
            push_provider_import_failure(
                &mut result.summary,
                line,
                format!(
                    "Crush message {} references missing session {}",
                    message.id, message.session_id
                ),
            );
            continue;
        };
        let parts: Value = match serde_json::from_str(&message.parts) {
            Ok(parts) => parts,
            Err(err) => {
                push_provider_import_failure(
                    &mut result.summary,
                    line,
                    format!("invalid JSON in Crush message {} parts: {err}", message.id),
                );
                continue;
            }
        };
        seen_message_sessions.insert(message.session_id.clone());
        let started_at = provider_timestamp_millis(session.created_at, context.imported_at);
        let occurred_at = provider_timestamp_millis(message.created_at, started_at);
        let ended_at = session
            .updated_at
            .map(|timestamp| provider_timestamp_millis(Some(timestamp), occurred_at));
        let event_type = crush_event_type(&message, &parts);
        let text =
            crush_parts_text(&parts).unwrap_or_else(|| format!("Crush {} message", message.role));
        let event = native_event(NativeEventDraft {
            provider: CaptureProvider::Crush,
            source_format: CRUSH_SQLITE_SOURCE_FORMAT,
            provider_session_id: message.session_id.clone(),
            provider_event_index,
            provider_event_hash: Some(message.id.clone()),
            cursor: format!(
                "session:{}:message:{}:rowid:{}",
                message.session_id, message.id, message.rowid
            ),
            event_type,
            role: Some(provider_role(Some(&message.role))),
            occurred_at,
            text,
            body: json!({
                "message_id": message.id,
                "role": message.role,
                "parts": parts,
                "provider": message.provider,
                "model": message.model,
                "is_summary_message": message.is_summary_message,
                "created_at": message.created_at,
                "updated_at": message.updated_at,
            }),
            metadata: json!({
                "source": "crush_messages",
                "source_format": CRUSH_SQLITE_SOURCE_FORMAT,
                "message_id": message.id,
                "session_id": message.session_id,
                "rowid": message.rowid,
                "provider": message.provider,
                "model": message.model,
            }),
        });
        result
            .files_touched
            .extend(provider_file_touches_from_raw_value(
                CaptureProvider::Crush,
                &session.id,
                CRUSH_SQLITE_SOURCE_FORMAT,
                Some(raw_source_path.as_str()),
                &event.payload,
                &event,
                line,
            ));
        result.captures.push((
            line,
            crush_capture(
                session,
                CrushCaptureContext {
                    started_at,
                    ended_at,
                    raw_source_path: &raw_source_path,
                    user_version,
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
        let started_at = provider_timestamp_millis(session.created_at, context.imported_at);
        let ended_at = session
            .updated_at
            .map(|timestamp| provider_timestamp_millis(Some(timestamp), started_at));
        result.captures.push((
            0,
            crush_capture(
                session,
                CrushCaptureContext {
                    started_at,
                    ended_at,
                    raw_source_path: &raw_source_path,
                    user_version,
                    schema_fingerprint: &schema_fingerprint,
                    event: None,
                },
                context,
            ),
        ));
    }

    result.files_touched.extend(crush_file_touches(
        files,
        read_files,
        &sessions_by_id,
        &raw_source_path,
        context.imported_at,
    ));
    Ok(result)
}

pub(crate) struct CrushCaptureContext<'a> {
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) ended_at: Option<DateTime<Utc>>,
    pub(crate) raw_source_path: &'a str,
    pub(crate) user_version: i64,
    pub(crate) schema_fingerprint: &'a str,
    pub(crate) event: Option<ProviderEventEnvelope>,
}

pub(crate) fn crush_capture(
    session: &CrushSessionRow,
    draft: CrushCaptureContext<'_>,
    context: &ProviderAdapterContext,
) -> ProviderCaptureEnvelope {
    let is_subagent = session.parent_session_id.is_some();
    native_provider_capture(
        NativeSessionDraft {
            provider: CaptureProvider::Crush,
            source_format: CRUSH_SQLITE_SOURCE_FORMAT,
            provider_session_id: session.id.clone(),
            parent_provider_session_id: session.parent_session_id.clone(),
            root_provider_session_id: session.parent_session_id.clone(),
            external_agent_id: None,
            agent_type: if is_subagent {
                AgentType::Subagent
            } else {
                AgentType::Primary
            },
            role_hint: Some(if is_subagent { "subagent" } else { "primary" }.to_owned()),
            is_primary: !is_subagent,
            started_at: draft.started_at,
            ended_at: draft.ended_at,
            cwd: None,
            fidelity: Fidelity::Imported,
            raw_source_path: draft.raw_source_path.to_owned(),
            trust: ProviderSourceTrust::ProviderNative,
            source_metadata: json!({
                "adapter": CRUSH_SQLITE_SOURCE_FORMAT,
                "sqlite_user_version": draft.user_version,
                "schema_fingerprint": draft.schema_fingerprint,
                "source_path": draft.raw_source_path,
                "upstream_tables": ["sessions", "messages", "files", "read_files"],
            }),
            session_metadata: json!({
                "source_format": CRUSH_SQLITE_SOURCE_FORMAT,
                "session_id": session.id,
                "title": session.title,
                "parent_session_id": session.parent_session_id,
                "summary_message_id": session.summary_message_id,
                "tokens": {
                    "prompt": session.prompt_tokens,
                    "completion": session.completion_tokens,
                },
                "cost": session.cost,
                "created_at": session.created_at,
                "updated_at": session.updated_at,
            }),
        },
        context,
        draft.event,
    )
}

pub(crate) fn crush_sessions(conn: &Connection) -> Result<Vec<CrushSessionRow>> {
    if !sqlite_table_exists(conn, "sessions")? {
        return Err(CaptureError::InvalidPayload(
            "Crush crush.db is missing required sessions table".into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "sessions")?;
    ensure_sqlite_table_columns(&columns, "Crush sessions table", &["id"])?;
    let parent_session_id = optional_column_expr(&columns, "parent_session_id", "NULL");
    let title = optional_column_expr(&columns, "title", "NULL");
    let created_at = optional_column_expr(&columns, "created_at", "NULL");
    let updated_at = optional_column_expr(&columns, "updated_at", "NULL");
    let prompt_tokens = optional_column_expr(&columns, "prompt_tokens", "NULL");
    let completion_tokens = optional_column_expr(&columns, "completion_tokens", "NULL");
    let cost = optional_column_expr(&columns, "cost", "NULL");
    let summary_message_id = optional_column_expr(&columns, "summary_message_id", "NULL");
    let order_by = if columns.contains("created_at") {
        "created_at, id"
    } else {
        "id"
    };
    let sql = format!(
        "select CAST(id AS TEXT), {parent_session_id}, {title}, {created_at}, {updated_at}, \
         {prompt_tokens}, {completion_tokens}, {cost}, {summary_message_id} \
         from sessions order by {order_by}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok(CrushSessionRow {
            id: row.get(0)?,
            parent_session_id: row.get(1)?,
            title: row.get(2)?,
            created_at: row.get(3)?,
            updated_at: row.get(4)?,
            prompt_tokens: row.get(5)?,
            completion_tokens: row.get(6)?,
            cost: row.get(7)?,
            summary_message_id: row.get(8)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CaptureError::from)
}

pub(crate) fn crush_messages(conn: &Connection) -> Result<Vec<CrushMessageRow>> {
    if !sqlite_table_exists(conn, "messages")? {
        return Err(CaptureError::InvalidPayload(
            "Crush crush.db is missing required messages table".into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "messages")?;
    ensure_sqlite_table_columns(
        &columns,
        "Crush messages table",
        &["id", "session_id", "role", "parts"],
    )?;
    let created_at = optional_column_expr(&columns, "created_at", "NULL");
    let updated_at = optional_column_expr(&columns, "updated_at", "NULL");
    let provider = optional_column_expr(&columns, "provider", "NULL");
    let model = optional_column_expr(&columns, "model", "NULL");
    let is_summary_message = optional_column_expr(&columns, "is_summary_message", "0");
    let order_by = if columns.contains("created_at") {
        "session_id, created_at, rowid"
    } else {
        "session_id, rowid"
    };
    let sql = format!(
        "select rowid, CAST(id AS TEXT), CAST(session_id AS TEXT), role, parts, \
         {created_at}, {updated_at}, {provider}, {model}, {is_summary_message} \
         from messages order by {order_by}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok(CrushMessageRow {
            rowid: row.get(0)?,
            id: row.get(1)?,
            session_id: row.get(2)?,
            role: row.get(3)?,
            parts: row.get(4)?,
            created_at: row.get(5)?,
            updated_at: row.get(6)?,
            provider: row.get(7)?,
            model: row.get(8)?,
            is_summary_message: sqlite_bool(row.get::<_, Option<i64>>(9)?),
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CaptureError::from)
}

pub(crate) fn crush_files(conn: &Connection) -> Result<Vec<CrushFileRow>> {
    if !sqlite_table_exists(conn, "files")? {
        return Ok(Vec::new());
    }
    let columns = sqlite_table_columns(conn, "files")?;
    ensure_sqlite_table_columns(&columns, "Crush files table", &["path"])?;
    let session_id = optional_column_expr(&columns, "session_id", "NULL");
    let version = optional_column_expr(&columns, "version", "NULL");
    let created_at = optional_column_expr(&columns, "created_at", "NULL");
    let updated_at = optional_column_expr(&columns, "updated_at", "NULL");
    let sql = format!(
        "select rowid, {session_id}, path, {version}, {created_at}, {updated_at} \
         from files order by rowid"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok(CrushFileRow {
            rowid: row.get(0)?,
            session_id: row.get(1)?,
            path: row.get(2)?,
            version: row.get(3)?,
            created_at: row.get(4)?,
            updated_at: row.get(5)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CaptureError::from)
}

pub(crate) fn crush_read_files(conn: &Connection) -> Result<Vec<CrushReadFileRow>> {
    if !sqlite_table_exists(conn, "read_files")? {
        return Ok(Vec::new());
    }
    let columns = sqlite_table_columns(conn, "read_files")?;
    ensure_sqlite_table_columns(&columns, "Crush read_files table", &["session_id", "path"])?;
    let read_at = optional_column_expr(&columns, "read_at", "NULL");
    let sql = format!(
        "select rowid, CAST(session_id AS TEXT), path, {read_at} from read_files order by rowid"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok(CrushReadFileRow {
            rowid: row.get(0)?,
            session_id: row.get(1)?,
            path: row.get(2)?,
            read_at: row.get(3)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CaptureError::from)
}

pub(crate) fn crush_file_touches(
    files: Vec<CrushFileRow>,
    read_files: Vec<CrushReadFileRow>,
    sessions_by_id: &BTreeMap<String, &CrushSessionRow>,
    raw_source_path: &str,
    fallback: DateTime<Utc>,
) -> Vec<(usize, ProviderFileTouchedEnvelope)> {
    let mut touches = Vec::new();
    for row in files {
        let Some(session_id) = row
            .session_id
            .as_ref()
            .filter(|session_id| sessions_by_id.contains_key(*session_id))
        else {
            continue;
        };
        let occurred_at = provider_timestamp_millis(row.updated_at.or(row.created_at), fallback);
        let touch_index = 0x0100_0000_0000_u64.saturating_add(row.rowid.max(0) as u64);
        touches.push((
            provider_line_from_index(touch_index),
            ProviderFileTouchedEnvelope {
                provider: CaptureProvider::Crush,
                provider_session_id: session_id.clone(),
                provider_touch_index: touch_index,
                provider_event_index: None,
                raw_source_path: Some(raw_source_path.to_owned()),
                path: row.path,
                change_kind: Some(FileChangeKind::Modified),
                old_path: None,
                line_count_delta: None,
                confidence: Confidence::Explicit,
                occurred_at,
                source_format: CRUSH_SQLITE_SOURCE_FORMAT.to_owned(),
                metadata: json!({
                    "source": "crush_files",
                    "rowid": row.rowid,
                    "version": row.version,
                    "created_at": row.created_at,
                    "updated_at": row.updated_at,
                }),
            },
        ));
    }
    for row in read_files {
        if !sessions_by_id.contains_key(&row.session_id) {
            continue;
        }
        let occurred_at =
            provider_timestamp_seconds(row.read_at.map(|value| value as f64), fallback);
        let touch_index = 0x0200_0000_0000_u64.saturating_add(row.rowid.max(0) as u64);
        touches.push((
            provider_line_from_index(touch_index),
            ProviderFileTouchedEnvelope {
                provider: CaptureProvider::Crush,
                provider_session_id: row.session_id,
                provider_touch_index: touch_index,
                provider_event_index: None,
                raw_source_path: Some(raw_source_path.to_owned()),
                path: row.path,
                change_kind: Some(FileChangeKind::Read),
                old_path: None,
                line_count_delta: None,
                confidence: Confidence::Explicit,
                occurred_at,
                source_format: CRUSH_SQLITE_SOURCE_FORMAT.to_owned(),
                metadata: json!({
                    "source": "crush_read_files",
                    "rowid": row.rowid,
                    "read_at": row.read_at,
                }),
            },
        ));
    }
    touches
}

pub(crate) fn crush_event_index(message: &CrushMessageRow) -> u64 {
    let base = message
        .created_at
        .or(message.updated_at)
        .unwrap_or(message.rowid)
        .max(0) as u64;
    base.saturating_mul(4_096)
        .saturating_add(text_id_index(&message.id, 0) % 4_096)
}

pub(crate) fn crush_event_type(message: &CrushMessageRow, parts: &Value) -> EventType {
    if message.is_summary_message {
        return EventType::Summary;
    }
    if crush_parts_have_type(parts, "shell_command") {
        EventType::CommandOutput
    } else if crush_parts_have_type(parts, "tool_result") || message.role == "tool" {
        EventType::ToolOutput
    } else if crush_parts_have_type(parts, "tool_call") {
        EventType::ToolCall
    } else {
        EventType::Message
    }
}

pub(crate) fn crush_parts_have_type(parts: &Value, expected: &str) -> bool {
    parts.as_array().is_some_and(|items| {
        items
            .iter()
            .any(|item| item.get("type").and_then(Value::as_str) == Some(expected))
    })
}

pub(crate) fn crush_parts_text(parts: &Value) -> Option<String> {
    let mut text = Vec::new();
    if let Some(items) = parts.as_array() {
        for item in items {
            let kind = item.get("type").and_then(Value::as_str).unwrap_or("part");
            let data = item.get("data").unwrap_or(item);
            match kind {
                "text" => push_json_text(&mut text, data.get("text").unwrap_or(data)),
                "reasoning" => {
                    push_json_text(
                        &mut text,
                        data.get("thinking")
                            .or_else(|| data.get("text"))
                            .unwrap_or(data),
                    );
                }
                "tool_call" => {
                    let name = data.get("name").and_then(Value::as_str).unwrap_or("tool");
                    text.push(format!("tool call: {name}"));
                    if let Some(input) = data.get("input").and_then(provider_value_text) {
                        text.push(format!("tool input: {input}"));
                    }
                }
                "tool_result" => {
                    let name = data.get("name").and_then(Value::as_str).unwrap_or("tool");
                    text.push(format!("tool result: {name}"));
                    for key in ["content", "data", "output"] {
                        if let Some(value) = data.get(key).and_then(provider_value_text) {
                            text.push(value);
                            break;
                        }
                    }
                }
                "shell_command" => {
                    if let Some(command) = data.get("command").and_then(Value::as_str) {
                        text.push(command.to_owned());
                    }
                    if let Some(output) = data.get("output").and_then(Value::as_str) {
                        text.push(output.to_owned());
                    }
                }
                "finish" => {
                    if let Some(reason) = data.get("reason").and_then(Value::as_str) {
                        text.push(format!("finish: {reason}"));
                    }
                }
                _ => push_json_text(&mut text, data),
            }
            if text.iter().map(|part| part.chars().count()).sum::<usize>()
                >= PROVIDER_MAX_TEXT_CHARS
            {
                break;
            }
        }
    } else {
        push_json_text(&mut text, parts);
    }
    (!text.is_empty()).then(|| text.join("\n"))
}

pub(crate) fn push_json_text(parts: &mut Vec<String>, value: &Value) {
    if let Some(text) = provider_value_text(value).filter(|text| !text.trim().is_empty()) {
        parts.push(text);
    }
}
