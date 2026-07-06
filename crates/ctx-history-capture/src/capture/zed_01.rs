#[allow(unused_imports)]
use super::*;

#[derive(Debug, Clone)]
pub struct ZedThreadsSqliteImportOptions {
    pub machine_id: String,
    pub source_path: Option<PathBuf>,
    pub imported_at: DateTime<Utc>,
    pub history_record_id: Option<Uuid>,
    pub allow_partial_failures: bool,
}

impl Default for ZedThreadsSqliteImportOptions {
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
pub struct ZedThreadsSqliteAdapter;

impl ProviderCaptureAdapter for ZedThreadsSqliteAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::Zed
    }

    fn source_format(&self) -> &str {
        ZED_THREADS_SQLITE_SOURCE_FORMAT
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        normalize_zed_threads_sqlite(path, context)
    }
}

pub fn import_zed_threads_sqlite(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: ZedThreadsSqliteImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = ZedThreadsSqliteAdapter.normalize_path(
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

pub(crate) const ZED_THREADS_SQLITE_SOURCE_FORMAT: &str = "zed_threads_sqlite";

#[derive(Debug, Clone)]
pub(crate) struct ZedThreadRow {
    pub(crate) rowid: i64,
    pub(crate) id: String,
    pub(crate) parent_id: Option<String>,
    pub(crate) folder_paths: Option<String>,
    pub(crate) folder_paths_order: Option<String>,
    pub(crate) summary: String,
    pub(crate) updated_at: String,
    pub(crate) data_type: String,
    pub(crate) data: Vec<u8>,
    pub(crate) created_at: Option<String>,
}

pub(crate) fn normalize_zed_threads_sqlite(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    let conn = open_provider_sqlite_readonly(path)?;
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let schema_fingerprint = opencode_schema_fingerprint(&conn)?;
    let rows = zed_thread_rows(&conn)?;
    let raw_source_path = path.display().to_string();
    let mut result = ProviderNormalizationResult::default();

    for row in rows {
        let row_line = zed_line_number(row.rowid, 0);
        let row_updated_at = match zed_required_timestamp(&row.updated_at, "Zed thread updated_at")
        {
            Ok(timestamp) => timestamp,
            Err(err) => {
                push_provider_import_failure(&mut result.summary, row_line, err.to_string());
                continue;
            }
        };
        let created_at = match row
            .created_at
            .as_deref()
            .map(|raw| zed_required_timestamp(raw, "Zed thread created_at"))
            .transpose()
        {
            Ok(timestamp) => timestamp.unwrap_or(row_updated_at),
            Err(err) => {
                push_provider_import_failure(&mut result.summary, row_line, err.to_string());
                continue;
            }
        };
        let thread = match zed_decode_thread_json(&row) {
            Ok(thread) => thread,
            Err(err) => {
                push_provider_import_failure(&mut result.summary, row_line, err.to_string());
                continue;
            }
        };
        let Some(messages) = thread.get("messages").and_then(Value::as_array) else {
            push_provider_import_failure(
                &mut result.summary,
                row_line,
                format!("Zed thread {} is missing DbThread.messages array", row.id),
            );
            continue;
        };
        let thread_updated_at = thread
            .get("updated_at")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339_utc)
            .unwrap_or(row_updated_at);
        let folder_paths = zed_folder_paths(row.folder_paths.as_deref());
        let cwd = zed_ordered_folder_paths(&folder_paths, row.folder_paths_order.as_deref())
            .into_iter()
            .next();

        if messages.is_empty() {
            result.captures.push((
                row_line,
                zed_capture(
                    ZedCaptureDraft {
                        row: &row,
                        thread: &thread,
                        started_at: created_at,
                        ended_at: Some(thread_updated_at),
                        cwd,
                        folder_paths,
                        raw_source_path: &raw_source_path,
                        user_version,
                        schema_fingerprint: &schema_fingerprint,
                        event: None,
                    },
                    context,
                ),
            ));
            continue;
        }

        for (message_index, message) in messages.iter().enumerate() {
            let line = zed_line_number(row.rowid, message_index as u64);
            let event = match zed_message_event(&row.id, message, message_index, thread_updated_at)
            {
                Ok(event) => event,
                Err(err) => {
                    push_provider_import_failure(&mut result.summary, line, err.to_string());
                    continue;
                }
            };
            result
                .files_touched
                .extend(provider_file_touches_from_raw_value(
                    CaptureProvider::Zed,
                    &row.id,
                    ZED_THREADS_SQLITE_SOURCE_FORMAT,
                    Some(raw_source_path.as_str()),
                    message,
                    &event,
                    line,
                ));
            result.captures.push((
                line,
                zed_capture(
                    ZedCaptureDraft {
                        row: &row,
                        thread: &thread,
                        started_at: created_at,
                        ended_at: Some(thread_updated_at),
                        cwd: cwd.clone(),
                        folder_paths: folder_paths.clone(),
                        raw_source_path: &raw_source_path,
                        user_version,
                        schema_fingerprint: &schema_fingerprint,
                        event: Some(event),
                    },
                    context,
                ),
            ));
        }
    }

    Ok(result)
}

pub(crate) struct ZedCaptureDraft<'a> {
    pub(crate) row: &'a ZedThreadRow,
    pub(crate) thread: &'a Value,
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) ended_at: Option<DateTime<Utc>>,
    pub(crate) cwd: Option<String>,
    pub(crate) folder_paths: Vec<String>,
    pub(crate) raw_source_path: &'a str,
    pub(crate) user_version: i64,
    pub(crate) schema_fingerprint: &'a str,
    pub(crate) event: Option<ProviderEventEnvelope>,
}

