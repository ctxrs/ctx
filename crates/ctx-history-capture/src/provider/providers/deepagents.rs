use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventRole, EventType, Fidelity, ProviderCaptureEnvelope,
    ProviderCursorCheckpoint, ProviderCursorRange, ProviderEventEnvelope, ProviderRawRetention,
    ProviderRedactionBoundary, ProviderSessionEnvelope, ProviderSourceEnvelope,
    ProviderSourceTrust, SessionStatus, PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
};
use rmpv::{decode::read_value as read_msgpack_value, Value as MsgpackValue};
use rusqlite::Connection;
use serde_json::{json, Value};

use crate::common::time::parse_rfc3339_utc;
use crate::provider::importer::provider_cursor_stream;
use crate::provider::native::{
    native_event, open_provider_sqlite_readonly, provider_line_from_index, NativeEventDraft,
};
use crate::provider::sqlite::{
    ensure_sqlite_table_columns, opencode_schema_fingerprint, sqlite_table_columns,
    sqlite_table_exists,
};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderImportFailure, ProviderNormalizationResult,
    Result, DEEPAGENTS_SQLITE_SOURCE_FORMAT,
};

pub(crate) struct DeepAgentsThread {
    pub(crate) thread_id: String,
    pub(crate) agent_name: Option<String>,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) updated_at: DateTime<Utc>,
    pub(crate) latest_checkpoint_id: Option<String>,
    pub(crate) git_branch: Option<String>,
    pub(crate) cwd: Option<String>,
    pub(crate) checkpoint_times: BTreeMap<String, DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub(crate) struct DeepAgentsWriteRow {
    pub(crate) thread_id: String,
    pub(crate) checkpoint_id: String,
    pub(crate) task_id: String,
    pub(crate) idx: i64,
    pub(crate) value_type: Option<String>,
    pub(crate) value: Vec<u8>,
    pub(crate) row_number: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct DeepAgentsMessage {
    pub(crate) role: EventRole,
    pub(crate) message_type: String,
    pub(crate) message_class: Option<String>,
    pub(crate) message_id: Option<String>,
    pub(crate) text: String,
}

#[derive(Debug, Clone)]
pub(crate) struct DeepAgentsEventDraft {
    pub(crate) thread_id: String,
    pub(crate) provider_event_index: u64,
    pub(crate) cursor: String,
    pub(crate) occurred_at: DateTime<Utc>,
    pub(crate) message: DeepAgentsMessage,
    pub(crate) checkpoint_id: String,
    pub(crate) task_id: String,
    pub(crate) write_idx: i64,
    pub(crate) message_offset: usize,
}

pub(crate) fn normalize_deepagents_sqlite(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    let conn = open_provider_sqlite_readonly(path)?;
    if !sqlite_table_exists(&conn, "checkpoints")? {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Deep Agents sessions.db is missing required checkpoints table",
        });
    }
    if !sqlite_table_exists(&conn, "writes")? {
        return Err(CaptureError::InvalidProviderTranscriptPath {
            path: path.to_path_buf(),
            reason: "Deep Agents sessions.db is missing required writes table",
        });
    }

    let checkpoint_columns = sqlite_table_columns(&conn, "checkpoints")?;
    ensure_sqlite_table_columns(
        &checkpoint_columns,
        "Deep Agents checkpoints table",
        &[
            "thread_id",
            "checkpoint_ns",
            "checkpoint_id",
            "checkpoint",
            "metadata",
        ],
    )?;
    let write_columns = sqlite_table_columns(&conn, "writes")?;
    ensure_sqlite_table_columns(
        &write_columns,
        "Deep Agents writes table",
        &[
            "thread_id",
            "checkpoint_ns",
            "checkpoint_id",
            "task_id",
            "idx",
            "channel",
            "type",
            "value",
        ],
    )?;

    let user_version = conn.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))?;
    let schema_fingerprint = opencode_schema_fingerprint(&conn)?;
    let threads = deepagents_threads(&conn, context)?;
    let write_rows = deepagents_message_write_rows(&conn)?;
    let mut result = ProviderNormalizationResult::default();
    let events_by_thread = deepagents_events_by_thread(write_rows, &threads, &mut result)?;
    let raw_source_path = context
        .source_path
        .as_ref()
        .map(|path| path.display().to_string());

    for thread in threads.values() {
        let events = events_by_thread.get(&thread.thread_id);
        if let Some(events) = events {
            for event in events {
                let line = provider_line_from_index(event.provider_event_index);
                result.captures.push((
                    line,
                    deepagents_capture(
                        thread,
                        Some(event),
                        context,
                        raw_source_path.clone(),
                        user_version,
                        &schema_fingerprint,
                    ),
                ));
            }
        } else {
            result.captures.push((
                0,
                deepagents_capture(
                    thread,
                    None,
                    context,
                    raw_source_path.clone(),
                    user_version,
                    &schema_fingerprint,
                ),
            ));
        }
    }

    Ok(result)
}

