use std::path::Path;

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventRole, EventType, Fidelity, ProviderCaptureEnvelope,
    ProviderEventEnvelope, ProviderSourceTrust,
};
use rusqlite::Connection;
use serde_json::{json, Value};

use crate::provider::custom_history_jsonl::push_provider_import_failure;
use crate::provider::file_touches::provider_file_touches_from_raw_value;
use crate::provider::native::{
    native_event, native_provider_capture, open_provider_sqlite_readonly,
    provider_capped_json_value, provider_line_from_index, provider_nonnegative_i64_to_u64,
    provider_timestamp_millis, provider_timestamp_value, provider_value_text, NativeEventDraft,
    NativeSessionDraft,
};
use crate::provider::sqlite::{
    ensure_sqlite_table_columns, opencode_schema_fingerprint, sqlite_table_columns,
    sqlite_table_exists,
};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderNormalizationResult, Result,
    KIRO_SQLITE_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS,
};

pub(crate) struct KiroConversationRow {
    pub(crate) table: &'static str,
    pub(crate) rowid: i64,
    pub(crate) key: String,
    pub(crate) conversation_id: Option<String>,
    pub(crate) value: String,
    pub(crate) created_at: Option<i64>,
    pub(crate) updated_at: Option<i64>,
}

pub(crate) fn normalize_kiro_sqlite(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    let conn = open_provider_sqlite_readonly(path)?;
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let schema_fingerprint = opencode_schema_fingerprint(&conn)?;
    let conversations = kiro_conversation_rows(&conn)?;
    let raw_source_path = path.display().to_string();
    let mut result = ProviderNormalizationResult::default();

    for row in conversations {
        let row_index = match provider_nonnegative_i64_to_u64(row.rowid, "Kiro conversation rowid")
        {
            Ok(index) => index,
            Err(err) => {
                push_provider_import_failure(&mut result.summary, 0, err.to_string());
                continue;
            }
        };
        let line = provider_line_from_index(row_index);
        let value: Value = match serde_json::from_str(&row.value) {
            Ok(value) => value,
            Err(err) => {
                push_provider_import_failure(
                    &mut result.summary,
                    line,
                    format!(
                        "invalid JSON in Kiro {} row {} for key {}: {err}",
                        row.table, row.rowid, row.key
                    ),
                );
                continue;
            }
        };
        let provider_session_id = kiro_provider_session_id(&row, &value);
        let started_at = kiro_session_started_at(&row, &value, context.imported_at);
        let ended_at = Some(kiro_session_ended_at(&row, &value, started_at));
        let history = value.get("history").and_then(Value::as_array);
        let mut emitted_event = false;

        if let Some(history) = history {
            for (history_index, entry) in history.iter().enumerate() {
                let user_at = kiro_entry_timestamp(entry, "user", started_at);
                if let Some(text) = kiro_user_prompt_text(entry) {
                    let event = kiro_event(
                        &row,
                        &provider_session_id,
                        history_index,
                        0,
                        EventType::Message,
                        EventRole::User,
                        user_at,
                        text,
                        entry,
                        None,
                    );
                    result
                        .files_touched
                        .extend(provider_file_touches_from_raw_value(
                            CaptureProvider::KiroCli,
                            &provider_session_id,
                            KIRO_SQLITE_SOURCE_FORMAT,
                            Some(raw_source_path.as_str()),
                            entry,
                            &event,
                            line,
                        ));
                    result.captures.push((
                        line,
                        kiro_capture(
                            &row,
                            &provider_session_id,
                            &value,
                            started_at,
                            ended_at,
                            &raw_source_path,
                            user_version,
                            &schema_fingerprint,
                            Some(event),
                            context,
                        ),
                    ));
                    emitted_event = true;
                }

                if let Some(assistant) = kiro_assistant_message(entry) {
                    let assistant_at = kiro_entry_timestamp(entry, "assistant", user_at);
                    let event = kiro_event(
                        &row,
                        &provider_session_id,
                        history_index,
                        1,
                        assistant.event_type,
                        EventRole::Assistant,
                        assistant_at,
                        assistant.text,
                        entry,
                        assistant.tool_uses,
                    );
                    result
                        .files_touched
                        .extend(provider_file_touches_from_raw_value(
                            CaptureProvider::KiroCli,
                            &provider_session_id,
                            KIRO_SQLITE_SOURCE_FORMAT,
                            Some(raw_source_path.as_str()),
                            entry,
                            &event,
                            line,
                        ));
                    result.captures.push((
                        line,
                        kiro_capture(
                            &row,
                            &provider_session_id,
                            &value,
                            started_at,
                            ended_at,
                            &raw_source_path,
                            user_version,
                            &schema_fingerprint,
                            Some(event),
                            context,
                        ),
                    ));
                    emitted_event = true;
                }
            }
        }

        if !emitted_event {
            result.captures.push((
                line,
                kiro_capture(
                    &row,
                    &provider_session_id,
                    &value,
                    started_at,
                    ended_at,
                    &raw_source_path,
                    user_version,
                    &schema_fingerprint,
                    None,
                    context,
                ),
            ));
        }
    }

    Ok(result)
}

