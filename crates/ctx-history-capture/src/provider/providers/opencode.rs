use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventType, Fidelity, ProviderCaptureEnvelope,
    ProviderCursorCheckpoint, ProviderCursorRange, ProviderEventEnvelope, ProviderRawRetention,
    ProviderRedactionBoundary, ProviderSessionEnvelope, ProviderSourceEnvelope,
    ProviderSourceTrust, RedactionState, SessionStatus, PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
};
use rusqlite::Connection;
use serde_json::{json, Value};

use crate::compute_payload_hash;
use crate::provider::native::{OpenCodeMessageRow, OpenCodeSessionRow};

use crate::provider::custom_history_jsonl::push_provider_import_failure;
use crate::provider::file_touches::provider_file_touches_from_raw_value;
use crate::provider::importer::provider_cursor_stream;
use crate::provider::native::{
    open_provider_sqlite_readonly, parse_json_object_string, provider_capped_json,
    provider_line_from_index, provider_local_preview, provider_nonnegative_i64_to_u64,
    provider_required_timestamp_millis, provider_role, provider_value_text,
};
use crate::provider::providers::real_content::text_has_real_content;
use crate::{
    CaptureError, ProviderAdapterContext, ProviderNormalizationResult, Result,
    KILO_SQLITE_SOURCE_FORMAT, OPENCODE_SQLITE_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS,
    PROVIDER_MAX_TEXT_CHARS,
};

pub(crate) struct OpenCodeSqliteDialect {
    pub(crate) provider: CaptureProvider,
    pub(crate) display_name: &'static str,
    pub(crate) source_format: &'static str,
    pub(crate) session_time_created_field: &'static str,
    pub(crate) session_message_seq_field: &'static str,
    pub(crate) session_message_time_created_field: &'static str,
    pub(crate) event_time_created_field: &'static str,
}

pub(crate) const OPENCODE_SQLITE_DIALECT: OpenCodeSqliteDialect = OpenCodeSqliteDialect {
    provider: CaptureProvider::OpenCode,
    display_name: "OpenCode",
    source_format: OPENCODE_SQLITE_SOURCE_FORMAT,
    session_time_created_field: "OpenCode session time_created",
    session_message_seq_field: "OpenCode session_message seq",
    session_message_time_created_field: "OpenCode session_message time_created",
    event_time_created_field: "OpenCode event time.created",
};

pub(crate) const KILO_SQLITE_DIALECT: OpenCodeSqliteDialect = OpenCodeSqliteDialect {
    provider: CaptureProvider::Kilo,
    display_name: "Kilo",
    source_format: KILO_SQLITE_SOURCE_FORMAT,
    session_time_created_field: "Kilo session time_created",
    session_message_seq_field: "Kilo session_message seq",
    session_message_time_created_field: "Kilo session_message time_created",
    event_time_created_field: "Kilo event time.created",
};

#[derive(Debug, Clone)]
pub(crate) struct OpenCodeMessageSelection {
    pub(crate) rows: Vec<OpenCodeMessageRow>,
    pub(crate) source_table: Option<&'static str>,
    pub(crate) skipped_non_conversational_rows: usize,
}

