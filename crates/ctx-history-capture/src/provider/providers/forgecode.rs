use std::{collections::BTreeSet, path::Path};

use chrono::{DateTime, Duration, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, Confidence, EventRole, EventType, Fidelity, FileChangeKind,
    ProviderCaptureEnvelope, ProviderEventEnvelope, ProviderSourceTrust,
};
use rusqlite::Connection;
use serde_json::{json, Value};

use crate::compute_payload_hash;
use crate::provider::file_touches::normalized_key;
use crate::provider::providers::goose::goose_timestamp;

use crate::provider::custom_history_jsonl::push_provider_import_failure;
use crate::provider::file_touches::provider_file_touches_from_raw_value;
use crate::provider::native::{
    native_event, native_provider_capture, open_provider_sqlite_readonly,
    provider_capped_json_value, provider_line_from_index, provider_role, provider_timestamp_value,
    provider_value_text, NativeEventDraft, NativeSessionDraft,
};
use crate::provider::sqlite::{
    ensure_sqlite_table_columns, opencode_schema_fingerprint, optional_column_expr,
    sqlite_table_columns, sqlite_table_exists,
};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderFileTouchedEnvelope, ProviderNormalizationResult,
    Result, FORGECODE_SQLITE_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS,
};

pub(crate) struct ForgeCodeConversationRow {
    pub(crate) rowid: i64,
    pub(crate) conversation_id: String,
    pub(crate) title: Option<String>,
    pub(crate) workspace_id: i64,
    pub(crate) context: Option<String>,
    pub(crate) created_at: String,
    pub(crate) updated_at: Option<String>,
    pub(crate) metrics: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ForgeCodeMessageParts<'a> {
    pub(crate) variant: &'static str,
    pub(crate) body: &'a Value,
    pub(crate) usage: Option<&'a Value>,
}

pub(crate) struct ForgeCodeCaptureContext<'a> {
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) ended_at: Option<DateTime<Utc>>,
    pub(crate) raw_source_path: &'a str,
    pub(crate) user_version: i64,
    pub(crate) schema_fingerprint: &'a str,
    pub(crate) context_value: Option<&'a Value>,
    pub(crate) metrics_value: Option<&'a Value>,
    pub(crate) event: Option<ProviderEventEnvelope>,
}

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

pub(crate) fn forgecode_metric_file_touches(
    row: &ForgeCodeConversationRow,
    metrics: &Value,
    raw_source_path: &str,
    fallback: DateTime<Utc>,
) -> Vec<(usize, ProviderFileTouchedEnvelope)> {
    let occurred_at = metrics
        .get("started_at")
        .map(|value| provider_timestamp_value(Some(value), fallback))
        .unwrap_or(fallback);
    let mut touches = Vec::new();
    let mut seen = BTreeSet::<(String, &'static str)>::new();

    if let Some(files_changed) = metrics.get("files_changed").and_then(Value::as_object) {
        let mut entries = files_changed.iter().collect::<Vec<_>>();
        entries.sort_by(|left, right| left.0.cmp(right.0));
        for (path, operation_value) in entries {
            let Some(operation) = forgecode_metric_operation(operation_value) else {
                continue;
            };
            let tool = operation
                .get("tool")
                .and_then(Value::as_str)
                .unwrap_or("write");
            let change_kind = forgecode_metric_change_kind(tool);
            if !seen.insert((path.clone(), change_kind.as_str())) {
                continue;
            }
            let lines_added = operation.get("lines_added").and_then(forgecode_json_i64);
            let lines_removed = operation.get("lines_removed").and_then(forgecode_json_i64);
            let line_count_delta = match (lines_added, lines_removed) {
                (Some(added), Some(removed)) => Some(added.saturating_sub(removed)),
                (Some(added), None) => Some(added),
                (None, Some(removed)) => Some(removed.saturating_neg()),
                _ => None,
            };
            let touch_index = 0x0400_0000_0000_u64.saturating_add(touches.len() as u64);
            touches.push((
                provider_line_from_index(touch_index),
                ProviderFileTouchedEnvelope {
                    provider: CaptureProvider::ForgeCode,
                    provider_session_id: row.conversation_id.clone(),
                    provider_touch_index: touch_index,
                    provider_event_index: None,
                    raw_source_path: Some(raw_source_path.to_owned()),
                    path: path.clone(),
                    change_kind: Some(change_kind),
                    old_path: None,
                    line_count_delta,
                    confidence: Confidence::Explicit,
                    occurred_at,
                    source_format: FORGECODE_SQLITE_SOURCE_FORMAT.to_owned(),
                    metadata: json!({
                        "source": "forgecode_metrics_files_changed",
                        "tool": tool,
                        "lines_added": lines_added,
                        "lines_removed": lines_removed,
                        "content_hash": operation.get("content_hash").and_then(Value::as_str),
                    }),
                },
            ));
        }
    }

    if let Some(files_accessed) = metrics.get("files_accessed").and_then(Value::as_array) {
        let mut paths = files_accessed
            .iter()
            .filter_map(Value::as_str)
            .filter(|path| !path.trim().is_empty())
            .collect::<Vec<_>>();
        paths.sort_unstable();
        paths.dedup();
        for path in paths {
            if !seen.insert((path.to_owned(), FileChangeKind::Read.as_str())) {
                continue;
            }
            let touch_index = 0x0500_0000_0000_u64.saturating_add(touches.len() as u64);
            touches.push((
                provider_line_from_index(touch_index),
                ProviderFileTouchedEnvelope {
                    provider: CaptureProvider::ForgeCode,
                    provider_session_id: row.conversation_id.clone(),
                    provider_touch_index: touch_index,
                    provider_event_index: None,
                    raw_source_path: Some(raw_source_path.to_owned()),
                    path: path.to_owned(),
                    change_kind: Some(FileChangeKind::Read),
                    old_path: None,
                    line_count_delta: None,
                    confidence: Confidence::Explicit,
                    occurred_at,
                    source_format: FORGECODE_SQLITE_SOURCE_FORMAT.to_owned(),
                    metadata: json!({
                        "source": "forgecode_metrics_files_accessed",
                    }),
                },
            ));
        }
    }

    touches
}

pub(crate) fn forgecode_metric_operation(value: &Value) -> Option<&Value> {
    match value {
        Value::Object(_) => Some(value),
        Value::Array(items) => items.iter().rev().find(|item| item.is_object()),
        _ => None,
    }
}

pub(crate) fn forgecode_metric_change_kind(tool: &str) -> FileChangeKind {
    match tool.to_ascii_lowercase().as_str() {
        "read" => FileChangeKind::Read,
        "patch" | "edit" | "update" | "write" => FileChangeKind::Modified,
        "delete" | "remove" => FileChangeKind::Deleted,
        "create" | "add" => FileChangeKind::Created,
        _ => FileChangeKind::Unknown,
    }
}

pub(crate) fn forgecode_json_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
}

pub(crate) fn forgecode_timestamp(raw: Option<&str>, fallback: DateTime<Utc>) -> DateTime<Utc> {
    goose_timestamp(raw, fallback)
}