pub(crate) struct KiroAssistantMessage {
    pub(crate) event_type: EventType,
    pub(crate) text: String,
    pub(crate) tool_uses: Option<Value>,
}

pub(crate) fn kiro_event(
    row: &KiroConversationRow,
    provider_session_id: &str,
    history_index: usize,
    part_index: u64,
    event_type: EventType,
    role: EventRole,
    occurred_at: DateTime<Utc>,
    text: String,
    entry: &Value,
    tool_uses: Option<Value>,
) -> ProviderEventEnvelope {
    let provider_event_index = history_index
        .saturating_mul(2)
        .saturating_add(part_index as usize) as u64;
    let role_name = match role {
        EventRole::User => "user",
        EventRole::Assistant => "assistant",
        EventRole::System => "system",
        EventRole::Tool => "tool",
        EventRole::Unknown => "unknown",
    };
    native_event(NativeEventDraft {
        provider: CaptureProvider::KiroCli,
        source_format: KIRO_SQLITE_SOURCE_FORMAT,
        provider_session_id: provider_session_id.to_owned(),
        provider_event_index,
        provider_event_hash: Some(format!(
            "{}:{}:{}:{role_name}",
            row.table, provider_session_id, history_index
        )),
        cursor: format!(
            "{}:{}:history:{}:{role_name}",
            row.table, provider_session_id, history_index
        ),
        event_type,
        role: Some(role),
        occurred_at,
        text,
        body: json!({
            "table": row.table,
            "key": row.key,
            "conversation_id": provider_session_id,
            "history_index": history_index,
            "role": role_name,
            "entry": entry,
            "tool_uses": tool_uses,
        }),
        metadata: json!({
            "source": row.table,
            "source_format": KIRO_SQLITE_SOURCE_FORMAT,
            "key": row.key,
            "conversation_id": provider_session_id,
            "history_index": history_index,
            "rowid": row.rowid,
        }),
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn kiro_capture(
    row: &KiroConversationRow,
    provider_session_id: &str,
    value: &Value,
    started_at: DateTime<Utc>,
    ended_at: Option<DateTime<Utc>>,
    raw_source_path: &str,
    user_version: i64,
    schema_fingerprint: &str,
    event: Option<ProviderEventEnvelope>,
    context: &ProviderAdapterContext,
) -> ProviderCaptureEnvelope {
    native_provider_capture(
        NativeSessionDraft {
            provider: CaptureProvider::KiroCli,
            source_format: KIRO_SQLITE_SOURCE_FORMAT,
            provider_session_id: provider_session_id.to_owned(),
            parent_provider_session_id: None,
            root_provider_session_id: None,
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".to_owned()),
            is_primary: true,
            started_at,
            ended_at,
            cwd: (!row.key.trim().is_empty()).then(|| row.key.clone()),
            fidelity: Fidelity::Imported,
            raw_source_path: raw_source_path.to_owned(),
            trust: ProviderSourceTrust::ProviderNative,
            source_metadata: json!({
                "adapter": KIRO_SQLITE_SOURCE_FORMAT,
                "sqlite_user_version": user_version,
                "schema_fingerprint": schema_fingerprint,
                "source_path": raw_source_path,
                "table": row.table,
            }),
            session_metadata: json!({
                "source_format": KIRO_SQLITE_SOURCE_FORMAT,
                "table": row.table,
                "key": row.key,
                "conversation_id": provider_session_id,
                "created_at": row.created_at,
                "updated_at": row.updated_at,
                "history_len": value
                    .get("history")
                    .and_then(Value::as_array)
                    .map(Vec::len),
                "conversation": provider_capped_json_value(value, PROVIDER_MAX_PREVIEW_CHARS),
            }),
        },
        context,
        event,
    )
}

pub(crate) fn kiro_conversation_rows(conn: &Connection) -> Result<Vec<KiroConversationRow>> {
    let mut rows = Vec::new();
    let mut found_table = false;

    if sqlite_table_exists(conn, "conversations_v2")? {
        found_table = true;
        let columns = sqlite_table_columns(conn, "conversations_v2")?;
        ensure_sqlite_table_columns(
            &columns,
            "Kiro conversations_v2 table",
            &[
                "key",
                "conversation_id",
                "value",
                "created_at",
                "updated_at",
            ],
        )?;
        let mut stmt = conn.prepare(
            "select rowid, key, conversation_id, value, created_at, updated_at \
             from conversations_v2 order by updated_at, key, conversation_id",
        )?;
        let mapped = stmt.query_map([], |row| {
            Ok(KiroConversationRow {
                table: "conversations_v2",
                rowid: row.get(0)?,
                key: row.get(1)?,
                conversation_id: Some(row.get(2)?),
                value: row.get(3)?,
                created_at: row.get(4)?,
                updated_at: row.get(5)?,
            })
        })?;
        rows.extend(
            mapped
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(CaptureError::from)?,
        );
    }

    if sqlite_table_exists(conn, "conversations")? {
        found_table = true;
        let columns = sqlite_table_columns(conn, "conversations")?;
        ensure_sqlite_table_columns(&columns, "Kiro conversations table", &["key", "value"])?;
        let mut stmt = conn.prepare("select rowid, key, value from conversations order by key")?;
        let mapped = stmt.query_map([], |row| {
            Ok(KiroConversationRow {
                table: "conversations",
                rowid: row.get(0)?,
                key: row.get(1)?,
                conversation_id: None,
                value: row.get(2)?,
                created_at: None,
                updated_at: None,
            })
        })?;
        rows.extend(
            mapped
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(CaptureError::from)?,
        );
    }

    if !found_table {
        return Err(CaptureError::InvalidPayload(
            "Kiro SQLite database is missing required conversations_v2 or conversations table"
                .into(),
        ));
    }

    Ok(rows)
}

pub(crate) fn kiro_provider_session_id(row: &KiroConversationRow, value: &Value) -> String {
    row.conversation_id
        .as_deref()
        .or_else(|| value.get("conversation_id").and_then(Value::as_str))
        .filter(|id| !id.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{}:{}:{}", row.table, row.key, row.rowid))
}

pub(crate) fn kiro_session_started_at(
    row: &KiroConversationRow,
    value: &Value,
    fallback: DateTime<Utc>,
) -> DateTime<Utc> {
    value
        .get("history")
        .and_then(Value::as_array)
        .and_then(|history| {
            history
                .iter()
                .map(|entry| kiro_entry_timestamp(entry, "user", fallback))
                .min()
        })
        .unwrap_or_else(|| provider_timestamp_millis(row.created_at, fallback))
}

pub(crate) fn kiro_session_ended_at(
    row: &KiroConversationRow,
    value: &Value,
    fallback: DateTime<Utc>,
) -> DateTime<Utc> {
    value
        .get("history")
        .and_then(Value::as_array)
        .and_then(|history| {
            history
                .iter()
                .flat_map(|entry| {
                    [
                        kiro_entry_timestamp(entry, "user", fallback),
                        kiro_entry_timestamp(entry, "assistant", fallback),
                    ]
                })
                .max()
        })
        .unwrap_or_else(|| provider_timestamp_millis(row.updated_at.or(row.created_at), fallback))
}

pub(crate) fn kiro_entry_timestamp(
    entry: &Value,
    role: &str,
    fallback: DateTime<Utc>,
) -> DateTime<Utc> {
    provider_timestamp_value(
        entry
            .get(role)
            .and_then(|value| value.get("timestamp"))
            .or_else(|| entry.get("timestamp")),
        fallback,
    )
}

pub(crate) fn kiro_user_prompt_text(entry: &Value) -> Option<String> {
    entry
        .pointer("/user/content/Prompt/prompt")
        .and_then(provider_value_text)
        .filter(|text| !text.trim().is_empty())
}

pub(crate) fn kiro_assistant_message(entry: &Value) -> Option<KiroAssistantMessage> {
    if let Some(content) = entry
        .pointer("/assistant/Response/content")
        .and_then(provider_value_text)
        .filter(|text| !text.trim().is_empty())
    {
        return Some(KiroAssistantMessage {
            event_type: EventType::Message,
            text: content,
            tool_uses: None,
        });
    }

    let tool_use = entry.pointer("/assistant/ToolUse")?;
    let tool_uses = tool_use
        .get("tool_uses")
        .or_else(|| tool_use.get("toolUses"))
        .cloned();
    let text = tool_use
        .get("content")
        .and_then(provider_value_text)
        .filter(|text| !text.trim().is_empty())
        .or_else(|| tool_uses.as_ref().and_then(kiro_tool_uses_text))
        .unwrap_or_else(|| "Kiro assistant tool use".to_owned());
    let has_tool_uses = tool_uses
        .as_ref()
        .and_then(Value::as_array)
        .map(|items| !items.is_empty())
        .unwrap_or(false);
    Some(KiroAssistantMessage {
        event_type: if has_tool_uses {
            EventType::ToolCall
        } else {
            EventType::Message
        },
        text,
        tool_uses,
    })
}

pub(crate) fn kiro_tool_uses_text(value: &Value) -> Option<String> {
    let names = value
        .as_array()?
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .filter(|name| !name.trim().is_empty())
        .collect::<Vec<_>>();
    (!names.is_empty()).then(|| format!("tool calls: {}", names.join(", ")))
}
