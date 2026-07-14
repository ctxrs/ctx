use std::{
    fs::{self},
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventRole, EventType, Fidelity, ProviderCaptureEnvelope,
    ProviderCursorCheckpoint, ProviderCursorRange, ProviderEventEnvelope, ProviderSessionEnvelope,
    ProviderSourceEnvelope, ProviderSourceTrust, SessionStatus,
    PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::common::io::{
    ensure_provider_path_parents_are_not_symlinks, ensure_regular_provider_transcript_file,
    read_json_file_limited,
};
use crate::provider::custom_history_jsonl::push_provider_import_failure;
use crate::provider::importer::provider_cursor_stream;
use crate::provider::native::{
    open_provider_sqlite_readonly, provider_capped_json, provider_policy_body,
    provider_policy_event_text, provider_role, task_json_string_field, task_json_time_field,
};
use crate::provider::sqlite::{
    ensure_sqlite_table_columns, sqlite_table_columns, sqlite_table_exists,
};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderNormalizationResult, Result,
    MAX_PROVIDER_JSONL_LINE_BYTES, PROVIDER_MAX_PREVIEW_CHARS,
};

pub(crate) const TRAE_STATE_VSCDB_SOURCE_FORMAT: &str = "trae_state_vscdb";
pub(crate) const TRAE_CN_INPUT_HISTORY_KEY: &str = "icube-ai-agent-storage-input-history";
pub(crate) const TRAE_CHAT_KEYS: &[&str] = &[
    "memento/icube-ai-agent-storage",
    TRAE_CN_INPUT_HISTORY_KEY,
    "chat.ChatSessionStore.index",
    "ChatStore",
    "memento/icube-ai-chat-storage-7467774676505887760",
    "memento/icube-ai-ng-chat-storage-7467774676505887760",
];

pub(crate) fn normalize_trae_history(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    let mut db_paths = collect_trae_state_vscdb_paths(path)?;
    db_paths.sort();
    db_paths.dedup();
    if db_paths.is_empty() {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "no Trae state.vscdb files found",
        });
    }

    let mut merged = ProviderNormalizationResult::default();
    for (workspace_ordinal, db_path) in db_paths.iter().enumerate() {
        let mut result = normalize_trae_state_vscdb(db_path, context, workspace_ordinal + 1)?;
        merged.summary.merge(result.summary);
        merged.captures.append(&mut result.captures);
        merged.files_touched.append(&mut result.files_touched);
    }
    if merged.captures.is_empty() && merged.summary.failed == 0 {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "no Trae chat sessions with messages were found",
        });
    }
    Ok(merged)
}

pub(crate) fn collect_trae_state_vscdb_paths(path: &Path) -> Result<Vec<PathBuf>> {
    let metadata = fs::symlink_metadata(path)?;
    let file_type = metadata.file_type();
    if file_type.is_file() {
        if path.file_name().and_then(|name| name.to_str()) != Some("state.vscdb") {
            return Ok(Vec::new());
        }
        ensure_regular_provider_transcript_file(path)?;
        return Ok(vec![path.to_path_buf()]);
    }
    if !file_type.is_dir() {
        return Ok(Vec::new());
    }
    ensure_provider_path_parents_are_not_symlinks(path)?;

    let direct = path.join("state.vscdb");
    if direct.is_file() {
        ensure_regular_provider_transcript_file(&direct)?;
        return Ok(vec![direct]);
    }

    let mut paths = Vec::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let candidate = entry.path().join("state.vscdb");
        if candidate.is_file() {
            ensure_regular_provider_transcript_file(&candidate)?;
            paths.push(candidate);
        }
    }
    Ok(paths)
}