pub(crate) fn zed_capture(
    draft: ZedCaptureDraft<'_>,
    context: &ProviderAdapterContext,
) -> ProviderCaptureEnvelope {
    let title = draft
        .thread
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or(&draft.row.summary);
    let model = draft.thread.get("model").cloned().unwrap_or(Value::Null);
    let token_usage = draft
        .thread
        .get("cumulative_token_usage")
        .cloned()
        .unwrap_or(Value::Null);
    native_provider_capture(
        NativeSessionDraft {
            provider: CaptureProvider::Zed,
            source_format: ZED_THREADS_SQLITE_SOURCE_FORMAT,
            provider_session_id: draft.row.id.clone(),
            parent_provider_session_id: draft.row.parent_id.clone(),
            root_provider_session_id: draft.row.parent_id.clone(),
            external_agent_id: Some("zed".to_owned()),
            agent_type: if draft.row.parent_id.is_some() {
                AgentType::Subagent
            } else {
                AgentType::Primary
            },
            role_hint: Some(
                if draft.row.parent_id.is_some() {
                    "subagent"
                } else {
                    "primary"
                }
                .to_owned(),
            ),
            is_primary: draft.row.parent_id.is_none(),
            started_at: draft.started_at,
            ended_at: draft.ended_at,
            cwd: draft.cwd,
            fidelity: Fidelity::Imported,
            raw_source_path: draft.raw_source_path.to_owned(),
            trust: ProviderSourceTrust::ProviderNative,
            source_metadata: json!({
                "adapter": ZED_THREADS_SQLITE_SOURCE_FORMAT,
                "sqlite_user_version": draft.user_version,
                "schema_fingerprint": draft.schema_fingerprint,
                "source_path": draft.raw_source_path,
                "upstream_schema_anchor": {
                    "repository": "zed-industries/zed",
                    "commit": "e3b73c6b30cdc09e820823fe44542b89850d4be1",
                    "files": [
                        "crates/agent/src/db.rs",
                        "crates/agent/src/thread.rs"
                    ],
                    "thread_version": draft.thread.get("version").and_then(Value::as_str)
                },
            }),
            session_metadata: json!({
                "source_format": ZED_THREADS_SQLITE_SOURCE_FORMAT,
                "title": title,
                "summary": draft.row.summary,
                "parent_id": draft.row.parent_id,
                "folder_paths": draft.folder_paths,
                "folder_paths_order": draft.row.folder_paths_order,
                "created_at": draft.row.created_at,
                "updated_at": draft.row.updated_at,
                "data_type": draft.row.data_type,
                "model": model,
                "profile": draft.thread.get("profile").cloned().unwrap_or(Value::Null),
                "speed": draft.thread.get("speed").cloned().unwrap_or(Value::Null),
                "thinking_enabled": draft.thread.get("thinking_enabled").cloned().unwrap_or(Value::Null),
                "thinking_effort": draft.thread.get("thinking_effort").cloned().unwrap_or(Value::Null),
                "cumulative_token_usage": token_usage,
                "message_timestamps": "Zed DbThread messages do not carry per-message timestamps; ctx uses the thread updated_at for events.",
            }),
        },
        context,
        draft.event,
    )
}

