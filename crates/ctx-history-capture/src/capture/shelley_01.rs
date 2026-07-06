#[allow(unused_imports)]
use super::*;

#[derive(Debug, Clone)]
pub struct ShelleySqliteImportOptions {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub history_record_id: Option<Uuid>,
    pub allow_partial_failures: bool,
}

impl Default for ShelleySqliteImportOptions {
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

#[derive(Debug, Clone, Copy, Default)]
pub struct ShelleySqliteAdapter;

impl ProviderCaptureAdapter for ShelleySqliteAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Shelley
    }

    fn source_format(&self) -> &str {
        SHELLEY_SQLITE_SOURCE_FORMAT
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        normalize_shelley_sqlite(path, context)
    }
}

pub fn import_shelley_sqlite(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: ShelleySqliteImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = ShelleySqliteAdapter.normalize_path(
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
            fast_event_inserts: true,
        },
    )
}

pub(crate) const SHELLEY_SQLITE_SOURCE_FORMAT: &str = "shelley_sqlite";

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

pub(crate) fn normalize_shelley_sqlite(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    let conn = open_provider_sqlite_readonly(path)?;
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let schema_fingerprint = opencode_schema_fingerprint(&conn)?;
    let conversations = shelley_conversations(&conn)?;
    let messages = shelley_messages(&conn)?;
    let conversations_by_id = conversations
        .iter()
        .map(|conversation| (conversation.conversation_id.clone(), conversation))
        .collect::<BTreeMap<_, _>>();
    let mut seen_message_conversations = BTreeSet::new();
    let raw_source_path = path.display().to_string();
    let mut result = ProviderNormalizationResult::default();

    for message in messages {
        let Some(conversation) = conversations_by_id.get(&message.conversation_id) else {
            result.summary.failed += 1;
            result.summary.failures.push(ProviderImportFailure {
                line: message.sequence_id.max(0) as usize,
                error: format!(
                    "Shelley message {} references missing conversation {}",
                    message.message_id, message.conversation_id
                ),
            });
            continue;
        };
        seen_message_conversations.insert(message.conversation_id.clone());
        let started_at = shelley_timestamp(conversation.created_at.as_deref(), context.imported_at);
        let ended_at = conversation
            .updated_at
            .as_deref()
            .map(|timestamp| shelley_timestamp(Some(timestamp), context.imported_at));
        let occurred_at = shelley_timestamp(message.created_at.as_deref(), started_at);
        let body = shelley_message_body(&message);
        let text = shelley_message_text(&message, &body)
            .unwrap_or_else(|| format!("Shelley {} message", message.entry_type));
        let event_type = shelley_event_type(&message, &body);
        let role = shelley_event_role(&message.entry_type);
        let event = native_event(NativeEventDraft {
            provider: CaptureProvider::Shelley,
            source_format: SHELLEY_SQLITE_SOURCE_FORMAT,
            provider_session_id: conversation.conversation_id.clone(),
            provider_event_index: shelley_event_index(&message),
            provider_event_hash: Some(message.message_id.clone()),
            cursor: format!(
                "conversation:{}:sequence:{}:message:{}",
                message.conversation_id, message.sequence_id, message.message_id
            ),
            event_type,
            role,
            occurred_at,
            text,
            body,
            metadata: json!({
                "source": "shelley_messages",
                "source_format": SHELLEY_SQLITE_SOURCE_FORMAT,
                "message_id": message.message_id,
                "conversation_id": message.conversation_id,
                "sequence_id": message.sequence_id,
                "rowid": message.rowid,
                "message_type": message.entry_type,
                "generation": message.generation,
                "excluded_from_context": message.excluded_from_context,
                "usage": message.usage_data.as_deref().map(provider_json_text),
                "llm_api_url": message.llm_api_url,
                "model_name": message.model_name,
                "forked_from_message_id": message.forked_from_message_id,
            }),
        });
        result.captures.push((
            message.rowid.max(0) as usize,
            shelley_capture(
                ShelleyCaptureDraft {
                    conversation,
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

    for conversation in conversations {
        if seen_message_conversations.contains(&conversation.conversation_id) {
            continue;
        }
        let started_at = shelley_timestamp(conversation.created_at.as_deref(), context.imported_at);
        let ended_at = conversation
            .updated_at
            .as_deref()
            .map(|timestamp| shelley_timestamp(Some(timestamp), context.imported_at));
        result.captures.push((
            0,
            shelley_capture(
                ShelleyCaptureDraft {
                    conversation: &conversation,
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

    Ok(result)
}

pub(crate) struct ShelleyCaptureDraft<'a> {
    pub(crate) conversation: &'a ShelleyConversationRow,
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) ended_at: Option<DateTime<Utc>>,
    pub(crate) raw_source_path: &'a str,
    pub(crate) user_version: i64,
    pub(crate) schema_fingerprint: &'a str,
    pub(crate) event: Option<ProviderEventEnvelope>,
}

pub(crate) fn shelley_capture(
    draft: ShelleyCaptureDraft<'_>,
    context: &ProviderAdapterContext,
) -> ProviderCaptureEnvelope {
    let ShelleyCaptureDraft {
        conversation,
        started_at,
        ended_at,
        raw_source_path,
        user_version,
        schema_fingerprint,
        event,
    } = draft;
    let is_subagent = conversation.parent_conversation_id.is_some() || !conversation.user_initiated;
    let conversation_options = conversation
        .conversation_options
        .as_deref()
        .map(provider_json_text)
        .unwrap_or(Value::Null);
    let tags = conversation
        .tags
        .as_deref()
        .map(provider_json_text)
        .unwrap_or(Value::Null);
    let queued_messages = conversation
        .queued_messages
        .as_deref()
        .map(provider_json_text)
        .unwrap_or(Value::Null);
    native_provider_capture(
        NativeSessionDraft {
            provider: CaptureProvider::Shelley,
            source_format: SHELLEY_SQLITE_SOURCE_FORMAT,
            provider_session_id: conversation.conversation_id.clone(),
            parent_provider_session_id: conversation.parent_conversation_id.clone(),
            root_provider_session_id: conversation.parent_conversation_id.clone(),
            external_agent_id: None,
            agent_type: if is_subagent {
                AgentType::Subagent
            } else {
                AgentType::Primary
            },
            role_hint: Some(if is_subagent { "subagent" } else { "primary" }.to_owned()),
            is_primary: !is_subagent,
            started_at,
            ended_at,
            cwd: conversation.cwd.clone(),
            fidelity: Fidelity::Imported,
            raw_source_path: raw_source_path.to_owned(),
            trust: ProviderSourceTrust::ProviderNative,
            source_metadata: json!({
                "adapter": SHELLEY_SQLITE_SOURCE_FORMAT,
                "sqlite_user_version": user_version,
                "schema_fingerprint": schema_fingerprint,
                "source_path": raw_source_path,
            }),
            session_metadata: json!({
                "source_format": SHELLEY_SQLITE_SOURCE_FORMAT,
                "conversation_id": conversation.conversation_id,
                "slug": conversation.slug,
                "title": conversation.slug,
                "user_initiated": conversation.user_initiated,
                "archived": conversation.archived,
                "parent_conversation_id": conversation.parent_conversation_id,
                "model": conversation.model,
                "conversation_options": conversation_options,
                "current_generation": conversation.current_generation,
                "agent_working": conversation.agent_working,
                "tags": tags,
                "is_draft": conversation.is_draft,
                "draft": conversation.draft,
                "queued_messages": queued_messages,
            }),
        },
        context,
        event,
    )
}

pub(crate) fn shelley_conversations(conn: &Connection) -> Result<Vec<ShelleyConversationRow>> {
    if !sqlite_table_exists(conn, "conversations")? {
        return Err(CaptureError::InvalidPayload(
            "Shelley shelley.db is missing required conversations table".into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "conversations")?;
    ensure_sqlite_table_columns(
        &columns,
        "Shelley conversations table",
        &["conversation_id"],
    )?;
    let slug = optional_column_expr(&columns, "slug", "NULL");
    let user_initiated = optional_column_expr(&columns, "user_initiated", "1");
    let created_at = optional_column_expr(&columns, "created_at", "NULL");
    let updated_at = optional_column_expr(&columns, "updated_at", "NULL");
    let cwd = optional_column_expr(&columns, "cwd", "NULL");
    let archived = optional_column_expr(&columns, "archived", "0");
    let parent_conversation_id = optional_column_expr(&columns, "parent_conversation_id", "NULL");
    let model = optional_column_expr(&columns, "model", "NULL");
    let conversation_options = optional_column_expr(&columns, "conversation_options", "NULL");
    let current_generation = optional_column_expr(&columns, "current_generation", "NULL");
    let agent_working = optional_column_expr(&columns, "agent_working", "0");
    let tags = optional_column_expr(&columns, "tags", "NULL");
    let is_draft = optional_column_expr(&columns, "is_draft", "0");
    let draft = optional_column_expr(&columns, "draft", "NULL");
    let queued_messages = optional_column_expr(&columns, "queued_messages", "NULL");
    let sql = format!(
        "select conversation_id, {slug}, {user_initiated}, {created_at}, {updated_at}, \
         {cwd}, {archived}, {parent_conversation_id}, {model}, {conversation_options}, \
         {current_generation}, {agent_working}, {tags}, {is_draft}, {draft}, \
         {queued_messages} \
         from conversations order by {created_at}, conversation_id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok(ShelleyConversationRow {
            conversation_id: row.get(0)?,
            slug: row.get(1)?,
            user_initiated: sqlite_bool(row.get::<_, Option<i64>>(2)?),
            created_at: row.get(3)?,
            updated_at: row.get(4)?,
            cwd: row.get(5)?,
            archived: sqlite_bool(row.get::<_, Option<i64>>(6)?),
            parent_conversation_id: row.get(7)?,
            model: row.get(8)?,
            conversation_options: row.get(9)?,
            current_generation: row.get(10)?,
            agent_working: sqlite_bool(row.get::<_, Option<i64>>(11)?),
            tags: row.get(12)?,
            is_draft: sqlite_bool(row.get::<_, Option<i64>>(13)?),
            draft: row.get(14)?,
            queued_messages: row.get(15)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CaptureError::from)
}

pub(crate) fn shelley_messages(conn: &Connection) -> Result<Vec<ShelleyMessageRow>> {
    if !sqlite_table_exists(conn, "messages")? {
        return Err(CaptureError::InvalidPayload(
            "Shelley shelley.db is missing required messages table".into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "messages")?;
    ensure_sqlite_table_columns(
        &columns,
        "Shelley messages table",
        &["message_id", "conversation_id", "type"],
    )?;
    let sequence_id = optional_column_expr(&columns, "sequence_id", "rowid");
    let llm_data = optional_column_expr(&columns, "llm_data", "NULL");
    let user_data = optional_column_expr(&columns, "user_data", "NULL");
    let usage_data = optional_column_expr(&columns, "usage_data", "NULL");
    let created_at = optional_column_expr(&columns, "created_at", "NULL");
    let display_data = optional_column_expr(&columns, "display_data", "NULL");
    let excluded_from_context = optional_column_expr(&columns, "excluded_from_context", "0");
    let generation = optional_column_expr(&columns, "generation", "NULL");
    let llm_api_url = optional_column_expr(&columns, "llm_api_url", "NULL");
    let model_name = optional_column_expr(&columns, "model_name", "NULL");
    let forked_from_message_id = optional_column_expr(&columns, "forked_from_message_id", "NULL");
    let sql = format!(
        "select rowid, message_id, conversation_id, {sequence_id}, type, {llm_data}, \
         {user_data}, {usage_data}, {created_at}, {display_data}, \
         {excluded_from_context}, {generation}, {llm_api_url}, {model_name}, \
         {forked_from_message_id} from messages order by conversation_id, {sequence_id}, rowid"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok(ShelleyMessageRow {
            rowid: row.get(0)?,
            message_id: row.get(1)?,
            conversation_id: row.get(2)?,
            sequence_id: row.get(3)?,
            entry_type: row.get(4)?,
            llm_data: row.get(5)?,
            user_data: row.get(6)?,
            usage_data: row.get(7)?,
            created_at: row.get(8)?,
            display_data: row.get(9)?,
            excluded_from_context: sqlite_bool(row.get::<_, Option<i64>>(10)?),
            generation: row.get(11)?,
            llm_api_url: row.get(12)?,
            model_name: row.get(13)?,
            forked_from_message_id: row.get(14)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CaptureError::from)
}

pub(crate) fn shelley_timestamp(raw: Option<&str>, fallback: DateTime<Utc>) -> DateTime<Utc> {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return fallback;
    };
    parse_rfc3339_utc(raw)
        .or_else(|| {
            NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S%.f")
                .ok()
                .map(|naive| DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc))
        })
        .unwrap_or(fallback)
}

pub(crate) fn shelley_message_body(message: &ShelleyMessageRow) -> Value {
    json!({
        "message_id": message.message_id,
        "conversation_id": message.conversation_id,
        "sequence_id": message.sequence_id,
        "type": message.entry_type,
        "llm_data": message.llm_data.as_deref().map(provider_json_text),
        "user_data": message.user_data.as_deref().map(provider_json_text),
        "display_data": message.display_data.as_deref().map(provider_json_text),
        "usage_data": message.usage_data.as_deref().map(provider_json_text),
    })
}

pub(crate) fn shelley_message_text(message: &ShelleyMessageRow, body: &Value) -> Option<String> {
    let mut parts = Vec::new();
    for pointer in ["/user_data", "/llm_data", "/display_data"] {
        if let Some(text) = body.pointer(pointer).and_then(shelley_value_text) {
            parts.push(text);
        }
    }
    if parts.is_empty() && message.entry_type == "system" {
        Some("Shelley system message".to_owned())
    } else if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

pub(crate) fn shelley_event_role(entry_type: &str) -> Option<EventRole> {
    Some(match entry_type {
        "user" => EventRole::User,
        "agent" | "assistant" => EventRole::Assistant,
        "tool" => EventRole::Tool,
        "system" | "error" | "gitinfo" | "warning" | "modelchange" => EventRole::System,
        _ => EventRole::Unknown,
    })
}

pub(crate) fn shelley_event_type(message: &ShelleyMessageRow, body: &Value) -> EventType {
    match message.entry_type.as_str() {
        "tool" => EventType::ToolOutput,
        "gitinfo" => EventType::VcsChange,
        "system" | "error" | "warning" | "modelchange" => EventType::Notice,
        "agent" | "assistant" if shelley_value_has_tool_use(body) => EventType::ToolCall,
        "user" | "agent" | "assistant" if shelley_value_has_tool_result(body) => {
            EventType::ToolOutput
        }
        "user" | "agent" | "assistant" => EventType::Message,
        _ => EventType::Notice,
    }
}

pub(crate) fn shelley_event_index(message: &ShelleyMessageRow) -> u64 {
    let sequence = message.sequence_id.max(0) as u64;
    let bucket = text_id_index(
        &format!("{}:{}", message.conversation_id, message.message_id),
        4_096,
    );
    sequence.saturating_mul(4_096).saturating_add(bucket)
}

pub(crate) fn shelley_value_has_tool_use(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.iter().any(shelley_value_has_tool_use),
        Value::Object(object) => {
            let content_type = shelley_content_type(value);
            matches!(
                content_type.as_deref(),
                Some("tool_use" | "server_tool_use")
            ) || object.values().any(shelley_value_has_tool_use)
        }
        _ => false,
    }
}

pub(crate) fn shelley_value_has_tool_result(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.iter().any(shelley_value_has_tool_result),
        Value::Object(object) => {
            let content_type = shelley_content_type(value);
            matches!(
                content_type.as_deref(),
                Some("tool_result" | "web_search_tool_result" | "web_search_result")
            ) || object.values().any(shelley_value_has_tool_result)
        }
        _ => false,
    }
}

pub(crate) fn shelley_value_text(value: &Value) -> Option<String> {
    let mut parts = Vec::new();
    shelley_collect_text(value, &mut parts);
    (!parts.is_empty()).then(|| parts.join("\n"))
}