pub(crate) fn normalize_trae_state_vscdb(
    path: &Path,
    context: &ProviderAdapterContext,
    workspace_ordinal: usize,
) -> Result<ProviderNormalizationResult> {
    let conn = open_provider_sqlite_readonly(path)?;
    if !sqlite_table_exists(&conn, "ItemTable")? {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Trae state.vscdb is missing ItemTable",
        });
    }
    let columns = sqlite_table_columns(&conn, "ItemTable")?;
    ensure_sqlite_table_columns(&columns, "Trae ItemTable", &["key", "value"])?;

    let chat_rows = trae_chat_rows(&conn)?;
    let workspace_id = path
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("state-vscdb")
        .to_owned();
    let workspace_folder = trae_workspace_folder(path);
    let raw_source_path = path.display().to_string();
    let mut result = ProviderNormalizationResult::default();

    for (key_index, row) in chat_rows.into_iter().enumerate() {
        let line_base = workspace_ordinal
            .saturating_mul(1_000_000)
            .saturating_add(key_index.saturating_mul(10_000));
        let chat_data = match serde_json::from_str::<Value>(&row.value) {
            Ok(value) => value,
            Err(err) => {
                push_provider_import_failure(
                    &mut result.summary,
                    line_base.saturating_add(1),
                    format!(
                        "Trae ItemTable key `{}` contains invalid JSON: {err}",
                        row.key
                    ),
                );
                continue;
            }
        };
        let sessions = trae_session_entries(&chat_data, &row.key);
        for (session_index, session) in sessions.into_iter().enumerate() {
            let Some(messages) = trae_session_messages(&session) else {
                continue;
            };
            if messages.is_empty() {
                continue;
            }
            let native_session_id = trae_session_id(&session, session_index);
            let provider_session_id = format!("{workspace_id}/{native_session_id}");
            let events = trae_events_from_messages(
                &provider_session_id,
                &workspace_id,
                &row.key,
                &messages,
                context.imported_at,
                line_base.saturating_add(session_index.saturating_mul(1_000)),
            );
            if events.is_empty() {
                continue;
            }
            let first_event_at = events
                .first()
                .map(|event| event.occurred_at)
                .unwrap_or(context.imported_at);
            let last_event_at = events.last().map(|event| event.occurred_at);
            let started_at =
                task_json_time_field(&session, &["createdAt", "created_at", "timestamp", "time"])
                    .unwrap_or(first_event_at);
            let ended_at =
                task_json_time_field(&session, &["updatedAt", "updated_at", "lastModified"])
                    .or(last_event_at);
            let title = task_json_string_field(&session, &["title", "name"])
                .or_else(|| trae_generated_title(&events));

            for event in events {
                let line = event.line_number;
                result.captures.push((
                    line,
                    trae_capture(TraeCaptureInput {
                        provider_session_id: &provider_session_id,
                        native_session_id: &native_session_id,
                        workspace_id: &workspace_id,
                        workspace_folder: workspace_folder.as_deref(),
                        raw_source_path: &raw_source_path,
                        chat_key: &row.key,
                        session: &session,
                        context,
                        started_at,
                        ended_at,
                        title: title.clone(),
                        event,
                    }),
                ));
            }
        }
    }

    Ok(result)
}

#[derive(Debug, Clone)]
pub(crate) struct TraeChatRow {
    pub(crate) key: String,
    pub(crate) value: String,
}