pub(crate) fn normalize_opencode_sqlite(
    path: &Path,
    context: &ProviderAdapterContext,
    dialect: &OpenCodeSqliteDialect,
) -> Result<ProviderNormalizationResult> {
    let conn = open_provider_sqlite_readonly(path)?;
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let schema_fingerprint = opencode_schema_fingerprint(&conn)?;
    let legacy_message_rows = opencode_count(&conn, "message").unwrap_or(0);
    let legacy_part_rows = opencode_count(&conn, "part").unwrap_or(0);
    let sessions = opencode_sessions(&conn, dialect)?;
    let session_ids = sessions
        .iter()
        .map(|session| session.id.clone())
        .collect::<BTreeSet<_>>();
    let message_selection = opencode_session_messages(&conn, dialect, &session_ids)?;
    let mut result = ProviderNormalizationResult::default();
    if message_selection.rows.is_empty() {
        push_provider_import_failure(
            &mut result.summary,
            0,
            format!(
                "{} SQLite database contained no real conversational message rows",
                dialect.display_name
            ),
        );
        return Ok(result);
    }
    let mut session_started = BTreeMap::new();
    for session in &sessions {
        session_started.insert(
            session.id.clone(),
            provider_required_timestamp_millis(
                session.time_created,
                dialect.session_time_created_field,
            )?,
        );
    }
    let sessions_by_id = sessions
        .into_iter()
        .map(|session| (session.id.clone(), session))
        .collect::<BTreeMap<_, _>>();
    let raw_source_path = path.display().to_string();
    let message_source_table = message_selection.source_table;
    let skipped_non_conversational_rows = message_selection.skipped_non_conversational_rows;

    for row in message_selection.rows {
        let provider_event_index =
            match provider_nonnegative_i64_to_u64(row.seq, dialect.session_message_seq_field) {
                Ok(value) => value,
                Err(err) => {
                    push_provider_import_failure(&mut result.summary, 0, err.to_string());
                    continue;
                }
            };
        let line = provider_line_from_index(provider_event_index);
        let Some(session) = sessions_by_id.get(&row.session_id) else {
            push_provider_import_failure(
                &mut result.summary,
                line,
                format!(
                    "{} session_message {} references missing session {}",
                    dialect.display_name, row.id, row.session_id
                ),
            );
            continue;
        };
        let data: Value = match serde_json::from_str(&row.data) {
            Ok(data) => data,
            Err(err) => {
                push_provider_import_failure(
                    &mut result.summary,
                    line,
                    format!("invalid JSON in session_message {}: {err}", row.id),
                );
                continue;
            }
        };
        let occurred_at = match opencode_event_time(&data, dialect) {
            Ok(Some(time)) => time,
            Ok(None) => match provider_required_timestamp_millis(
                row.time_created,
                dialect.session_message_time_created_field,
            ) {
                Ok(time) => time,
                Err(err) => {
                    push_provider_import_failure(&mut result.summary, line, err.to_string());
                    continue;
                }
            },
            Err(err) => {
                push_provider_import_failure(&mut result.summary, line, err.to_string());
                continue;
            }
        };
        let started_at = session_started
            .get(&session.id)
            .copied()
            .unwrap_or(occurred_at);
        let event = opencode_event(&row, &data, occurred_at, provider_event_index, dialect);
        result
            .files_touched
            .extend(provider_file_touches_from_raw_value(
                dialect.provider,
                &session.id,
                dialect.source_format,
                Some(raw_source_path.as_str()),
                &data,
                &event,
                line,
            ));
        let is_subagent = session.parent_id.is_some();
        result.captures.push((
            line,
            ProviderCaptureEnvelope {
                schema_version: PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
                provider: dialect.provider,
                source: ProviderSourceEnvelope {
                    source_format: dialect.source_format.to_owned(),
                    machine_id: context.machine_id.clone(),
                    observed_at: context.imported_at,
                    raw_source_path: Some(raw_source_path.clone()),
                    raw_retention: ProviderRawRetention::PathReference,
                    redaction_boundary: ProviderRedactionBoundary::BeforeExport,
                    trust: ProviderSourceTrust::ProviderNative,
                    fidelity: Fidelity::Imported,
                    cursor: Some(ProviderCursorRange {
                        before: None,
                        after: Some(ProviderCursorCheckpoint {
                            stream: provider_cursor_stream(
                                dialect.provider,
                                dialect.source_format,
                            ),
                            cursor: format!("session_message:{}:seq:{}", row.session_id, row.seq),
                            observed_at: occurred_at,
                        }),
                    }),
                    idempotency_key: Some(format!(
                        "provider-source:{}:{}:{}",
                        dialect.provider.as_str(),
                        dialect.source_format,
                        session.id
                    )),
                    metadata: json!({
                        "adapter": dialect.source_format,
                        "sqlite_user_version": user_version,
                        "schema_fingerprint": schema_fingerprint,
                        "legacy_message_rows": legacy_message_rows,
                        "legacy_part_rows": legacy_part_rows,
                    }),
                },
                session: ProviderSessionEnvelope {
                    provider_session_id: session.id.clone(),
                    parent_provider_session_id: session.parent_id.clone(),
                    root_provider_session_id: session.parent_id.clone(),
                    external_agent_id: session.agent.clone(),
                    agent_type: if is_subagent {
                        AgentType::Subagent
                    } else {
                        AgentType::Primary
                    },
                    role_hint: session
                        .agent
                        .clone()
                        .or_else(|| Some(if is_subagent { "subagent" } else { "primary" }.to_owned())),
                    is_primary: !is_subagent,
                    status: SessionStatus::Imported,
                    started_at,
                    ended_at: None,
                    cwd: Some(session.directory.clone()),
                    fidelity: Fidelity::Imported,
                    idempotency_key: Some(format!(
                        "provider-session:{}:{}",
                        dialect.provider.as_str(),
                        session.id
                    )),
                    artifacts: Vec::new(),
                    metadata: json!({
                        "source_format": dialect.source_format,
                        "title": session.title,
                        "model": parse_json_object_string(session.model.as_deref()),
                        "agent": session.agent,
                        "time_updated": session.time_updated,
                        "tokens": {
                            "input": session.tokens_input,
                            "output": session.tokens_output,
                            "reasoning": session.tokens_reasoning,
                            "cache_read": session.tokens_cache_read,
                            "cache_write": session.tokens_cache_write,
                        },
                        "legacy_projection": {
                            "message_rows": legacy_message_rows,
                            "part_rows": legacy_part_rows,
                            "selected_message_table": message_source_table,
                            "skipped_non_conversational_rows": skipped_non_conversational_rows,
                            "import_policy": "session_message/session_entry are authoritative only when they contain real conversational content; otherwise legacy message rows may be used"
                        },
                    }),
                },
                event: Some(event),
            },
        ));
    }

    Ok(result)
}