pub(crate) fn zed_thread_rows(conn: &Connection) -> Result<Vec<ZedThreadRow>> {
    if !sqlite_table_exists(conn, "threads")? {
        return Err(CaptureError::InvalidPayload(
            "Zed threads.db is missing required threads table".into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "threads")?;
    ensure_sqlite_table_columns(
        &columns,
        "Zed threads table",
        &["id", "summary", "updated_at", "data_type", "data"],
    )?;
    let parent_id = optional_column_expr(&columns, "parent_id", "NULL");
    let folder_paths = optional_column_expr(&columns, "folder_paths", "NULL");
    let folder_paths_order = optional_column_expr(&columns, "folder_paths_order", "NULL");
    let created_at = optional_column_expr(&columns, "created_at", "NULL");
    let sql = format!(
        "select rowid, id, {parent_id}, {folder_paths}, {folder_paths_order}, summary, \
         updated_at, data_type, data, {created_at} from threads order by updated_at, id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok(ZedThreadRow {
            rowid: row.get(0)?,
            id: row.get(1)?,
            parent_id: row.get(2)?,
            folder_paths: row.get(3)?,
            folder_paths_order: row.get(4)?,
            summary: row.get(5)?,
            updated_at: row.get(6)?,
            data_type: row.get(7)?,
            data: row.get(8)?,
            created_at: row.get(9)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CaptureError::from)
}

pub(crate) fn zed_decode_thread_json(row: &ZedThreadRow) -> Result<Value> {
    let json = match row.data_type.as_str() {
        "json" => Cow::Borrowed(row.data.as_slice()),
        "zstd" => Cow::Owned(zed_decode_zstd(&row.data)?),
        other => {
            return Err(CaptureError::InvalidPayload(format!(
                "Zed thread {} has unsupported data_type {other:?}",
                row.id
            )));
        }
    };
    serde_json::from_slice(&json).map_err(CaptureError::from)
}

pub(crate) fn zed_decode_zstd(data: &[u8]) -> Result<Vec<u8>> {
    let mut decoder = zstd::stream::read::Decoder::new(data)?;
    let mut limited = decoder
        .by_ref()
        .take(MAX_PROVIDER_SQLITE_VALUE_BYTES as u64 + 1);
    let mut out = Vec::new();
    limited.read_to_end(&mut out)?;
    if out.len() > MAX_PROVIDER_SQLITE_VALUE_BYTES {
        return Err(CaptureError::InvalidPayload(format!(
            "Zed compressed thread JSON exceeds {} decompressed bytes",
            MAX_PROVIDER_SQLITE_VALUE_BYTES
        )));
    }
    Ok(out)
}

pub(crate) fn zed_required_timestamp(raw: &str, field: &'static str) -> Result<DateTime<Utc>> {
    parse_rfc3339_utc(raw)
        .ok_or_else(|| CaptureError::InvalidPayload(format!("{field} is not RFC3339: {raw:?}")))
}

pub(crate) fn zed_line_number(rowid: i64, message_index: u64) -> usize {
    let row = u64::try_from(rowid.max(0)).unwrap_or(0);
    provider_line_from_index(row.saturating_mul(10_000).saturating_add(message_index))
}

pub(crate) fn zed_folder_paths(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect()
}

pub(crate) fn zed_ordered_folder_paths(paths: &[String], order: Option<&str>) -> Vec<String> {
    let Some(order) = order else {
        return paths.to_vec();
    };
    let indices = order
        .split(',')
        .filter_map(|item| item.parse::<usize>().ok())
        .collect::<Vec<_>>();
    if indices.len() != paths.len() {
        return paths.to_vec();
    }
    let mut ordered = paths
        .iter()
        .cloned()
        .zip(indices)
        .collect::<Vec<(String, usize)>>();
    ordered.sort_by_key(|(_, index)| *index);
    ordered.into_iter().map(|(path, _)| path).collect()
}

pub(crate) fn zed_message_event(
    provider_session_id: &str,
    message: &Value,
    message_index: usize,
    occurred_at: DateTime<Utc>,
) -> Result<ProviderEventEnvelope> {
    let kind = zed_message_kind(message).unwrap_or("Unknown");
    let text = zed_message_text(message).unwrap_or_else(|| format!("Zed {kind} message"));
    let event_type = zed_message_event_type(kind, message);
    let role = zed_message_role(kind);
    let provider_event_index = u64::try_from(message_index).map_err(|_| {
        CaptureError::InvalidPayload(format!("Zed message index is too large: {message_index}"))
    })?;
    let message_hash = compute_payload_hash(message)?;
    Ok(native_event(NativeEventDraft {
        provider: CaptureProvider::Zed,
        source_format: ZED_THREADS_SQLITE_SOURCE_FORMAT,
        provider_session_id: provider_session_id.to_owned(),
        provider_event_index,
        provider_event_hash: Some(format!("zed-message:{message_hash}")),
        cursor: format!("thread:{provider_session_id}:message:{message_index}"),
        event_type,
        role,
        occurred_at,
        text,
        body: json!({
            "message_kind": kind,
            "message": message,
        }),
        metadata: json!({
            "source": "zed_threads_db",
            "source_format": ZED_THREADS_SQLITE_SOURCE_FORMAT,
            "message_index": message_index,
            "message_kind": kind,
            "timestamp_source": "thread.updated_at",
        }),
    }))
}

pub(crate) fn zed_message_kind(message: &Value) -> Option<&str> {
    match message {
        Value::String(kind) => Some(kind.as_str()),
        Value::Object(object) if object.len() == 1 => object.keys().next().map(String::as_str),
        _ => None,
    }
}

pub(crate) fn zed_message_inner<'a>(message: &'a Value, kind: &str) -> Option<&'a Value> {
    match message {
        Value::Object(object) => object.get(kind),
        _ => None,
    }
}

