use std::{borrow::Cow, io::Read, path::Path};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventRole, EventType, Fidelity, ProviderCaptureEnvelope,
    ProviderEventEnvelope, ProviderSourceTrust,
};
use rusqlite::Connection;
use serde_json::{json, Value};

use crate::compute_payload_hash;

use crate::common::time::parse_rfc3339_utc;
use crate::provider::custom_history_jsonl::push_provider_import_failure;
use crate::provider::file_touches::provider_file_touches_from_raw_value;
use crate::provider::native::{
    native_event, native_provider_capture, open_provider_sqlite_readonly, provider_line_from_index,
    provider_value_text, NativeEventDraft, NativeSessionDraft,
};
use crate::provider::sqlite::{
    ensure_sqlite_table_columns, opencode_schema_fingerprint, optional_column_expr,
    sqlite_table_columns, sqlite_table_exists,
};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderNormalizationResult, Result,
    MAX_PROVIDER_SQLITE_VALUE_BYTES, ZED_THREADS_SQLITE_SOURCE_FORMAT,
};

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
            let events =
                match zed_message_events(&row.id, message, message_index, thread_updated_at) {
                    Ok(events) => events,
                    Err(err) => {
                        let line = zed_line_number(row.rowid, message_index as u64);
                        push_provider_import_failure(&mut result.summary, line, err.to_string());
                        continue;
                    }
                };
            for (event_ordinal, event) in events.into_iter().enumerate() {
                let line = zed_line_number(row.rowid, event.provider_event_index);
                if event_ordinal == 0 {
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
                }
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
            "Zed compressed thread JSON exceeds {MAX_PROVIDER_SQLITE_VALUE_BYTES} decompressed bytes"
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
const ZED_EVENTS_PER_MESSAGE: u64 = 2;
const ZED_SPLIT_EVENT_IDENTITY_INDEX_OFFSET: u64 = 1_000_000;

pub(crate) fn zed_message_events(
    provider_session_id: &str,
    message: &Value,
    message_index: usize,
    occurred_at: DateTime<Utc>,
) -> Result<Vec<ProviderEventEnvelope>> {
    let kind = zed_message_kind(message).unwrap_or("Unknown");
    if kind == "Agent" && zed_has_tool_use(message) && zed_has_tool_result(message) {
        return Ok(vec![
            zed_message_event(
                provider_session_id,
                message,
                message_index,
                occurred_at,
                EventType::ToolCall,
                "tool_call",
                0,
            )?,
            zed_message_event(
                provider_session_id,
                message,
                message_index,
                occurred_at,
                EventType::ToolOutput,
                "tool_output",
                1,
            )?,
        ]);
    }
    let event_type = zed_message_event_type(kind, message);
    Ok(vec![zed_message_event(
        provider_session_id,
        message,
        message_index,
        occurred_at,
        event_type,
        "message",
        0,
    )?])
}

pub(crate) fn zed_message_event(
    provider_session_id: &str,
    message: &Value,
    message_index: usize,
    occurred_at: DateTime<Utc>,
    event_type: EventType,
    event_suffix: &str,
    split_index: u64,
) -> Result<ProviderEventEnvelope> {
    let kind = zed_message_kind(message).unwrap_or("Unknown");
    let text = zed_message_text_for_event_type(kind, message, event_type)
        .unwrap_or_else(|| format!("Zed {kind} message"));
    let role = zed_message_role(kind);
    let message_event_index = u64::try_from(message_index).map_err(|_| {
        CaptureError::InvalidPayload(format!("Zed message index is too large: {message_index}"))
    })?;
    let provider_event_index = message_event_index
        .saturating_mul(ZED_EVENTS_PER_MESSAGE)
        .saturating_add(split_index);
    let provider_event_identity_index = if split_index == 0 {
        message_event_index
    } else {
        message_event_index
            .saturating_add(ZED_SPLIT_EVENT_IDENTITY_INDEX_OFFSET.saturating_mul(split_index))
    };
    let message_hash = if split_index == 0 && event_suffix == "message" {
        compute_payload_hash(message)?
    } else {
        compute_payload_hash(&json!({
            "event_suffix": event_suffix,
            "message": message,
        }))?
    };
    let cursor = if split_index == 0 && event_suffix == "message" {
        format!("thread:{provider_session_id}:message:{message_index}")
    } else {
        format!("thread:{provider_session_id}:message:{message_index}:{event_suffix}")
    };
    Ok(native_event(NativeEventDraft {
        provider: CaptureProvider::Zed,
        source_format: ZED_THREADS_SQLITE_SOURCE_FORMAT,
        provider_session_id: provider_session_id.to_owned(),
        provider_event_index,
        provider_event_hash: Some(format!("zed-message:{message_hash}")),
        cursor,
        event_type,
        role,
        occurred_at,
        text,
        body: zed_message_body(kind, message, event_type),
        metadata: json!({
            "source": "zed_threads_db",
            "source_format": ZED_THREADS_SQLITE_SOURCE_FORMAT,
            "message_index": message_index,
            "message_kind": kind,
            "event_suffix": event_suffix,
            "split_index": split_index,
            "provider_event_identity_index": provider_event_identity_index,
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
        "Agent" if zed_has_tool_result(message) => EventType::ToolOutput,
        "Agent" if zed_has_tool_use(message) => EventType::ToolCall,
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

pub(crate) fn zed_message_text_for_event_type(
    kind: &str,
    message: &Value,
    event_type: EventType,
) -> Option<String> {
    if kind == "Agent" {
        let inner = zed_message_inner(message, kind)?;
        return match event_type {
            EventType::ToolCall => zed_content_array_text(inner.get("content")),
            EventType::ToolOutput => zed_tool_results_text(inner.get("tool_results")),
            _ => zed_agent_message_text(inner),
        };
    }
    zed_message_text(message)
}

pub(crate) fn zed_message_body(kind: &str, message: &Value, event_type: EventType) -> Value {
    match event_type {
        EventType::ToolCall => json!({
            "message_kind": kind,
            "raw_message_retention": "metadata_only",
            "tool_uses": zed_tool_use_summaries(message),
        }),
        EventType::ToolOutput => json!({
            "message_kind": kind,
            "raw_message_retention": "metadata_only",
            "tool_results": zed_tool_result_summaries(message),
        }),
        _ => json!({
            "message_kind": kind,
            "message": message,
        }),
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
    if value.get("input").is_some_and(|input| !input.is_null())
        || value
            .get("raw_input")
            .and_then(Value::as_str)
            .is_some_and(|raw_input| !raw_input.trim().is_empty())
    {
        parts.push("tool input: present".to_owned());
    }
    parts.join("\n")
}

pub(crate) fn zed_tool_use_summaries(value: &Value) -> Vec<Value> {
    let mut summaries = Vec::new();
    zed_collect_tool_use_summaries(value, &mut summaries);
    summaries
}

fn zed_collect_tool_use_summaries(value: &Value, summaries: &mut Vec<Value>) {
    match value {
        Value::Array(items) => {
            for item in items {
                zed_collect_tool_use_summaries(item, summaries);
            }
        }
        Value::Object(object) => {
            if let Some(tool_use) = object.get("ToolUse") {
                summaries.push(zed_tool_use_summary(tool_use));
            }
            for nested in object.values() {
                zed_collect_tool_use_summaries(nested, summaries);
            }
        }
        _ => {}
    }
}

fn zed_tool_use_summary(value: &Value) -> Value {
    let input = value.get("input").filter(|input| !input.is_null());
    json!({
        "id": value.get("id").and_then(Value::as_str),
        "name": value.get("name").and_then(Value::as_str),
        "input_present": input.is_some(),
        "input_kind": input.map(zed_value_kind),
        "raw_input_present": value
            .get("raw_input")
            .and_then(Value::as_str)
            .is_some_and(|raw_input| !raw_input.trim().is_empty()),
    })
}

pub(crate) fn zed_tool_result_summaries(value: &Value) -> Vec<Value> {
    let mut summaries = Vec::new();
    zed_collect_tool_result_summaries(value, &mut summaries);
    summaries
}

fn zed_collect_tool_result_summaries(value: &Value, summaries: &mut Vec<Value>) {
    match value {
        Value::Array(items) => {
            for item in items {
                zed_collect_tool_result_summaries(item, summaries);
            }
        }
        Value::Object(object) => {
            if let Some(tool_result) = object.get("ToolResult") {
                summaries.push(zed_tool_result_summary(tool_result));
            }
            if let Some(results) = object.get("tool_results").and_then(Value::as_object) {
                for result in results.values() {
                    summaries.push(zed_tool_result_summary(result));
                }
            }
            for nested in object.values() {
                zed_collect_tool_result_summaries(nested, summaries);
            }
        }
        _ => {}
    }
}

fn zed_tool_result_summary(value: &Value) -> Value {
    json!({
        "id": value.get("id").and_then(Value::as_str),
        "tool_name": value.get("tool_name").and_then(Value::as_str),
        "is_error": value.get("is_error").and_then(Value::as_bool).unwrap_or(false),
        "content_present": value.get("content").is_some_and(|content| !content.is_null()),
        "output_present": value.get("output").is_some_and(|output| !output.is_null()),
    })
}

fn zed_value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
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

pub(crate) fn zed_tool_results_text(value: Option<&Value>) -> Option<String> {
    let object = value?.as_object()?;
    let mut parts = Vec::new();
    for result in object.values() {
        let name = result
            .get("tool_name")
            .and_then(Value::as_str)
            .unwrap_or("tool");
        parts.push(format!("tool result: {name}"));
        if result
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            parts.push("tool error".to_owned());
        }
        if let Some(content) = zed_tool_result_content_text(result.get("content")) {
            parts.push(content);
        }
        if let Some(output) = result.get("output").and_then(provider_value_text) {
            parts.push(output);
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

pub(crate) fn zed_tool_result_content_text(value: Option<&Value>) -> Option<String> {
    let value = value?;
    if let Some(text) = value.as_str() {
        return Some(text.to_owned());
    }
    if let Some(items) = value.as_array() {
        let mut parts = Vec::new();
        for item in items {
            if let Some((kind, body)) = zed_external_tag(item) {
                match kind {
                    "Text" => {
                        if let Some(text) = body.as_str() {
                            parts.push(text.to_owned());
                        }
                    }
                    "Image" => parts.push("<image />".to_owned()),
                    _ => {
                        if let Some(text) = provider_value_text(body) {
                            parts.push(text);
                        }
                    }
                }
            } else if let Some(text) = provider_value_text(item) {
                parts.push(text);
            }
        }
        return (!parts.is_empty()).then(|| parts.join("\n"));
    }
    provider_value_text(value)
}

pub(crate) fn zed_external_tag(value: &Value) -> Option<(&str, &Value)> {
    let object = value.as_object()?;
    if object.len() != 1 {
        return None;
    }
    object
        .iter()
        .next()
        .map(|(key, value)| (key.as_str(), value))
}

pub(crate) fn zed_has_tool_use(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.iter().any(zed_has_tool_use),
        Value::Object(object) => {
            object.contains_key("ToolUse")
                || object.get("content").is_some_and(zed_has_tool_use)
                || object.values().any(zed_has_tool_use)
        }
        _ => false,
    }
}

pub(crate) fn zed_has_tool_result(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.iter().any(zed_has_tool_result),
        Value::Object(object) => {
            object
                .get("tool_results")
                .and_then(Value::as_object)
                .is_some_and(|results| !results.is_empty())
                || object.contains_key("ToolResult")
                || object.values().any(zed_has_tool_result)
        }
        _ => false,
    }
}