pub(crate) fn opencode_sessions(
    conn: &Connection,
    dialect: &OpenCodeSqliteDialect,
) -> Result<Vec<OpenCodeSessionRow>> {
    if !sqlite_table_exists(conn, "session")? {
        return Err(CaptureError::InvalidPayload(
            format!(
                "{} SQLite database is missing required session table",
                dialect.display_name
            )
            .into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "session")?;
    ensure_sqlite_table_columns(
        &columns,
        &format!("{} SQLite session table", dialect.display_name),
        &["id"],
    )?;
    let parent_id = optional_column_expr(&columns, "parent_id", "NULL");
    let title = optional_column_expr(
        &columns,
        "title",
        optional_column_expr(&columns, "slug", "id"),
    );
    let directory = optional_column_expr(&columns, "directory", "''");
    let model = optional_column_expr(&columns, "model", "NULL");
    let agent = optional_column_expr(&columns, "agent", "NULL");
    let time_created = optional_column_expr(&columns, "time_created", "0");
    let time_updated = optional_column_expr(&columns, "time_updated", time_created);
    let tokens_input = optional_column_expr(&columns, "tokens_input", "0");
    let tokens_output = optional_column_expr(&columns, "tokens_output", "0");
    let tokens_reasoning = optional_column_expr(&columns, "tokens_reasoning", "0");
    let tokens_cache_read = optional_column_expr(&columns, "tokens_cache_read", "0");
    let tokens_cache_write = optional_column_expr(&columns, "tokens_cache_write", "0");
    let order_by = if columns.contains("time_created") {
        "time_created, id"
    } else {
        "id"
    };
    let sql = format!(
        "select id, {parent_id}, {title}, {directory}, {model}, {agent}, {time_created}, \
         {time_updated}, {tokens_input}, {tokens_output}, {tokens_reasoning}, \
         {tokens_cache_read}, {tokens_cache_write} from session order by {order_by}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok(OpenCodeSessionRow {
            id: row.get(0)?,
            parent_id: row.get(1)?,
            title: row.get(2)?,
            directory: row.get(3)?,
            model: row.get(4)?,
            agent: row.get(5)?,
            time_created: row.get(6)?,
            time_updated: row.get(7)?,
            tokens_input: row.get(8)?,
            tokens_output: row.get(9)?,
            tokens_reasoning: row.get(10)?,
            tokens_cache_read: row.get(11)?,
            tokens_cache_write: row.get(12)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CaptureError::from)
}

pub(crate) fn opencode_session_messages(
    conn: &Connection,
    dialect: &OpenCodeSqliteDialect,
    session_ids: &BTreeSet<String>,
) -> Result<OpenCodeMessageSelection> {
    let mut skipped_non_conversational_rows = 0usize;
    if sqlite_table_exists(conn, "session_message")? {
        let rows = opencode_session_message_rows(conn, dialect)?;
        if opencode_rows_have_import_blocking_errors(&rows, session_ids, dialect) {
            return Ok(OpenCodeMessageSelection {
                rows,
                source_table: Some("session_message"),
                skipped_non_conversational_rows,
            });
        }
        if opencode_rows_have_real_message_content(&rows) {
            return Ok(OpenCodeMessageSelection {
                rows,
                source_table: Some("session_message"),
                skipped_non_conversational_rows,
            });
        }
        skipped_non_conversational_rows += rows.len();
    }
    if sqlite_table_exists(conn, "session_entry")? {
        let rows = opencode_session_entry_rows(conn, dialect)?;
        if opencode_rows_have_import_blocking_errors(&rows, session_ids, dialect) {
            return Ok(OpenCodeMessageSelection {
                rows,
                source_table: Some("session_entry"),
                skipped_non_conversational_rows,
            });
        }
        if opencode_rows_have_real_message_content(&rows) {
            return Ok(OpenCodeMessageSelection {
                rows,
                source_table: Some("session_entry"),
                skipped_non_conversational_rows,
            });
        }
        skipped_non_conversational_rows += rows.len();
    }
    if sqlite_table_exists(conn, "message")? {
        let rows = opencode_message_rows(conn, dialect)?;
        if opencode_rows_have_import_blocking_errors(&rows, session_ids, dialect) {
            return Ok(OpenCodeMessageSelection {
                rows,
                source_table: Some("message"),
                skipped_non_conversational_rows,
            });
        }
        if opencode_rows_have_real_message_content(&rows) {
            return Ok(OpenCodeMessageSelection {
                rows,
                source_table: Some("message"),
                skipped_non_conversational_rows,
            });
        }
        skipped_non_conversational_rows += rows.len();
    }
    Ok(OpenCodeMessageSelection {
        rows: Vec::new(),
        source_table: None,
        skipped_non_conversational_rows,
    })
}

pub(crate) fn opencode_session_message_rows(
    conn: &Connection,
    dialect: &OpenCodeSqliteDialect,
) -> Result<Vec<OpenCodeMessageRow>> {
    let columns = sqlite_table_columns(conn, "session_message")?;
    ensure_sqlite_table_columns(
        &columns,
        &format!("{} SQLite session_message table", dialect.display_name),
        &["id", "session_id", "data"],
    )?;
    let entry_type = optional_column_expr(&columns, "type", "'message'");
    let time_created = optional_column_expr(&columns, "time_created", "0");
    let time_updated = optional_column_expr(&columns, "time_updated", time_created);
    let (seq_expr, order_expr) = if columns.contains("seq") {
        ("seq", "seq, id")
    } else if columns.contains("time_created") {
        ("NULL", "time_created, id")
    } else {
        ("NULL", "id")
    };
    let sql = format!(
        "select id, session_id, {entry_type}, {seq_expr}, {time_created}, {time_updated}, data \
         from session_message order by session_id, {order_expr}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<i64>>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, String>(6)?,
        ))
    })?;
    let rows = rows
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CaptureError::from)?;
    let mut messages = Vec::new();
    let mut next_seq_by_session = BTreeMap::<String, i64>::new();
    for (id, session_id, entry_type, seq, time_created, time_updated, data) in rows {
        let seq = seq.unwrap_or_else(|| next_opencode_seq(&mut next_seq_by_session, &session_id));
        let entry_type = opencode_entry_type_from_data(&entry_type, &data);
        messages.push(OpenCodeMessageRow {
            id,
            session_id,
            entry_type,
            seq,
            time_created,
            time_updated,
            data,
        });
    }
    Ok(messages)
}