pub(crate) fn deepagents_threads(
    conn: &Connection,
    context: &ProviderAdapterContext,
) -> Result<BTreeMap<String, DeepAgentsThread>> {
    let mut stmt = conn.prepare(
        "select thread_id, checkpoint_id, metadata \
         from checkpoints \
         where checkpoint_ns = '' \
         order by thread_id, checkpoint_id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<Vec<u8>>>(2)?,
        ))
    })?;
    let mut threads = BTreeMap::<String, DeepAgentsThread>::new();
    for row in rows {
        let (thread_id, checkpoint_id, metadata_blob) = row?;
        let metadata = deepagents_metadata_json(metadata_blob.as_deref());
        let updated_at =
            deepagents_metadata_time(&metadata, "updated_at").unwrap_or(context.imported_at);
        let entry = threads
            .entry(thread_id.clone())
            .or_insert_with(|| DeepAgentsThread {
                thread_id: thread_id.clone(),
                agent_name: deepagents_metadata_string(&metadata, "agent_name"),
                created_at: updated_at,
                updated_at,
                latest_checkpoint_id: Some(checkpoint_id.clone()),
                git_branch: deepagents_metadata_string(&metadata, "git_branch"),
                cwd: deepagents_metadata_string(&metadata, "cwd"),
                checkpoint_times: BTreeMap::new(),
            });
        if updated_at < entry.created_at {
            entry.created_at = updated_at;
        }
        if updated_at >= entry.updated_at {
            entry.updated_at = updated_at;
            entry.latest_checkpoint_id = Some(checkpoint_id.clone());
            entry.agent_name = deepagents_metadata_string(&metadata, "agent_name")
                .or_else(|| entry.agent_name.clone());
            entry.git_branch = deepagents_metadata_string(&metadata, "git_branch")
                .or_else(|| entry.git_branch.clone());
            entry.cwd = deepagents_metadata_string(&metadata, "cwd").or_else(|| entry.cwd.clone());
        }
        entry.checkpoint_times.insert(checkpoint_id, updated_at);
    }
    Ok(threads)
}

pub(crate) fn deepagents_message_write_rows(conn: &Connection) -> Result<Vec<DeepAgentsWriteRow>> {
    let mut stmt = conn.prepare(
        "select thread_id, checkpoint_id, task_id, idx, type, value \
         from writes \
         where checkpoint_ns = '' and channel = 'messages' \
         order by thread_id, checkpoint_id, task_id, idx",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(DeepAgentsWriteRow {
            thread_id: row.get(0)?,
            checkpoint_id: row.get(1)?,
            task_id: row.get(2)?,
            idx: row.get(3)?,
            value_type: row.get(4)?,
            value: row.get(5)?,
            row_number: 0,
        })
    })?;
    let mut out = Vec::new();
    for (index, row) in rows.enumerate() {
        let mut row = row?;
        row.row_number = u64::try_from(index + 1).unwrap_or(u64::MAX);
        out.push(row);
    }
    Ok(out)
}