pub(crate) fn trae_chat_rows(conn: &Connection) -> Result<Vec<TraeChatRow>> {
    let mut rows = Vec::new();
    for key in TRAE_CHAT_KEYS {
        let value = conn
            .query_row(
                "select value from ItemTable where [key] = ?1",
                [key],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if let Some(value) = value {
            rows.push(TraeChatRow {
                key: (*key).to_owned(),
                value,
            });
        }
    }
    Ok(rows)
}

pub(crate) fn trae_workspace_folder(path: &Path) -> Option<String> {
    let workspace_json = path.parent()?.join("workspace.json");
    let value = read_json_file_limited(
        &workspace_json,
        MAX_PROVIDER_JSONL_LINE_BYTES,
        "Trae workspace.json",
    )
    .ok()?;
    task_json_string_field(&value, &["folder", "workspace", "path"])
        .map(|folder| trae_workspace_folder_label(&folder))
}

pub(crate) fn trae_workspace_folder_label(folder: &str) -> String {
    let Some(path) = folder.strip_prefix("file://") else {
        return folder.to_owned();
    };
    percent_decode_uri_path(path)
}

pub(crate) fn percent_decode_uri_path(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hi = (bytes[index + 1] as char).to_digit(16);
            let lo = (bytes[index + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push(((hi << 4) | lo) as u8);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| value.to_owned())
}

pub(crate) fn trae_session_entries(value: &Value, key: &str) -> Vec<Value> {
    if key == "memento/icube-ai-agent-storage" {
        if let Some(items) = value.get("list").and_then(Value::as_array) {
            return items.clone();
        }
    }
    if key == TRAE_CN_INPUT_HISTORY_KEY {
        if let Some(items) = value.as_array() {
            return vec![json!({
                "id": "trae-cn-input-history",
                "title": "Trae CN input history",
                "messages": items,
            })];
        }
    }
    if key == "ChatStore" {
        for field in ["sessions", "entries", "conversations", "list"] {
            if let Some(entries) = trae_entries_from_field(value.get(field)) {
                return entries;
            }
        }
        if let Some(items) = value.as_array() {
            return items.clone();
        }
    }
    for field in ["entries", "sessions", "conversations", "list"] {
        if let Some(entries) = trae_entries_from_field(value.get(field)) {
            return entries;
        }
    }
    if let Some(items) = value.as_array() {
        return items.clone();
    }
    Vec::new()
}

pub(crate) fn trae_entries_from_field(value: Option<&Value>) -> Option<Vec<Value>> {
    match value? {
        Value::Array(items) => Some(items.clone()),
        Value::Object(map) => Some(map.values().cloned().collect()),
        _ => None,
    }
}

pub(crate) fn trae_session_messages(session: &Value) -> Option<Vec<Value>> {
    for field in ["messages", "chatMessages", "bubbles", "items"] {
        if let Some(messages) = session.get(field).and_then(Value::as_array) {
            return Some(messages.clone());
        }
    }
    None
}

pub(crate) fn trae_session_id(session: &Value, index: usize) -> String {
    task_json_string_field(
        session,
        &[
            "sessionId",
            "session_id",
            "id",
            "conversationId",
            "conversation_id",
        ],
    )
    .unwrap_or_else(|| format!("session-{}", index.saturating_add(1)))
}

#[derive(Debug, Clone)]
pub(crate) struct TraeEventInput {
    pub(crate) line_number: usize,
    pub(crate) provider_event_index: u64,
    pub(crate) native_message_id: String,
    pub(crate) role: Option<String>,
    pub(crate) occurred_at: DateTime<Utc>,
    pub(crate) text: String,
    pub(crate) raw_message: Value,
}

pub(crate) fn trae_events_from_messages(
    provider_session_id: &str,
    workspace_id: &str,
    chat_key: &str,
    messages: &[Value],
    fallback_time: DateTime<Utc>,
    line_base: usize,
) -> Vec<TraeEventInput> {
    let mut events = Vec::new();
    for (message_index, message) in messages.iter().enumerate() {
        let Some(text) = trae_message_text(message) else {
            continue;
        };
        if text.trim().is_empty() {
            continue;
        }
        let native_message_id = task_json_string_field(
            message,
            &[
                "id",
                "messageId",
                "message_id",
                "uuid",
                "requestId",
                "responseId",
            ],
        )
        .unwrap_or_else(|| {
            format!("{workspace_id}:{provider_session_id}:{chat_key}:{message_index}")
        });
        let occurred_at = task_json_time_field(
            message,
            &["createdAt", "created_at", "timestamp", "time", "date"],
        )
        .unwrap_or(fallback_time);
        let mut role = task_json_string_field(message, &["role", "type", "sender"]);
        if chat_key == TRAE_CN_INPUT_HISTORY_KEY && role.is_none() {
            role = Some("user".to_owned());
        }
        events.push(TraeEventInput {
            line_number: line_base.saturating_add(message_index).saturating_add(1),
            provider_event_index: message_index as u64,
            native_message_id,
            role,
            occurred_at,
            text,
            raw_message: message.clone(),
        });
    }
    events
}

pub(crate) fn trae_message_text(message: &Value) -> Option<String> {
    for field in [
        "content",
        "inputText",
        "text",
        "message",
        "summary",
        "answer",
        "query",
        "parsedQuery",
    ] {
        if let Some(text) = message.get(field).and_then(trae_content_text) {
            return Some(text);
        }
    }
    message
        .pointer("/data/summary")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

pub(crate) fn trae_content_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.trim().to_owned()),
        Value::Array(items) => {
            let parts = items
                .iter()
                .filter_map(trae_content_text)
                .filter(|text| !text.trim().is_empty())
                .collect::<Vec<_>>();
            (!parts.is_empty()).then(|| parts.join("\n"))
        }
        Value::Object(map) => {
            for field in ["text", "content", "value", "summary"] {
                if let Some(text) = map.get(field).and_then(trae_content_text) {
                    return Some(text);
                }
            }
            None
        }
        _ => None,
    }
}