pub(crate) fn opencode_entry_type_from_data(fallback: &str, data: &str) -> String {
    if !fallback.trim().is_empty() && fallback != "message" {
        return fallback.to_owned();
    }
    serde_json::from_str::<Value>(data)
        .ok()
        .and_then(|value| opencode_message_type_from_data(&value))
        .unwrap_or_else(|| fallback.to_owned())
}

pub(crate) fn opencode_session_entry_rows(
    conn: &Connection,
    dialect: &OpenCodeSqliteDialect,
) -> Result<Vec<OpenCodeMessageRow>> {
    let columns = sqlite_table_columns(conn, "session_entry")?;
    ensure_sqlite_table_columns(
        &columns,
        &format!("{} SQLite session_entry table", dialect.display_name),
        &[
            "id",
            "session_id",
            "type",
            "time_created",
            "time_updated",
            "data",
        ],
    )?;
    let mut stmt = conn.prepare(
        "select id, session_id, type, time_created, time_updated, data \
         from session_entry order by session_id, time_created, id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, String>(5)?,
        ))
    })?;
    let mut messages = Vec::new();
    let mut next_seq_by_session = BTreeMap::<String, i64>::new();
    for row in rows {
        let (id, session_id, entry_type, time_created, time_updated, data) = row?;
        let seq = next_opencode_seq(&mut next_seq_by_session, &session_id);
        messages.push(OpenCodeMessageRow {
            id,
            session_id,
            entry_type,
            seq,
            time_created,
            time_updated,
            data,
        });
    }
    Ok(messages)
}