pub(crate) fn deepagents_events_by_thread(
    rows: Vec<DeepAgentsWriteRow>,
    threads: &BTreeMap<String, DeepAgentsThread>,
    result: &mut ProviderNormalizationResult,
) -> Result<BTreeMap<String, Vec<DeepAgentsEventDraft>>> {
    let mut events = BTreeMap::<String, Vec<DeepAgentsEventDraft>>::new();
    let mut next_index = BTreeMap::<String, u64>::new();
    let mut seen_message_ids = BTreeMap::<String, BTreeSet<String>>::new();

    for row in rows {
        let Some(thread) = threads.get(&row.thread_id) else {
            result.summary.failed += 1;
            result.summary.failures.push(ProviderImportFailure {
                line: provider_line_from_index(row.row_number),
                error: format!(
                    "Deep Agents writes row references unknown thread_id {}",
                    row.thread_id
                ),
            });
            continue;
        };
        let decoded = match deepagents_messages_from_blob(row.value_type.as_deref(), &row.value) {
            Ok(messages) => messages,
            Err(err) => {
                result.summary.failed += 1;
                result.summary.failures.push(ProviderImportFailure {
                    line: provider_line_from_index(row.row_number),
                    error: err.to_string(),
                });
                continue;
            }
        };
        for (message_offset, message) in decoded.into_iter().enumerate() {
            if let Some(message_id) = &message.message_id {
                let seen = seen_message_ids.entry(row.thread_id.clone()).or_default();
                if !seen.insert(message_id.clone()) {
                    continue;
                }
            }
            let provider_event_index = next_index
                .entry(row.thread_id.clone())
                .and_modify(|index| *index += 1)
                .or_insert(1);
            let occurred_at = thread
                .checkpoint_times
                .get(&row.checkpoint_id)
                .copied()
                .unwrap_or(thread.updated_at);
            let cursor = format!(
                "thread:{}:checkpoint:{}:task:{}:write:{}:message:{}",
                row.thread_id, row.checkpoint_id, row.task_id, row.idx, message_offset
            );
            events
                .entry(row.thread_id.clone())
                .or_default()
                .push(DeepAgentsEventDraft {
                    thread_id: row.thread_id.clone(),
                    provider_event_index: *provider_event_index,
                    cursor,
                    occurred_at,
                    message,
                    checkpoint_id: row.checkpoint_id.clone(),
                    task_id: row.task_id.clone(),
                    write_idx: row.idx,
                    message_offset,
                });
        }
    }

    Ok(events)
}

pub(crate) fn deepagents_capture(
    thread: &DeepAgentsThread,
    event: Option<&DeepAgentsEventDraft>,
    context: &ProviderAdapterContext,
    raw_source_path: Option<String>,
    sqlite_user_version: i64,
    schema_fingerprint: &str,
) -> ProviderCaptureEnvelope {
    let observed_at = event
        .map(|event| event.occurred_at)
        .unwrap_or(thread.updated_at);
    let cursor = event.map(|event| event.cursor.clone()).or_else(|| {
        thread
            .latest_checkpoint_id
            .as_ref()
            .map(|checkpoint_id| format!("thread:{}:checkpoint:{checkpoint_id}", thread.thread_id))
    });
    ProviderCaptureEnvelope {
        schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
        provider: CaptureProvider::DeepAgents,
        source: ProviderSourceEnvelope {
            source_format: DEEPAGENTS_SQLITE_SOURCE_FORMAT.to_owned(),
            machine_id: context.machine_id.clone(),
            observed_at,
            raw_source_path: raw_source_path.clone(),
            source_root: context.source_root_display().or(raw_source_path.clone()),
            raw_retention: ProviderRawRetention::PathReference,
            redaction_boundary: ProviderRedactionBoundary::BeforeExport,
            trust: ProviderSourceTrust::ProviderNative,
            fidelity: Fidelity::Imported,
            cursor: cursor.clone().map(|cursor| ProviderCursorRange {
                before: None,
                after: Some(ProviderCursorCheckpoint {
                    stream: provider_cursor_stream(
                        CaptureProvider::DeepAgents,
                        DEEPAGENTS_SQLITE_SOURCE_FORMAT,
                    ),
                    cursor,
                    observed_at,
                }),
            }),
            idempotency_key: Some(format!(
                "provider-source:deepagents:{DEEPAGENTS_SQLITE_SOURCE_FORMAT}:{}",
                thread.thread_id
            )),
            metadata: json!({
                "adapter": DEEPAGENTS_SQLITE_SOURCE_FORMAT,
                "sqlite_user_version": sqlite_user_version,
                "schema_fingerprint": schema_fingerprint,
                "message_import_policy": "root writes.messages only; checkpoint state blobs are not indexed",
            }),
        },
        session: ProviderSessionEnvelope {
            provider_session_id: thread.thread_id.clone(),
            parent_provider_session_id: None,
            root_provider_session_id: None,
            external_agent_id: thread.agent_name.clone(),
            agent_type: AgentType::Primary,
            role_hint: thread
                .agent_name
                .clone()
                .or_else(|| Some("agent".to_owned())),
            is_primary: true,
            status: SessionStatus::Imported,
            started_at: thread.created_at,
            ended_at: Some(thread.updated_at),
            cwd: thread.cwd.clone(),
            fidelity: Fidelity::Imported,
            idempotency_key: Some(format!("provider-session:deepagents:{}", thread.thread_id)),
            artifacts: Vec::new(),
            metadata: json!({
                "source_format": DEEPAGENTS_SQLITE_SOURCE_FORMAT,
                "agent_name": thread.agent_name,
                "git_branch": thread.git_branch,
                "latest_checkpoint_id": thread.latest_checkpoint_id,
                "storage": "LangGraph AsyncSqliteSaver checkpoints/writes",
            }),
        },
        event: event.map(deepagents_event),
    }
}