pub(crate) fn trae_generated_title(events: &[TraeEventInput]) -> Option<String> {
    events
        .iter()
        .find(|event| provider_role(event.role.as_deref()) == EventRole::User)
        .or_else(|| events.first())
        .map(|event| event.text.replace('\n', " "))
        .map(|title| title.chars().take(50).collect::<String>())
        .filter(|title| !title.trim().is_empty())
}

pub(crate) struct TraeCaptureInput<'a> {
    pub(crate) provider_session_id: &'a str,
    pub(crate) native_session_id: &'a str,
    pub(crate) workspace_id: &'a str,
    pub(crate) workspace_folder: Option<&'a str>,
    pub(crate) raw_source_path: &'a str,
    pub(crate) chat_key: &'a str,
    pub(crate) session: &'a Value,
    pub(crate) context: &'a ProviderAdapterContext,
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) ended_at: Option<DateTime<Utc>>,
    pub(crate) title: Option<String>,
    pub(crate) event: TraeEventInput,
}

pub(crate) fn trae_capture(input: TraeCaptureInput<'_>) -> ProviderCaptureEnvelope {
    let event_envelope = trae_event(
        input.provider_session_id,
        input.workspace_id,
        input.chat_key,
        &input.event,
    );
    ProviderCaptureEnvelope {
        schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
        provider: CaptureProvider::Trae,
        source: ProviderSourceEnvelope {
            source_format: TRAE_STATE_VSCDB_SOURCE_FORMAT.to_owned(),
            machine_id: input.context.machine_id.clone(),
            observed_at: input.context.imported_at,
            raw_source_path: Some(input.raw_source_path.to_owned()),
            source_root: input
                .context
                .source_root_display()
                .or_else(|| Some(input.raw_source_path.to_owned())),
            trust: ProviderSourceTrust::ProviderNative,
            fidelity: Fidelity::Partial,
            cursor: Some(ProviderCursorRange {
                before: None,
                after: Some(ProviderCursorCheckpoint {
                    stream: provider_cursor_stream(
                        CaptureProvider::Trae,
                        TRAE_STATE_VSCDB_SOURCE_FORMAT,
                    ),
                    cursor: event_envelope
                        .cursor
                        .clone()
                        .unwrap_or_else(|| input.provider_session_id.to_owned()),
                    observed_at: event_envelope.occurred_at,
                }),
            }),
            idempotency_key: Some(format!(
                "provider-source:trae:{TRAE_STATE_VSCDB_SOURCE_FORMAT}:{}",
                input.provider_session_id
            )),
            metadata: json!({
                "adapter": TRAE_STATE_VSCDB_SOURCE_FORMAT,
                "chat_key": input.chat_key,
                "native_workspace_id": input.workspace_id,
                "schema_proof": "yuanjing001/trae-chats-exporter src/extension.ts and src/utils.ts read Trae User/workspaceStorage/*/state.vscdb ItemTable keys",
                "native_auto_scope": "Trae and Trae CN User/workspaceStorage roots with known ItemTable chat keys",
            }),
        },
        session: ProviderSessionEnvelope {
            provider_session_id: input.provider_session_id.to_owned(),
            parent_provider_session_id: None,
            root_provider_session_id: None,
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".to_owned()),
            is_primary: true,
            status: SessionStatus::Imported,
            started_at: input.started_at,
            ended_at: input.ended_at,
            cwd: input.workspace_folder.map(str::to_owned),
            fidelity: Fidelity::Partial,
            idempotency_key: Some(format!(
                "provider-session:trae:{}",
                input.provider_session_id
            )),
            artifacts: Vec::new(),
            metadata: json!({
                "source_format": TRAE_STATE_VSCDB_SOURCE_FORMAT,
                "provider": CaptureProvider::Trae.as_str(),
                "display_name": "Trae",
                "title": input.title,
                "native_workspace_id": input.workspace_id,
                "native_session_id": input.native_session_id,
                "workspace_folder": input.workspace_folder,
                "chat_key": input.chat_key,
                "session": provider_capped_json(
                    &trae_session_metadata_preview(input.session),
                    PROVIDER_MAX_PREVIEW_CHARS,
                ),
                "limitations": [
                    "Importer is based on public exporter source and synthetic fixture; no real local Trae run fixture is bundled",
                    "Only known Trae and Trae CN ItemTable chat keys and direct message arrays are imported",
                    "Trae CN input-history rows are usually user prompts only and may not include assistant replies"
                ],
            }),
        },
        event: Some(event_envelope),
    }
}