pub(crate) fn opencode_message_rows(
    conn: &Connection,
    dialect: &OpenCodeSqliteDialect,
) -> Result<Vec<OpenCodeMessageRow>> {
    let columns = sqlite_table_columns(conn, "message")?;
    ensure_sqlite_table_columns(
        &columns,
        &format!("{} SQLite message table", dialect.display_name),
        &["id", "session_id", "time_created", "time_updated", "data"],
    )?;
    let mut stmt = conn.prepare(
        "select id, session_id, time_created, time_updated, data \
         from message order by session_id, time_created, id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    let mut messages = Vec::new();
    let mut next_seq_by_session = BTreeMap::<String, i64>::new();
    for row in rows {
        let (id, session_id, time_created, time_updated, data) = row?;
        let seq = next_opencode_seq(&mut next_seq_by_session, &session_id);
        let entry_type = serde_json::from_str::<Value>(&data)
            .ok()
            .and_then(|value| opencode_message_type_from_data(&value))
            .unwrap_or_else(|| "message".to_owned());
        messages.push(OpenCodeMessageRow {
            id,
            session_id,
            entry_type,
            seq,
            time_created,
            time_updated,
            data,
        });
    }
    Ok(messages)
}

pub(crate) fn next_opencode_seq(
    next_seq_by_session: &mut BTreeMap<String, i64>,
    session_id: &str,
) -> i64 {
    let entry = next_seq_by_session
        .entry(session_id.to_owned())
        .and_modify(|seq| *seq += 1)
        .or_insert(1);
    *entry
}

pub(crate) fn opencode_message_type_from_data(data: &Value) -> Option<String> {
    data.get("role")
        .or_else(|| data.get("type"))
        .or_else(|| data.pointer("/message/role"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

pub(crate) fn opencode_rows_have_real_message_content(rows: &[OpenCodeMessageRow]) -> bool {
    rows.iter().any(opencode_message_row_has_real_content)
}

pub(crate) fn opencode_rows_have_import_blocking_errors(
    rows: &[OpenCodeMessageRow],
    session_ids: &BTreeSet<String>,
    dialect: &OpenCodeSqliteDialect,
) -> bool {
    rows.iter().any(|row| {
        provider_nonnegative_i64_to_u64(row.seq, dialect.session_message_seq_field).is_err()
            || !session_ids.contains(&row.session_id)
            || opencode_row_has_invalid_time_or_json(row, dialect)
    })
}

pub(crate) fn opencode_row_has_invalid_time_or_json(
    row: &OpenCodeMessageRow,
    dialect: &OpenCodeSqliteDialect,
) -> bool {
    let Ok(data) = serde_json::from_str::<Value>(&row.data) else {
        return true;
    };
    match opencode_event_time(&data, dialect) {
        Ok(Some(_)) => false,
        Ok(None) => provider_required_timestamp_millis(
            row.time_created,
            dialect.session_message_time_created_field,
        )
        .is_err(),
        Err(_) => true,
    }
}

pub(crate) fn opencode_message_row_has_real_content(row: &OpenCodeMessageRow) -> bool {
    let Ok(data) = serde_json::from_str::<Value>(&row.data) else {
        return false;
    };
    opencode_data_has_real_message_content(&row.entry_type, &data)
}

pub(crate) fn opencode_data_has_real_message_content(entry_type: &str, data: &Value) -> bool {
    if !matches!(entry_type, "assistant" | "user" | "system") {
        return false;
    }
    ["text", "content", "message"]
        .into_iter()
        .any(|key| data.get(key).is_some_and(opencode_value_has_real_content))
}

pub(crate) fn opencode_value_has_real_content(value: &Value) -> bool {
    match value {
        Value::String(text) => text_has_real_content(Some(text)),
        Value::Array(values) => values.iter().any(opencode_value_has_real_content),
        Value::Object(object) => {
            if object
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| {
                    matches!(
                        kind,
                        "tool"
                            | "tool_use"
                            | "toolCall"
                            | "function_call"
                            | "agent"
                            | "tool_result"
                    )
                })
            {
                return false;
            }
            [
                "text", "content", "output", "summary", "thinking", "command",
            ]
            .into_iter()
            .any(|key| object.get(key).is_some_and(opencode_value_has_real_content))
        }
        Value::Number(_) | Value::Bool(_) | Value::Null => false,
    }
}

pub(crate) fn sqlite_table_exists(conn: &Connection, table: &str) -> Result<bool> {
    let exists: i64 = conn.query_row(
        "select count(*) from sqlite_schema where type = 'table' and name = ?1",
        [table],
        |row| row.get(0),
    )?;
    Ok(exists > 0)
}

pub(crate) fn sqlite_table_columns(conn: &Connection, table: &str) -> Result<BTreeSet<String>> {
    let mut stmt = conn.prepare(&format!("pragma table_info({})", sqlite_ident(table)))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    rows.collect::<std::result::Result<BTreeSet<_>, _>>()
        .map_err(CaptureError::from)
}

pub(crate) fn optional_column_expr<'a>(
    columns: &BTreeSet<String>,
    column: &'a str,
    fallback: &'a str,
) -> &'a str {
    if columns.contains(column) {
        column
    } else {
        fallback
    }
}

pub(crate) fn ensure_sqlite_table_columns(
    columns: &BTreeSet<String>,
    label: &str,
    required: &[&str],
) -> Result<()> {
    let missing = required
        .iter()
        .copied()
        .filter(|column| !columns.contains(*column))
        .collect::<Vec<_>>();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(CaptureError::InvalidPayload(format!(
            "{label} missing required column(s): {}",
            missing.join(", ")
        )))
    }
}

