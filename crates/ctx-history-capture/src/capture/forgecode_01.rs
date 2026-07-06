#[allow(unused_imports)]
use super::*;

impl ProviderCaptureAdapter for ForgeCodeSqliteAdapter {
    fn provider(&self) -> CaptureProvider {
        CaptureProvider::ForgeCode
    }

    fn source_format(&self) -> &str {
        FORGECODE_SQLITE_SOURCE_FORMAT
    }

    fn normalize_path(
        &self,
        path: &Path,
        context: &ProviderAdapterContext,
    ) -> Result<ProviderNormalizationResult> {
        normalize_forgecode_sqlite(path, context)
    }
}

pub fn import_forgecode_sqlite(
    path: impl AsRef<Path>,
    store: &mut Store,
    options: ForgeCodeSqliteImportOptions,
) -> Result<ProviderImportSummary> {
    let path = path.as_ref();
    let source_path = options
        .source_path
        .clone()
        .unwrap_or_else(|| path.to_path_buf());
    let normalization = ForgeCodeSqliteAdapter.normalize_path(
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

pub(crate) const FORGECODE_SQLITE_SOURCE_FORMAT: &str = "forgecode_sqlite";

pub(crate) fn normalize_forgecode_sqlite(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    let conn = open_provider_sqlite_readonly(path)?;
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let schema_fingerprint = opencode_schema_fingerprint(&conn)?;
    let conversations = forgecode_conversations(&conn)?;
    let raw_source_path = path.display().to_string();
    let mut result = ProviderNormalizationResult::default();

    for row in conversations {
        let row_line = provider_line_from_index(row.rowid.max(0) as u64);
        let started_at = forgecode_timestamp(Some(&row.created_at), context.imported_at);
        let ended_at = row
            .updated_at
            .as_deref()
            .map(|raw| forgecode_timestamp(Some(raw), started_at));

        let context_value = match row.context.as_deref().filter(|raw| !raw.trim().is_empty()) {
            Some(raw) => match serde_json::from_str::<Value>(raw) {
                Ok(value) => Some(value),
                Err(err) => {
                    push_provider_import_failure(
                        &mut result.summary,
                        row_line,
                        format!(
                            "invalid JSON in ForgeCode conversations.context {}: {err}",
                            row.conversation_id
                        ),
                    );
                    None
                }
            },
            None => None,
        };
        let metrics_value = match row.metrics.as_deref().filter(|raw| !raw.trim().is_empty()) {
            Some(raw) => match serde_json::from_str::<Value>(raw) {
                Ok(value) => Some(value),
                Err(err) => {
                    push_provider_import_failure(
                        &mut result.summary,
                        row_line,
                        format!(
                            "invalid JSON in ForgeCode conversations.metrics {}: {err}",
                            row.conversation_id
                        ),
                    );
                    None
                }
            },
            None => None,
        };

        if let Some(metrics) = metrics_value.as_ref() {
            result.files_touched.extend(forgecode_metric_file_touches(
                &row,
                metrics,
                &raw_source_path,
                ended_at.unwrap_or(started_at),
            ));
        }

        let mut emitted_events = false;
        if let Some(messages) = context_value
            .as_ref()
            .and_then(|value| value.get("messages"))
            .and_then(Value::as_array)
        {
            for (index, entry) in messages.iter().enumerate() {
                let provider_event_index = (index as u64).saturating_add(1);
                let occurred_at =
                    started_at + Duration::milliseconds(i64::try_from(index).unwrap_or(i64::MAX));
                let event = forgecode_event(&row, entry, provider_event_index, occurred_at);
                let line = provider_line_from_index(provider_event_index);
                result
                    .files_touched
                    .extend(provider_file_touches_from_raw_value(
                        CaptureProvider::ForgeCode,
                        &row.conversation_id,
                        FORGECODE_SQLITE_SOURCE_FORMAT,
                        Some(raw_source_path.as_str()),
                        entry,
                        &event,
                        line,
                    ));
                result.captures.push((
                    line,
                    forgecode_capture(
                        &row,
                        ForgeCodeCaptureContext {
                            started_at,
                            ended_at,
                            raw_source_path: &raw_source_path,
                            user_version,
                            schema_fingerprint: &schema_fingerprint,
                            context_value: context_value.as_ref(),
                            metrics_value: metrics_value.as_ref(),
                            event: Some(event),
                        },
                        context,
                    ),
                ));
                emitted_events = true;
            }
        }

        if !emitted_events {
            result.captures.push((
                row_line,
                forgecode_capture(
                    &row,
                    ForgeCodeCaptureContext {
                        started_at,
                        ended_at,
                        raw_source_path: &raw_source_path,
                        user_version,
                        schema_fingerprint: &schema_fingerprint,
                        context_value: context_value.as_ref(),
                        metrics_value: metrics_value.as_ref(),
                        event: None,
                    },
                    context,
                ),
            ));
        }
    }

    Ok(result)
}

pub(crate) fn forgecode_conversations(conn: &Connection) -> Result<Vec<ForgeCodeConversationRow>> {
    if !sqlite_table_exists(conn, "conversations")? {
        return Err(CaptureError::InvalidPayload(
            "ForgeCode .forge.db is missing required conversations table".into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "conversations")?;
    ensure_sqlite_table_columns(
        &columns,
        "ForgeCode conversations table",
        &["conversation_id", "workspace_id", "created_at"],
    )?;
    let title = optional_column_expr(&columns, "title", "NULL");
    let context = optional_column_expr(&columns, "context", "NULL");
    let updated_at = optional_column_expr(&columns, "updated_at", "NULL");
    let metrics = optional_column_expr(&columns, "metrics", "NULL");
    let order_by = if columns.contains("updated_at") {
        "COALESCE(updated_at, created_at), conversation_id"
    } else {
        "created_at, conversation_id"
    };
    let sql = format!(
        "select rowid, CAST(conversation_id AS TEXT), {title}, workspace_id, {context}, \
         CAST(created_at AS TEXT), CAST({updated_at} AS TEXT), {metrics} \
         from conversations order by {order_by}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok(ForgeCodeConversationRow {
            rowid: row.get(0)?,
            conversation_id: row.get(1)?,
            title: row.get(2)?,
            workspace_id: row.get(3)?,
            context: row.get(4)?,
            created_at: row.get(5)?,
            updated_at: row.get(6)?,
            metrics: row.get(7)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CaptureError::from)
}

pub(crate) fn forgecode_capture(
    row: &ForgeCodeConversationRow,
    draft: ForgeCodeCaptureContext<'_>,
    context: &ProviderAdapterContext,
) -> ProviderCaptureEnvelope {
    let context_message_count = draft
        .context_value
        .and_then(|value| value.get("messages"))
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    native_provider_capture(
        NativeSessionDraft {
            provider: CaptureProvider::ForgeCode,
            source_format: FORGECODE_SQLITE_SOURCE_FORMAT,
            provider_session_id: row.conversation_id.clone(),
            parent_provider_session_id: None,
            root_provider_session_id: None,
            external_agent_id: draft
                .context_value
                .and_then(|value| value.get("initiator"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            agent_type: AgentType::Primary,
            role_hint: Some("primary".to_owned()),
            is_primary: true,
            started_at: draft.started_at,
            ended_at: draft.ended_at,
            cwd: None,
            fidelity: Fidelity::Imported,
            raw_source_path: draft.raw_source_path.to_owned(),
            trust: ProviderSourceTrust::ProviderNative,
            source_metadata: json!({
                "adapter": FORGECODE_SQLITE_SOURCE_FORMAT,
                "sqlite_user_version": draft.user_version,
                "schema_fingerprint": draft.schema_fingerprint,
                "source_path": draft.raw_source_path,
                "upstream_tables": ["conversations"],
                "upstream_schema_anchor": "crates/forge_repo/src/database/migrations/2025-09-12-065405_create_conversations_table/up.sql",
                "upstream_dto_anchor": "crates/forge_repo/src/conversation/conversation_record.rs",
            }),
            session_metadata: json!({
                "source_format": FORGECODE_SQLITE_SOURCE_FORMAT,
                "conversation_id": row.conversation_id,
                "title": row.title,
                "workspace_id": row.workspace_id,
                "created_at": row.created_at,
                "updated_at": row.updated_at,
                "context_conversation_id": draft.context_value
                    .and_then(|value| value.get("conversation_id"))
                    .and_then(Value::as_str),
                "initiator": draft.context_value
                    .and_then(|value| value.get("initiator"))
                    .and_then(Value::as_str),
                "context_message_count": context_message_count,
                "tools_count": draft.context_value
                    .and_then(|value| value.get("tools"))
                    .and_then(Value::as_array)
                    .map(Vec::len),
                "tool_choice": draft.context_value
                    .and_then(|value| value.get("tool_choice"))
                    .map(|value| provider_capped_json_value(value, PROVIDER_MAX_PREVIEW_CHARS)),
                "context": draft.context_value
                    .map(|value| provider_capped_json_value(value, PROVIDER_MAX_PREVIEW_CHARS)),
                "metrics": draft.metrics_value
                    .map(|value| provider_capped_json_value(value, PROVIDER_MAX_PREVIEW_CHARS)),
                "limitations": [
                    "ForgeCode stores conversation messages as a context JSON snapshot; message cursors use array index because the DTO does not expose stable message ids",
                    "recognized text, tool call, tool result, image, usage, and metrics fields are normalized; unrecognized DTO fields are retained as capped raw JSON metadata",
                    "workspace_id is retained, but the current Forge schema does not keep a workspace path after the workspace table was dropped"
                ],
            }),
        },
        context,
        draft.event,
    )
}

pub(crate) fn forgecode_event(
    row: &ForgeCodeConversationRow,
    entry: &Value,
    provider_event_index: u64,
    occurred_at: DateTime<Utc>,
) -> ProviderEventEnvelope {
    let parts = forgecode_message_parts(entry);
    let event_type = forgecode_event_type(parts);
    let role = forgecode_event_role(parts);
    let text = forgecode_message_text(parts, event_type);
    let message_hash = compute_payload_hash(entry).ok();
    native_event(NativeEventDraft {
        provider: CaptureProvider::ForgeCode,
        source_format: FORGECODE_SQLITE_SOURCE_FORMAT,
        provider_session_id: row.conversation_id.clone(),
        provider_event_index,
        provider_event_hash: message_hash,
        cursor: format!(
            "conversation:{}:message:{}",
            row.conversation_id, provider_event_index
        ),
        event_type,
        role,
        occurred_at,
        text,
        body: json!({
            "message_index": provider_event_index,
            "message_variant": parts.variant,
            "message": entry,
            "usage": parts.usage,
        }),
        metadata: json!({
            "source": "forgecode_conversations",
            "source_format": FORGECODE_SQLITE_SOURCE_FORMAT,
            "conversation_id": row.conversation_id,
            "message_index": provider_event_index,
            "message_variant": parts.variant,
            "role": forgecode_role_text(parts),
            "model": forgecode_text_body(parts)
                .and_then(|body| body.get("model"))
                .and_then(provider_value_text),
            "usage": parts.usage
                .map(|value| provider_capped_json_value(value, PROVIDER_MAX_PREVIEW_CHARS)),
        }),
    })
}

pub(crate) fn forgecode_message_parts(entry: &Value) -> ForgeCodeMessageParts<'_> {
    let message = entry.get("message").unwrap_or(entry);
    let usage = entry.get("usage");
    if let Some((variant, body)) = forgecode_message_variant(message) {
        return ForgeCodeMessageParts {
            variant,
            body,
            usage,
        };
    }
    ForgeCodeMessageParts {
        variant: "unknown",
        body: message,
        usage,
    }
}

pub(crate) fn forgecode_message_variant(value: &Value) -> Option<(&'static str, &Value)> {
    let Value::Object(object) = value else {
        return None;
    };
    object
        .iter()
        .find_map(|(key, value)| match normalized_key(key).as_str() {
            "text" => Some(("text", value)),
            "tool" => Some(("tool", value)),
            "image" => Some(("image", value)),
            _ => None,
        })
}

pub(crate) fn forgecode_event_type(parts: ForgeCodeMessageParts<'_>) -> EventType {
    match parts.variant {
        "text" if forgecode_text_has_tool_calls(parts.body) => EventType::ToolCall,
        "text" => EventType::Message,
        "tool" => EventType::ToolOutput,
        "image" => EventType::Artifact,
        _ => EventType::Notice,
    }
}

pub(crate) fn forgecode_event_role(parts: ForgeCodeMessageParts<'_>) -> Option<EventRole> {
    match parts.variant {
        "text" => forgecode_role_text(parts).map(|role| provider_role(Some(&role))),
        "tool" => Some(EventRole::Tool),
        "image" => Some(EventRole::Unknown),
        _ => None,
    }
}

pub(crate) fn forgecode_role_text(parts: ForgeCodeMessageParts<'_>) -> Option<String> {
    forgecode_text_body(parts)
        .and_then(|body| body.get("role"))
        .and_then(Value::as_str)
        .map(|role| role.to_ascii_lowercase())
}

pub(crate) fn forgecode_text_body(parts: ForgeCodeMessageParts<'_>) -> Option<&Value> {
    (parts.variant == "text").then_some(parts.body)
}

pub(crate) fn forgecode_text_has_tool_calls(body: &Value) -> bool {
    body.get("tool_calls")
        .or_else(|| body.get("toolCalls"))
        .and_then(Value::as_array)
        .is_some_and(|calls| !calls.is_empty())
}

pub(crate) fn forgecode_message_text(
    parts: ForgeCodeMessageParts<'_>,
    event_type: EventType,
) -> String {
    match parts.variant {
        "text" => forgecode_text_message_text(parts.body, event_type),
        "tool" => forgecode_tool_result_text(parts.body),
        "image" => forgecode_image_text(parts.body),
        _ => {
            provider_value_text(parts.body).unwrap_or_else(|| "ForgeCode conversation event".into())
        }
    }
}

pub(crate) fn forgecode_text_message_text(body: &Value, event_type: EventType) -> String {
    let mut parts = Vec::new();
    if let Some(content) = body
        .get("content")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
    {
        parts.push(content.to_owned());
    }
    if let Some(tool_text) = body
        .get("tool_calls")
        .or_else(|| body.get("toolCalls"))
        .and_then(forgecode_tool_calls_text)
    {
        parts.push(tool_text);
    }
    if parts.is_empty() {
        if let Some(raw_content) = body.get("raw_content").and_then(provider_value_text) {
            parts.push(raw_content);
        }
    }
    if parts.is_empty() {
        let role = body
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        parts.push(if event_type == EventType::ToolCall {
            format!("ForgeCode {role} tool call")
        } else {
            format!("ForgeCode {role} message")
        });
    }
    parts.join("\n")
}

pub(crate) fn forgecode_tool_calls_text(value: &Value) -> Option<String> {
    let calls = value.as_array()?;
    let mut parts = Vec::new();
    for call in calls {
        let name = call
            .get("name")
            .and_then(forgecode_scalar_text)
            .unwrap_or_else(|| "tool".to_owned());
        parts.push(format!("tool call: {name}"));
        if let Some(call_id) = call.get("call_id").and_then(forgecode_scalar_text) {
            parts.push(format!("tool call id: {call_id}"));
        }
        if let Some(arguments) = call
            .get("arguments")
            .and_then(provider_value_text)
            .filter(|text| !text.trim().is_empty())
        {
            parts.push(format!("tool input: {arguments}"));
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

pub(crate) fn forgecode_tool_result_text(body: &Value) -> String {
    let name = body
        .get("name")
        .and_then(forgecode_scalar_text)
        .unwrap_or_else(|| "tool".to_owned());
    let mut parts = vec![format!("tool result: {name}")];
    if let Some(call_id) = body.get("call_id").and_then(forgecode_scalar_text) {
        parts.push(format!("tool call id: {call_id}"));
    }
    if body
        .pointer("/output/is_error")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        parts.push("tool error".to_owned());
    }
    if let Some(values) = body.pointer("/output/values").and_then(Value::as_array) {
        for value in values {
            if let Some(text) = forgecode_tool_value_text(value) {
                parts.push(text);
            }
        }
    }
    parts.join("\n")
}

pub(crate) fn forgecode_tool_value_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Object(object) => {
            for (key, child) in object {
                match normalized_key(key).as_str() {
                    "text" | "markdown" => return child.as_str().map(str::to_owned),
                    "ai" => {
                        return child
                            .get("value")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                            .or_else(|| provider_value_text(child));
                    }
                    "image" => return Some(forgecode_image_text(child)),
                    "filediff" => {
                        let path = child
                            .get("path")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown");
                        return Some(format!("[File diff: {path}]"));
                    }
                    "pair" => {
                        if let Some(items) = child.as_array() {
                            return items.first().and_then(forgecode_tool_value_text);
                        }
                    }
                    "empty" => return None,
                    _ => {}
                }
            }
            provider_value_text(value)
        }
        Value::Array(items) => {
            let parts = items
                .iter()
                .filter_map(forgecode_tool_value_text)
                .collect::<Vec<_>>();
            (!parts.is_empty()).then(|| parts.join("\n"))
        }
        Value::Number(_) | Value::Bool(_) => Some(value.to_string()),
        Value::Null => None,
    }
}

pub(crate) fn forgecode_image_text(body: &Value) -> String {
    let mime_type = body
        .get("mime_type")
        .or_else(|| body.get("mimeType"))
        .and_then(Value::as_str)
        .unwrap_or("image");
    let url = body
        .get("url")
        .and_then(Value::as_str)
        .filter(|url| !url.trim().is_empty());
    match url {
        Some(url) => format!("ForgeCode image: {mime_type} {url}"),
        None => format!("ForgeCode image: {mime_type}"),
    }
}

pub(crate) fn forgecode_scalar_text(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| provider_value_text(value))
}