pub(crate) fn zed_message_role(kind: &str) -> Option<EventRole> {
    Some(match kind {
        "User" | "Resume" => EventRole::User,
        "Agent" => EventRole::Assistant,
        "Compaction" => EventRole::System,
        _ => EventRole::Unknown,
    })
}

pub(crate) fn zed_message_event_type(kind: &str, message: &Value) -> EventType {
    match kind {
        "Agent" if zed_has_tool_use(message) => EventType::ToolCall,
        "Agent" if zed_has_tool_result(message) => EventType::ToolOutput,
        "User" | "Agent" | "Resume" => EventType::Message,
        "Compaction" => EventType::Summary,
        _ => EventType::Notice,
    }
}

pub(crate) fn zed_message_text(message: &Value) -> Option<String> {
    let kind = zed_message_kind(message)?;
    let inner = zed_message_inner(message, kind);
    match kind {
        "User" => zed_user_message_text(inner?),
        "Agent" => zed_agent_message_text(inner?),
        "Resume" => Some("[resume]".to_owned()),
        "Compaction" => zed_compaction_text(inner.unwrap_or(message)),
        _ => provider_value_text(message),
    }
}

pub(crate) fn zed_user_message_text(value: &Value) -> Option<String> {
    zed_content_array_text(value.get("content"))
}

pub(crate) fn zed_agent_message_text(value: &Value) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(text) = zed_content_array_text(value.get("content")) {
        parts.push(text);
    }
    if let Some(text) = zed_tool_results_text(value.get("tool_results")) {
        parts.push(text);
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

pub(crate) fn zed_compaction_text(value: &Value) -> Option<String> {
    if let Some(summary) = value.get("Summary").and_then(Value::as_str) {
        return Some(summary.to_owned());
    }
    if let Some(native) = value.get("ProviderNative") {
        return provider_value_text(native);
    }
    provider_value_text(value)
}

pub(crate) fn zed_content_array_text(value: Option<&Value>) -> Option<String> {
    let items = value?.as_array()?;
    let mut parts = Vec::new();
    for item in items {
        if let Some(text) = zed_content_item_text(item) {
            parts.push(text);
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

pub(crate) fn zed_content_item_text(value: &Value) -> Option<String> {
    let (kind, body) = zed_external_tag(value)?;
    match kind {
        "Text" => body.as_str().map(str::to_owned),
        "Thinking" => body
            .get("text")
            .and_then(Value::as_str)
            .map(|text| format!("<think>{text}</think>")),
        "RedactedThinking" => Some("<redacted_thinking />".to_owned()),
        "ToolUse" => Some(zed_tool_use_text(body)),
        "Mention" => zed_mention_text(body),
        "Image" => Some("<image />".to_owned()),
        other => provider_value_text(body).map(|text| format!("{other}: {text}")),
    }
}

pub(crate) fn zed_tool_use_text(value: &Value) -> String {
    let name = value.get("name").and_then(Value::as_str).unwrap_or("tool");
    let mut parts = vec![format!("tool call: {name}")];
    if let Some(input) = value.get("input") {
        if !input.is_null() {
            parts.push(format!(
                "tool input: {}",
                provider_capped_json(input, PROVIDER_MAX_PREVIEW_CHARS)
            ));
        }
    } else if let Some(raw_input) = value.get("raw_input").and_then(Value::as_str) {
        if !raw_input.trim().is_empty() {
            parts.push(format!("tool input: {raw_input}"));
        }
    }
    parts.join("\n")
}

pub(crate) fn zed_mention_text(value: &Value) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(uri) = value.get("uri") {
        if let Some(uri_text) = provider_value_text(uri) {
            parts.push(uri_text);
        }
    }
    if let Some(content) = value.get("content").and_then(Value::as_str) {
        parts.push(content.to_owned());
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}