pub(crate) fn sqlite_ident(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

pub(crate) fn opencode_schema_fingerprint(conn: &Connection) -> Result<String> {
    let mut stmt = conn.prepare(
        "select name, sql from sqlite_schema where type in ('table','index') order by name",
    )?;
    let rows = stmt.query_map([], |row| {
        let name: String = row.get(0)?;
        let sql: Option<String> = row.get(1)?;
        Ok(format!("{name}:{}", sql.unwrap_or_default()))
    })?;
    let schema = rows.collect::<std::result::Result<Vec<_>, _>>()?.join("\n");
    compute_payload_hash(&json!({ "schema": schema }))
}

pub(crate) fn opencode_count(conn: &Connection, table: &str) -> rusqlite::Result<i64> {
    conn.query_row(&format!("select count(*) from {table}"), [], |row| {
        row.get(0)
    })
}

pub(crate) fn opencode_event(
    row: &OpenCodeMessageRow,
    data: &Value,
    occurred_at: DateTime<Utc>,
    provider_event_index: u64,
    dialect: &OpenCodeSqliteDialect,
) -> ProviderEventEnvelope {
    let event_type = opencode_event_type(&row.entry_type, data);
    let role = Some(provider_role(Some(&row.entry_type)));
    let text = opencode_event_text(&row.entry_type, data, event_type, dialect);
    let (text, truncated) = provider_local_preview(&text, PROVIDER_MAX_TEXT_CHARS);
    ProviderEventEnvelope {
        provider_event_index,
        provider_event_hash: Some(row.id.clone()),
        cursor: Some(format!(
            "session_message:{}:seq:{}",
            row.session_id, row.seq
        )),
        event_type,
        role,
        occurred_at,
        fidelity: Fidelity::Imported,
        redaction_state: RedactionState::LocalPreview,
        idempotency_key: Some(format!(
            "provider-event:{}:{}:{}",
            dialect.provider.as_str(),
            row.session_id,
            row.id
        )),
        artifacts: Vec::new(),
        payload: json!({
            "entry_type": row.entry_type,
            "message_id": row.id,
            "session_message_seq": row.seq,
            "text": text,
            "truncated": truncated,
            "body": provider_capped_json(data, PROVIDER_MAX_PREVIEW_CHARS),
        }),
        metadata: json!({
            "source": dialect.source_format,
            "source_format": dialect.source_format,
            "session_message_id": row.id,
            "session_message_seq": row.seq,
            "time_created": row.time_created,
            "time_updated": row.time_updated,
            "model": data.get("model").cloned(),
            "tokens": data.get("tokens").cloned(),
            "cost": data.get("cost").cloned(),
            "finish": data.get("finish").cloned(),
            "error": data.get("error").cloned(),
        }),
    }
}

pub(crate) fn opencode_event_type(entry_type: &str, data: &Value) -> EventType {
    match entry_type {
        "assistant" if opencode_content_has_tool(data) => EventType::ToolCall,
        "assistant" | "user" | "system" => EventType::Message,
        "shell" => EventType::CommandOutput,
        _ => EventType::Notice,
    }
}

pub(crate) fn opencode_event_text(
    entry_type: &str,
    data: &Value,
    event_type: EventType,
    dialect: &OpenCodeSqliteDialect,
) -> String {
    if let Some(text) = data.get("text").and_then(Value::as_str) {
        return text.to_owned();
    }
    if entry_type == "shell" {
        let command = data
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or("shell");
        let output = data.get("output").and_then(Value::as_str).unwrap_or("");
        return format!("{command}\n{output}");
    }
    if let Some(content) = data.get("content") {
        if let Some(text) = provider_value_text(content) {
            return text;
        }
    }
    if event_type == EventType::Notice {
        format!("{} event: {entry_type}", dialect.display_name)
    } else {
        serde_json::to_string(data).unwrap_or_else(|_| entry_type.to_owned())
    }
}

pub(crate) fn opencode_content_has_tool(data: &Value) -> bool {
    data.get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks.iter().any(|block| {
                matches!(
                    block.get("type").and_then(Value::as_str),
                    Some("tool" | "tool_use" | "toolCall")
                )
            })
        })
        .unwrap_or(false)
}

pub(crate) fn opencode_event_time(
    data: &Value,
    dialect: &OpenCodeSqliteDialect,
) -> Result<Option<DateTime<Utc>>> {
    let Some(value) = data.pointer("/time/created") else {
        return Ok(None);
    };
    let millis = value.as_i64().ok_or_else(|| {
        CaptureError::InvalidPayload(format!(
            "{} event time.created must be integer millis",
            dialect.display_name
        ))
    })?;
    provider_required_timestamp_millis(millis, dialect.event_time_created_field).map(Some)
}