pub(crate) fn deepagents_event(event: &DeepAgentsEventDraft) -> ProviderEventEnvelope {
    let event_type = if event.message.role == EventRole::Tool {
        EventType::ToolOutput
    } else {
        EventType::Message
    };
    native_event(NativeEventDraft {
        provider: CaptureProvider::DeepAgents,
        source_format: DEEPAGENTS_SQLITE_SOURCE_FORMAT,
        provider_session_id: event.thread_id.clone(),
        provider_event_index: event.provider_event_index,
        provider_event_hash: Some(event.cursor.clone()),
        cursor: event.cursor.clone(),
        event_type,
        role: Some(event.message.role),
        occurred_at: event.occurred_at,
        text: event.message.text.clone(),
        body: json!({
            "message_type": event.message.message_type,
            "message_class": event.message.message_class,
            "message_id": event.message.message_id,
            "checkpoint_id": event.checkpoint_id,
            "task_id": event.task_id,
            "write_idx": event.write_idx,
            "message_offset": event.message_offset,
        }),
        metadata: json!({
            "source": DEEPAGENTS_SQLITE_SOURCE_FORMAT,
            "source_format": DEEPAGENTS_SQLITE_SOURCE_FORMAT,
            "checkpoint_id": event.checkpoint_id,
            "task_id": event.task_id,
            "write_idx": event.write_idx,
            "message_offset": event.message_offset,
            "message_type": event.message.message_type,
            "message_class": event.message.message_class,
            "message_id": event.message.message_id,
            "privacy": "decoded from writes.messages only",
        }),
    })
}

pub(crate) fn deepagents_messages_from_blob(
    value_type: Option<&str>,
    value: &[u8],
) -> Result<Vec<DeepAgentsMessage>> {
    match value_type {
        Some("msgpack") => {
            let decoded = deepagents_decode_msgpack(value)?;
            Ok(deepagents_messages_from_msgpack_value(&decoded))
        }
        Some(other) => Err(CaptureError::InvalidPayload(format!(
            "unsupported Deep Agents writes.messages value type {other:?}"
        ))),
        None => Err(CaptureError::InvalidPayload(
            "Deep Agents writes.messages row has no value type".to_owned(),
        )),
    }
}

pub(crate) fn deepagents_decode_msgpack(value: &[u8]) -> Result<MsgpackValue> {
    let mut cursor = std::io::Cursor::new(value);
    read_msgpack_value(&mut cursor).map_err(|err| {
        CaptureError::InvalidPayload(format!("invalid Deep Agents msgpack payload: {err}"))
    })
}

pub(crate) fn deepagents_messages_from_msgpack_value(
    value: &MsgpackValue,
) -> Vec<DeepAgentsMessage> {
    match value {
        MsgpackValue::Array(items) => items
            .iter()
            .filter_map(deepagents_message_from_msgpack_value)
            .collect(),
        _ => deepagents_message_from_msgpack_value(value)
            .into_iter()
            .collect(),
    }
}