fn trae_session_metadata_preview(session: &Value) -> Value {
    let mut preview = session.clone();
    if let Value::Object(object) = &mut preview {
        for key in ["messages", "chatMessages", "bubbles", "items"] {
            object.remove(key);
        }
    }
    provider_policy_body(EventType::Notice, &preview)
}

pub(crate) fn trae_event(
    provider_session_id: &str,
    workspace_id: &str,
    chat_key: &str,
    event: &TraeEventInput,
) -> ProviderEventEnvelope {
    let event_type = EventType::Message;
    let retained_text = provider_policy_event_text(event_type, &event.text, &event.raw_message);
    let event_id = format!("{provider_session_id}:{}", event.native_message_id);
    ProviderEventEnvelope {
        provider_event_index: event.provider_event_index,
        provider_event_hash: Some(event_id.clone()),
        cursor: Some(format!("{chat_key}:{event_id}")),
        event_type,
        role: Some(provider_role(event.role.as_deref())),
        occurred_at: event.occurred_at,
        fidelity: Fidelity::Partial,
        idempotency_key: Some(format!(
            "provider-event:trae:{TRAE_STATE_VSCDB_SOURCE_FORMAT}:{event_id}"
        )),
        artifacts: Vec::new(),
        payload: json!({
            "event_id": event_id,
            "native_workspace_id": workspace_id,
            "native_message_id": event.native_message_id,
            "text": retained_text.text,
            "text_retention": retained_text.retention.as_json(),
            "body": provider_capped_json(&provider_policy_body(event_type, &event.raw_message), PROVIDER_MAX_PREVIEW_CHARS),
        }),
        metadata: json!({
            "source": "trae_state_vscdb_itemtable",
            "source_format": TRAE_STATE_VSCDB_SOURCE_FORMAT,
            "chat_key": chat_key,
            "native_message_id": event.native_message_id,
            "role": event.role,
            "model": task_json_string_field(&event.raw_message, &["model", "modelType", "model_id"]),
        }),
    }
}
