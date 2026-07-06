#[allow(unused_imports)]
use super::*;

#[derive(Debug, Clone)]
pub struct WarpSqliteImportOptions {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub history_record_id: Option<Uuid>,
    pub allow_partial_failures: bool,
}

impl Default for WarpSqliteImportOptions {
    fn default() -> Self {
        Self {
            machine_id: default_machine_id(),
            source_path: None,
            imported_at: utc_now(),
            history_record_id: None,
            allow_partial_failures: false,
        }
    }
}

pub fn import_warp_sqlite(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: WarpSqliteImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = normalize_warp_sqlite(
        path,
        &ProviderAdapterContext {
            machine_id: options.machine_id,
            source_path: Some(source_path),
            imported_at: options.imported_at,
            tool_output_mode: CodexToolOutputMode::Full,
            event_mode: CodexEventImportMode::Rich,
            include_notices: true,
        },
    )?;
    import_normalized_provider_captures(
        store,
        normalization,
        NormalizedProviderImportOptions {
            history_record_id: options.history_record_id,
            allow_partial_failures: options.allow_partial_failures,
            persist_cursors: true,
            wrap_transaction: true,
            fast_event_inserts: false,
        },
    )
}

pub(crate) const WARP_SQLITE_SOURCE_FORMAT: &str = "warp_sqlite";

#[derive(Debug, Clone)]
pub(crate) struct WarpConversationRow {
    pub(crate) rowid: i64,
    pub(crate) conversation_id: String,
    pub(crate) conversation_data: String,
    pub(crate) last_modified_at: String,
}