pub(crate) fn deepagents_message_from_msgpack_value(
    value: &MsgpackValue,
) -> Option<DeepAgentsMessage> {
    match value {
        MsgpackValue::Map(fields) => deepagents_message_from_fields(fields, None),
        MsgpackValue::Ext(5, payload) => {
            let decoded = deepagents_decode_msgpack(payload).ok()?;
            let MsgpackValue::Array(items) = decoded else {
                return None;
            };
            let class_name = items.get(1).and_then(msgpack_string);
            let fields = match items.get(2)? {
                MsgpackValue::Map(fields) => fields,
                _ => return None,
            };
            deepagents_message_from_fields(fields, class_name)
        }
        _ => None,
    }
}

pub(crate) fn deepagents_message_from_fields(
    fields: &[(MsgpackValue, MsgpackValue)],
    class_name: Option<String>,
) -> Option<DeepAgentsMessage> {
    let message_type = msgpack_map_string(fields, "type")
        .or_else(|| msgpack_map_string(fields, "role"))
        .or_else(|| class_name.clone())
        .unwrap_or_else(|| "unknown".to_owned());
    let role = deepagents_message_role(&message_type, class_name.as_deref())?;
    if role == EventRole::System {
        return None;
    }
    let content = msgpack_map_get(fields, "content")?;
    let text = deepagents_content_text(content)?;
    if text.trim().is_empty() || text.starts_with("[SYSTEM]") {
        return None;
    }
    Some(DeepAgentsMessage {
        role,
        message_type,
        message_class: class_name,
        message_id: msgpack_map_string(fields, "id"),
        text,
    })
}

pub(crate) fn deepagents_message_role(
    message_type: &str,
    class_name: Option<&str>,
) -> Option<EventRole> {
    let lowered = message_type.to_ascii_lowercase();
    match lowered.as_str() {
        "human" | "user" => Some(EventRole::User),
        "ai" | "assistant" => Some(EventRole::Assistant),
        "tool" => Some(EventRole::Tool),
        "system" => Some(EventRole::System),
        _ => match class_name.unwrap_or_default() {
            "HumanMessage" => Some(EventRole::User),
            "AIMessage" => Some(EventRole::Assistant),
            "ToolMessage" => Some(EventRole::Tool),
            "SystemMessage" => Some(EventRole::System),
            _ => None,
        },
    }
}

pub(crate) fn deepagents_content_text(value: &MsgpackValue) -> Option<String> {
    if let Some(text) = msgpack_string(value) {
        return Some(text);
    }
    if let MsgpackValue::Array(items) = value {
        let parts = items
            .iter()
            .filter_map(|item| match item {
                MsgpackValue::Map(fields) => msgpack_map_string(fields, "text"),
                _ => msgpack_string(item),
            })
            .collect::<Vec<_>>();
        let joined = parts.join(" ").trim().to_owned();
        if !joined.is_empty() {
            return Some(joined);
        }
    }
    None
}

pub(crate) fn msgpack_map_get<'a>(
    fields: &'a [(MsgpackValue, MsgpackValue)],
    key: &str,
) -> Option<&'a MsgpackValue> {
    fields.iter().find_map(|(field_key, field_value)| {
        (msgpack_string(field_key).as_deref() == Some(key)).then_some(field_value)
    })
}

pub(crate) fn msgpack_map_string(
    fields: &[(MsgpackValue, MsgpackValue)],
    key: &str,
) -> Option<String> {
    msgpack_map_get(fields, key).and_then(msgpack_string)
}

pub(crate) fn msgpack_string(value: &MsgpackValue) -> Option<String> {
    match value {
        MsgpackValue::String(text) => text.as_str().map(str::to_owned),
        _ => None,
    }
}

pub(crate) fn deepagents_metadata_json(blob: Option<&[u8]>) -> Value {
    blob.and_then(|blob| serde_json::from_slice::<Value>(blob).ok())
        .unwrap_or_else(|| json!({}))
}

pub(crate) fn deepagents_metadata_string(metadata: &Value, key: &str) -> Option<String> {
    metadata
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

pub(crate) fn deepagents_metadata_time(metadata: &Value, key: &str) -> Option<DateTime<Utc>> {
    metadata
        .get(key)
        .and_then(Value::as_str)
        .and_then(parse_rfc3339_utc)
}