#[derive(Debug, Clone)]
pub(crate) struct WarpTaskRow {
    pub(crate) rowid: i64,
    pub(crate) conversation_id: String,
    pub(crate) task_id: String,
    pub(crate) task: Vec<u8>,
    pub(crate) last_modified_at: String,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct WarpTaskProto {
    pub(crate) id: String,
    pub(crate) description: String,
    pub(crate) parent_task_id: Option<String>,
    pub(crate) summary: String,
    pub(crate) messages: Vec<WarpMessageProto>,
}

#[derive(Debug, Clone)]
pub(crate) struct WarpMessageProto {
    pub(crate) id: String,
    pub(crate) task_id: String,
    pub(crate) request_id: String,
    pub(crate) timestamp: Option<DateTime<Utc>>,
    pub(crate) kind: &'static str,
    pub(crate) role: Option<EventRole>,
    pub(crate) event_type: EventType,
    pub(crate) text: String,
}

impl Default for WarpMessageProto {
    fn default() -> Self {
        Self {
            id: String::new(),
            task_id: String::new(),
            request_id: String::new(),
            timestamp: None,
            kind: "unknown",
            role: None,
            event_type: EventType::Notice,
            text: String::new(),
        }
    }
}

pub(crate) fn normalize_warp_sqlite(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    let conn = open_provider_sqlite_readonly(path)?;
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let schema_fingerprint = opencode_schema_fingerprint(&conn)?;
    let conversations = warp_conversation_rows(&conn)?;
    let tasks = warp_task_rows(&conn)?;
    let mut tasks_by_conversation = BTreeMap::<String, Vec<WarpTaskRow>>::new();
    for task in tasks {
        tasks_by_conversation
            .entry(task.conversation_id.clone())
            .or_default()
            .push(task);
    }

    let raw_source_path = path.display().to_string();
    let mut result = ProviderNormalizationResult::default();

    for conversation in conversations {
        let line_base = warp_line_number(conversation.rowid, 0);
        let conversation_modified = match warp_sqlite_timestamp(
            &conversation.last_modified_at,
            "Warp agent_conversations.last_modified_at",
        ) {
            Ok(timestamp) => timestamp,
            Err(err) => {
                push_provider_import_failure(&mut result.summary, line_base, err.to_string());
                continue;
            }
        };
        let conversation_data = serde_json::from_str::<Value>(&conversation.conversation_data)
            .unwrap_or_else(|_| json!({ "parse_error": "invalid conversation_data JSON" }));
        let parent_conversation_id = conversation_data
            .get("parent_conversation_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let is_subagent = parent_conversation_id.is_some();
        let tasks = tasks_by_conversation
            .remove(&conversation.conversation_id)
            .unwrap_or_default();
        let mut decoded_tasks = Vec::new();
        let mut events = Vec::new();
        for task_row in tasks {
            let task = match warp_decode_task(&task_row.task) {
                Ok(task) => task,
                Err(err) => {
                    push_provider_import_failure(
                        &mut result.summary,
                        warp_line_number(task_row.rowid, 0),
                        format!(
                            "failed to decode Warp agent_tasks.task {}: {err}",
                            task_row.task_id
                        ),
                    );
                    continue;
                }
            };
            let task_modified = warp_sqlite_timestamp(
                &task_row.last_modified_at,
                "Warp agent_tasks.last_modified_at",
            )
            .unwrap_or(conversation_modified);
            let task_id = if task.id.is_empty() {
                task_row.task_id.clone()
            } else {
                task.id.clone()
            };
            for (message_index, message) in task.messages.iter().enumerate() {
                if message.text.trim().is_empty() {
                    continue;
                }
                let message_time = message.timestamp.unwrap_or(task_modified);
                let provider_event_index = events.len() as u64;
                events.push(warp_message_event(
                    &conversation.conversation_id,
                    &task_id,
                    message,
                    message_index as u64,
                    provider_event_index,
                    message_time,
                ));
            }
            decoded_tasks.push(json!({
                "task_id": task_id,
                "stored_task_id": task_row.task_id,
                "description": provider_local_preview(&task.description, PROVIDER_MAX_PREVIEW_CHARS).0,
                "summary": provider_local_preview(&task.summary, PROVIDER_MAX_PREVIEW_CHARS).0,
                "parent_task_id": task.parent_task_id,
                "message_count": task.messages.len(),
            }));
        }

        let started_at = events
            .iter()
            .map(|event| event.occurred_at)
            .min()
            .unwrap_or(conversation_modified);
        let session_metadata = warp_session_metadata(&conversation_data, &decoded_tasks);
        if events.is_empty() {
            result.captures.push((
                line_base,
                warp_capture(
                    &conversation.conversation_id,
                    parent_conversation_id.clone(),
                    is_subagent,
                    started_at,
                    conversation_modified,
                    &raw_source_path,
                    user_version,
                    &schema_fingerprint,
                    session_metadata,
                    None,
                    context,
                ),
            ));
            continue;
        }

        for (event_index, event) in events.into_iter().enumerate() {
            result.captures.push((
                warp_line_number(conversation.rowid, event_index as u64 + 1),
                warp_capture(
                    &conversation.conversation_id,
                    parent_conversation_id.clone(),
                    is_subagent,
                    started_at,
                    conversation_modified,
                    &raw_source_path,
                    user_version,
                    &schema_fingerprint,
                    session_metadata.clone(),
                    Some(event),
                    context,
                ),
            ));
        }
    }

    Ok(result)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn warp_capture(
    conversation_id: &str,
    parent_conversation_id: Option<String>,
    is_subagent: bool,
    started_at: DateTime<Utc>,
    ended_at: DateTime<Utc>,
    raw_source_path: &str,
    user_version: i64,
    schema_fingerprint: &str,
    session_metadata: Value,
    event: Option<ProviderEventEnvelope>,
    context: &ProviderAdapterContext,
) -> ProviderCaptureEnvelope {
    native_provider_capture(
        NativeSessionDraft {
            provider: CaptureProvider::Warp,
            source_format: WARP_SQLITE_SOURCE_FORMAT,
            provider_session_id: conversation_id.to_owned(),
            parent_provider_session_id: parent_conversation_id.clone(),
            root_provider_session_id: parent_conversation_id,
            external_agent_id: Some("warp-agent".to_owned()),
            agent_type: if is_subagent {
                AgentType::Subagent
            } else {
                AgentType::Primary
            },
            role_hint: Some(if is_subagent { "subagent" } else { "primary" }.to_owned()),
            is_primary: !is_subagent,
            started_at,
            ended_at: Some(ended_at),
            cwd: None,
            fidelity: Fidelity::Imported,
            raw_source_path: raw_source_path.to_owned(),
            trust: ProviderSourceTrust::ProviderNative,
            source_metadata: json!({
                "adapter": WARP_SQLITE_SOURCE_FORMAT,
                "sqlite_user_version": user_version,
                "schema_fingerprint": schema_fingerprint,
                "source_path": raw_source_path,
                "upstream_schema_anchor": {
                    "repository": "warpdotdev/warp",
                    "files": [
                        "crates/persistence/src/schema.rs",
                        "crates/persistence/src/model.rs",
                        "app/src/persistence/agent.rs"
                    ],
                    "proto_repository": "warpdotdev/warp-proto-apis",
                    "proto_files": ["apis/multi_agent/v1/task.proto"]
                },
            }),
            session_metadata,
        },
        context,
        event,
    )
}

pub(crate) fn warp_session_metadata(conversation_data: &Value, decoded_tasks: &[Value]) -> Value {
    json!({
        "source_format": WARP_SQLITE_SOURCE_FORMAT,
        "title": conversation_data
            .get("agent_name")
            .and_then(Value::as_str)
            .unwrap_or("Warp conversation"),
        "agent_name": conversation_data.get("agent_name").cloned().unwrap_or(Value::Null),
        "parent_conversation_id": conversation_data
            .get("parent_conversation_id")
            .cloned()
            .unwrap_or(Value::Null),
        "run_id": conversation_data.get("run_id").cloned().unwrap_or(Value::Null),
        "has_server_conversation_token": conversation_data
            .get("server_conversation_token")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty()),
        "has_forked_from_server_conversation_token": conversation_data
            .get("forked_from_server_conversation_token")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty()),
        "conversation_usage_metadata": conversation_data
            .get("conversation_usage_metadata")
            .cloned()
            .unwrap_or(Value::Null),
        "task_summaries": decoded_tasks,
        "privacy": "server conversation tokens are intentionally not copied from Warp conversation_data",
    })
}

pub(crate) fn warp_conversation_rows(conn: &Connection) -> Result<Vec<WarpConversationRow>> {
    if !sqlite_table_exists(conn, "agent_conversations")? {
        return Err(CaptureError::InvalidPayload(
            "Warp SQLite database is missing required agent_conversations table".into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "agent_conversations")?;
    ensure_sqlite_table_columns(
        &columns,
        "Warp agent_conversations table",
        &["conversation_id", "conversation_data", "last_modified_at"],
    )?;
    let mut stmt = conn.prepare(
        "select rowid, conversation_id, conversation_data, last_modified_at \
         from agent_conversations order by last_modified_at, conversation_id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(WarpConversationRow {
            rowid: row.get(0)?,
            conversation_id: row.get(1)?,
            conversation_data: row.get(2)?,
            last_modified_at: row.get(3)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CaptureError::from)
}

pub(crate) fn warp_task_rows(conn: &Connection) -> Result<Vec<WarpTaskRow>> {
    if !sqlite_table_exists(conn, "agent_tasks")? {
        return Err(CaptureError::InvalidPayload(
            "Warp SQLite database is missing required agent_tasks table".into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "agent_tasks")?;
    ensure_sqlite_table_columns(
        &columns,
        "Warp agent_tasks table",
        &["conversation_id", "task_id", "task", "last_modified_at"],
    )?;
    let mut stmt = conn.prepare(
        "select rowid, conversation_id, task_id, task, last_modified_at \
         from agent_tasks order by conversation_id, task_id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(WarpTaskRow {
            rowid: row.get(0)?,
            conversation_id: row.get(1)?,
            task_id: row.get(2)?,
            task: row.get(3)?,
            last_modified_at: row.get(4)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CaptureError::from)
}

pub(crate) fn warp_sqlite_timestamp(raw: &str, field: &'static str) -> Result<DateTime<Utc>> {
    if let Some(timestamp) = parse_rfc3339_utc(raw) {
        return Ok(timestamp);
    }
    let naive = NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S%.f").map_err(|_| {
        CaptureError::InvalidPayload(format!("{field} is not a supported timestamp: {raw:?}"))
    })?;
    Ok(DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc))
}

pub(crate) fn warp_line_number(rowid: i64, index: u64) -> usize {
    let row = u64::try_from(rowid.max(0)).unwrap_or(0);
    provider_line_from_index(row.saturating_mul(100_000).saturating_add(index))
}

pub(crate) fn warp_message_event(
    conversation_id: &str,
    task_id: &str,
    message: &WarpMessageProto,
    message_index: u64,
    provider_event_index: u64,
    occurred_at: DateTime<Utc>,
) -> ProviderEventEnvelope {
    let (text, truncated) = provider_local_preview(&message.text, PROVIDER_MAX_TEXT_CHARS);
    let message_id = if message.id.is_empty() {
        format!("{task_id}:{message_index}")
    } else {
        message.id.clone()
    };
    ProviderEventEnvelope {
        provider_event_index,
        provider_event_hash: Some(message_id.clone()),
        cursor: Some(format!("agent_task:{task_id}:message:{message_index}")),
        event_type: message.event_type,
        role: message.role,
        occurred_at,
        fidelity: Fidelity::Imported,
        redaction_state: RedactionState::LocalPreview,
        idempotency_key: Some(format!(
            "provider-event:warp:{conversation_id}:{message_id}"
        )),
        artifacts: Vec::new(),
        payload: json!({
            "kind": message.kind,
            "message_id": message_id,
            "task_id": task_id,
            "request_id": if message.request_id.is_empty() { Value::Null } else { json!(message.request_id) },
            "text": text,
            "truncated": truncated,
            "body": {
                "text": text,
                "message_index": message_index,
            },
        }),
        metadata: json!({
            "source": WARP_SQLITE_SOURCE_FORMAT,
            "source_format": WARP_SQLITE_SOURCE_FORMAT,
            "message_kind": message.kind,
            "task_id": task_id,
            "proto_task_id": if message.task_id.is_empty() { Value::Null } else { json!(message.task_id) },
            "request_id": if message.request_id.is_empty() { Value::Null } else { json!(message.request_id) },
        }),
    }
}

pub(crate) fn warp_decode_task(data: &[u8]) -> Result<WarpTaskProto> {
    let mut task = WarpTaskProto::default();
    let mut pos = 0;
    while pos < data.len() {
        let (field, wire) = proto_key(data, &mut pos)?;
        match (field, wire) {
            (1, 2) => task.id = proto_string(data, &mut pos)?,
            (2, 2) => task.description = proto_string(data, &mut pos)?,
            (3, 2) => task.parent_task_id = warp_decode_dependencies(proto_len(data, &mut pos)?)?,
            (5, 2) => task
                .messages
                .push(warp_decode_message(proto_len(data, &mut pos)?)?),
            (6, 2) => task.summary = proto_string(data, &mut pos)?,
            _ => proto_skip(data, &mut pos, wire)?,
        }
    }
    Ok(task)
}

pub(crate) fn warp_decode_dependencies(data: &[u8]) -> Result<Option<String>> {
    let mut pos = 0;
    let mut parent = None;
    while pos < data.len() {
        let (field, wire) = proto_key(data, &mut pos)?;
        match (field, wire) {
            (1, 2) => {
                let value = proto_string(data, &mut pos)?;
                if !value.is_empty() {
                    parent = Some(value);
                }
            }
            _ => proto_skip(data, &mut pos, wire)?,
        }
    }
    Ok(parent)
}

pub(crate) fn warp_decode_message(data: &[u8]) -> Result<WarpMessageProto> {
    let mut message = WarpMessageProto::default();
    let mut pos = 0;
    while pos < data.len() {
        let (field, wire) = proto_key(data, &mut pos)?;
        match (field, wire) {
            (1, 2) => message.id = proto_string(data, &mut pos)?,
            (11, 2) => message.task_id = proto_string(data, &mut pos)?,
            (13, 2) => message.request_id = proto_string(data, &mut pos)?,
            (14, 2) => message.timestamp = warp_decode_timestamp(proto_len(data, &mut pos)?)?,
            (2, 2) => {
                message.kind = "user_query";
                message.role = Some(EventRole::User);
                message.event_type = EventType::Message;
                message.text =
                    proto_nested_string_field(proto_len(data, &mut pos)?, 1)?.unwrap_or_default();
            }
            (3, 2) => {
                message.kind = "agent_output";
                message.role = Some(EventRole::Assistant);
                message.event_type = EventType::Message;
                message.text =
                    proto_nested_string_field(proto_len(data, &mut pos)?, 1)?.unwrap_or_default();
            }
            (4, 2) => {
                let tool_name =
                    warp_tool_name(proto_first_len_field(proto_len(data, &mut pos)?)?.unwrap_or(0));
                message.kind = "tool_call";
                message.role = Some(EventRole::Assistant);
                message.event_type = EventType::ToolCall;
                message.text = format!("tool call: {tool_name}");
            }
            (5, 2) => {
                let tool_name = warp_tool_result_name(
                    proto_first_len_field(proto_len(data, &mut pos)?)?.unwrap_or(0),
                );
                message.kind = "tool_call_result";
                message.role = Some(EventRole::Tool);
                message.event_type = EventType::ToolOutput;
                message.text = format!("tool result: {tool_name}");
            }
            (9, 2) => {
                message.kind = "system_query";
                message.role = Some(EventRole::System);
                message.event_type = EventType::Message;
                message.text = warp_decode_system_query(proto_len(data, &mut pos)?)?;
            }
            (15, 2) => {
                message.kind = "agent_reasoning";
                message.role = Some(EventRole::Assistant);
                message.event_type = EventType::Message;
                message.text =
                    proto_nested_string_field(proto_len(data, &mut pos)?, 1)?.unwrap_or_default();
            }
            (16, 2) => {
                message.kind = "summarization";
                message.role = Some(EventRole::Assistant);
                message.event_type = EventType::Message;
                message.text = warp_decode_summarization(proto_len(data, &mut pos)?)?;
            }
            (21, 2) => {
                message.kind = "debug_output";
                message.event_type = EventType::Notice;
                message.text = "debug output".to_owned();
                proto_skip(data, &mut pos, wire)?;
            }
            (24, 2) => {
                message.kind = "messages_received_from_agents";
                message.role = Some(EventRole::Assistant);
                message.event_type = EventType::Message;
                message.text = warp_decode_received_messages(proto_len(data, &mut pos)?)?;
            }
            _ => proto_skip(data, &mut pos, wire)?,
        }
    }
    Ok(message)
}

pub(crate) fn warp_decode_timestamp(data: &[u8]) -> Result<Option<DateTime<Utc>>> {
    let mut pos = 0;
    let mut seconds = None;
    let mut nanos = 0u32;
    while pos < data.len() {
        let (field, wire) = proto_key(data, &mut pos)?;
        match (field, wire) {
            (1, 0) => seconds = Some(proto_varint(data, &mut pos)? as i64),
            (2, 0) => nanos = proto_varint(data, &mut pos)? as u32,
            _ => proto_skip(data, &mut pos, wire)?,
        }
    }
    Ok(seconds.and_then(|secs| DateTime::<Utc>::from_timestamp(secs, nanos)))
}
