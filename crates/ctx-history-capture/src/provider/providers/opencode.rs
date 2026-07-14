use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, Confidence, EventType, Fidelity, FileChangeKind,
    ProviderCaptureEnvelope, ProviderCursorCheckpoint, ProviderCursorRange, ProviderEventEnvelope,
    ProviderSessionEnvelope, ProviderSourceEnvelope, ProviderSourceTrust, SessionStatus,
    PROVIDER_CAPTURE_ENVELOPE_SCHEMA_VERSION,
};
use rusqlite::Connection;
use serde_json::{json, Value};

use crate::provider::native::{OpenCodeMessageRow, OpenCodeSessionRow};
use crate::{compute_payload_hash, fnv1a64};

use crate::provider::custom_history_jsonl::push_provider_import_failure;
use crate::provider::file_touches::{
    normalize_file_path, provider_file_touch_envelopes, provider_file_touches_from_raw_value,
    FileTouchDraft, ProviderFileTouchEnvelopeContext,
};
use crate::provider::importer::provider_cursor_stream;
use crate::provider::native::{
    open_provider_sqlite_readonly, parse_json_object_string, provider_capped_json,
    provider_line_from_index, provider_nonnegative_i64_to_u64, provider_policy_body,
    provider_policy_event_text, provider_required_timestamp_millis, provider_role,
    provider_value_text,
};
use crate::provider::providers::real_content::text_has_real_content;
use crate::provider::sqlite::{sqlite_is_too_big, sqlite_row_ids_with_oversized_value};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderNormalizationResult, Result,
    KILO_SQLITE_SOURCE_FORMAT, MIMOCODE_SQLITE_SOURCE_FORMAT, OPENCODE_SQLITE_SOURCE_FORMAT,
    PROVIDER_MAX_PREVIEW_CHARS,
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

pub(crate) const MIMOCODE_SQLITE_DIALECT: OpenCodeSqliteDialect = OpenCodeSqliteDialect {
    provider: CaptureProvider::MiMoCode,
    display_name: "MiMo Code",
    source_format: MIMOCODE_SQLITE_SOURCE_FORMAT,
    session_time_created_field: "MiMo Code session time_created",
    session_message_seq_field: "MiMo Code session_message seq",
    session_message_time_created_field: "MiMo Code session_message time_created",
    event_time_created_field: "MiMo Code event time.created",
};

#[derive(Debug, Clone)]
pub(crate) struct OpenCodeMessageSelection {
    pub(crate) rows: Vec<OpenCodeMessageRow>,
    pub(crate) source_table: Option<&'static str>,
    pub(crate) skipped_non_conversational_rows: usize,
    pub(crate) skipped_oversized_values: usize,
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
    let message_selection = opencode_session_messages(path, &conn, dialect, &session_ids)?;
    let mut result = ProviderNormalizationResult::default();
    result.summary.skipped += message_selection.skipped_oversized_values;
    result.summary.skipped_events += message_selection.skipped_oversized_values;
    if message_selection.rows.is_empty() {
        if message_selection.skipped_oversized_values == 0
            || message_selection.skipped_non_conversational_rows > 0
        {
            push_provider_import_failure(
                &mut result.summary,
                0,
                format!(
                    "{} SQLite database contained no real conversational message rows",
                    dialect.display_name
                ),
            );
        }
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
        if data.get("source_table").and_then(Value::as_str) == Some("message+part") {
            result.files_touched.extend(provider_file_touch_envelopes(
                ProviderFileTouchEnvelopeContext {
                    provider: dialect.provider,
                    provider_session_id: &session.id,
                    source_format: dialect.source_format,
                    raw_source_path: Some(raw_source_path.as_str()),
                    source_root: Some(raw_source_path.as_str()),
                    occurred_at,
                    provider_event_index: Some(provider_event_index),
                    provider_touch_base_index: provider_event_index << 16,
                    line_number: line,
                },
                opencode_message_part_file_touch_drafts(&data),
            ));
        }
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
                    source_root: context
                        .source_root_display()
                        .or_else(|| Some(raw_source_path.clone())),
                    trust: ProviderSourceTrust::ProviderNative,
                    fidelity: Fidelity::Imported,
                    cursor: Some(ProviderCursorRange {
                        before: None,
                        after: Some(ProviderCursorCheckpoint {
                            stream: provider_cursor_stream(
                                dialect.provider,
                                dialect.source_format,
                            ),
                            cursor: opencode_event_cursor(&row, &data),
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
        return Err(CaptureError::InvalidPayload(format!(
            "{} SQLite database is missing required session table",
            dialect.display_name
        )));
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
    path: &Path,
    conn: &Connection,
    dialect: &OpenCodeSqliteDialect,
    session_ids: &BTreeSet<String>,
) -> Result<OpenCodeMessageSelection> {
    let mut skipped_non_conversational_rows = 0usize;
    let mut skipped_oversized_values = 0usize;
    if sqlite_table_exists(conn, "session_message")? {
        let (rows, oversized) = opencode_session_message_rows(path, conn, dialect)?;
        skipped_oversized_values += oversized;
        if opencode_rows_have_import_blocking_errors(&rows, session_ids, dialect) {
            return Ok(OpenCodeMessageSelection {
                rows,
                source_table: Some("session_message"),
                skipped_non_conversational_rows,
                skipped_oversized_values,
            });
        }
        if opencode_rows_have_real_message_content(&rows) {
            return Ok(OpenCodeMessageSelection {
                rows,
                source_table: Some("session_message"),
                skipped_non_conversational_rows,
                skipped_oversized_values,
            });
        }
        skipped_non_conversational_rows += rows.len();
    }
    if sqlite_table_exists(conn, "session_entry")? {
        let (rows, oversized) = opencode_session_entry_rows(path, conn, dialect)?;
        skipped_oversized_values += oversized;
        if opencode_rows_have_import_blocking_errors(&rows, session_ids, dialect) {
            return Ok(OpenCodeMessageSelection {
                rows,
                source_table: Some("session_entry"),
                skipped_non_conversational_rows,
                skipped_oversized_values,
            });
        }
        if opencode_rows_have_real_message_content(&rows) {
            return Ok(OpenCodeMessageSelection {
                rows,
                source_table: Some("session_entry"),
                skipped_non_conversational_rows,
                skipped_oversized_values,
            });
        }
        skipped_non_conversational_rows += rows.len();
    }
    if sqlite_table_exists(conn, "message")? {
        let (rows, oversized) = opencode_message_rows(path, conn, dialect)?;
        skipped_oversized_values += oversized;
        if opencode_rows_have_import_blocking_errors(&rows, session_ids, dialect) {
            return Ok(OpenCodeMessageSelection {
                rows,
                source_table: Some("message"),
                skipped_non_conversational_rows,
                skipped_oversized_values,
            });
        }
        if opencode_rows_have_real_message_content(&rows) {
            return Ok(OpenCodeMessageSelection {
                rows,
                source_table: Some("message"),
                skipped_non_conversational_rows,
                skipped_oversized_values,
            });
        }
        skipped_non_conversational_rows += rows.len();
        if sqlite_table_exists(conn, "part")? {
            let (rows, oversized) = opencode_message_part_rows(path, conn, dialect)?;
            skipped_oversized_values += oversized;
            if opencode_rows_have_import_blocking_errors(&rows, session_ids, dialect) {
                return Ok(OpenCodeMessageSelection {
                    rows,
                    source_table: Some("message+part"),
                    skipped_non_conversational_rows,
                    skipped_oversized_values,
                });
            }
            if opencode_rows_have_real_message_content(&rows) {
                return Ok(OpenCodeMessageSelection {
                    rows,
                    source_table: Some("message+part"),
                    skipped_non_conversational_rows,
                    skipped_oversized_values,
                });
            }
            skipped_non_conversational_rows += rows.len();
        }
    }
    Ok(OpenCodeMessageSelection {
        rows: Vec::new(),
        source_table: None,
        skipped_non_conversational_rows,
        skipped_oversized_values,
    })
}

fn exclude_ids_clause(ids: &BTreeSet<String>) -> String {
    if ids.is_empty() {
        String::new()
    } else {
        format!("where id not in ({})", placeholders(ids.len()))
    }
}

fn oversized_sqlite_data_row_ids(path: &Path, table: &str) -> Result<BTreeSet<String>> {
    sqlite_row_ids_with_oversized_value(path, table, "id", "data")
}

pub(crate) fn opencode_session_message_rows(
    path: &Path,
    conn: &Connection,
    dialect: &OpenCodeSqliteDialect,
) -> Result<(Vec<OpenCodeMessageRow>, usize)> {
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
    let oversized_ids = oversized_sqlite_data_row_ids(path, "session_message")?;
    let mut skipped_oversized = oversized_ids.len();
    let exclude_clause = exclude_ids_clause(&oversized_ids);
    let sql = format!(
        "select id, session_id, {entry_type}, {seq_expr}, {time_created}, {time_updated}, data \
         from session_message \
         {exclude_clause} \
         order by session_id, {order_expr}",
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(rusqlite::params_from_iter(oversized_ids.iter()))?;
    let mut messages = Vec::new();
    let mut next_seq_by_session = BTreeMap::<String, i64>::new();
    while let Some(row) = rows.next()? {
        let id = row.get::<_, String>(0)?;
        let data: String = match row.get::<_, String>(6) {
            Ok(value) => value,
            Err(err) if sqlite_is_too_big(&err) => {
                skipped_oversized += 1;
                continue;
            }
            Err(err) => return Err(CaptureError::from(err)),
        };
        let session_id = row.get::<_, String>(1)?;
        let entry_type_raw = row.get::<_, String>(2)?;
        let seq = row.get::<_, Option<i64>>(3)?;
        let time_created = row.get::<_, i64>(4)?;
        let time_updated = row.get::<_, i64>(5)?;
        let seq = seq.unwrap_or_else(|| next_opencode_seq(&mut next_seq_by_session, &session_id));
        let entry_type = opencode_entry_type_from_data(&entry_type_raw, &data);
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
    Ok((messages, skipped_oversized))
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
    path: &Path,
    conn: &Connection,
    dialect: &OpenCodeSqliteDialect,
) -> Result<(Vec<OpenCodeMessageRow>, usize)> {
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
    let oversized_ids = oversized_sqlite_data_row_ids(path, "session_entry")?;
    let mut skipped_oversized = oversized_ids.len();
    let exclude_clause = exclude_ids_clause(&oversized_ids);
    let sql = format!(
        "select id, session_id, type, time_created, time_updated, data \
         from session_entry \
         {exclude_clause} \
         order by session_id, time_created, id",
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(rusqlite::params_from_iter(oversized_ids.iter()))?;
    let mut messages = Vec::new();
    let mut next_seq_by_session = BTreeMap::<String, i64>::new();
    while let Some(row) = rows.next()? {
        let id = row.get::<_, String>(0)?;
        let data: String = match row.get::<_, String>(5) {
            Ok(value) => value,
            Err(err) if sqlite_is_too_big(&err) => {
                skipped_oversized += 1;
                continue;
            }
            Err(err) => return Err(CaptureError::from(err)),
        };
        let session_id = row.get::<_, String>(1)?;
        let entry_type = row.get::<_, String>(2)?;
        let time_created = row.get::<_, i64>(3)?;
        let time_updated = row.get::<_, i64>(4)?;
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
    Ok((messages, skipped_oversized))
}

pub(crate) fn opencode_message_rows(
    path: &Path,
    conn: &Connection,
    dialect: &OpenCodeSqliteDialect,
) -> Result<(Vec<OpenCodeMessageRow>, usize)> {
    let columns = sqlite_table_columns(conn, "message")?;
    ensure_sqlite_table_columns(
        &columns,
        &format!("{} SQLite message table", dialect.display_name),
        &["id", "session_id", "time_created", "time_updated", "data"],
    )?;
    let oversized_ids = oversized_sqlite_data_row_ids(path, "message")?;
    let mut skipped_oversized = oversized_ids.len();
    let exclude_clause = exclude_ids_clause(&oversized_ids);
    let sql = format!(
        "select id, session_id, time_created, time_updated, data \
         from message \
         {exclude_clause} \
         order by session_id, time_created, id",
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(rusqlite::params_from_iter(oversized_ids.iter()))?;
    let mut messages = Vec::new();
    let mut next_seq_by_session = BTreeMap::<String, i64>::new();
    while let Some(row) = rows.next()? {
        let id = row.get::<_, String>(0)?;
        let data: String = match row.get::<_, String>(4) {
            Ok(value) => value,
            Err(err) if sqlite_is_too_big(&err) => {
                skipped_oversized += 1;
                continue;
            }
            Err(err) => return Err(CaptureError::from(err)),
        };
        let session_id = row.get::<_, String>(1)?;
        let time_created = row.get::<_, i64>(2)?;
        let time_updated = row.get::<_, i64>(3)?;
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
    Ok((messages, skipped_oversized))
}

#[derive(Debug, Clone)]
struct OpenCodePartRow {
    message_id: String,
    session_id: String,
    role: String,
    part_id: String,
    part_type: String,
    seq: i64,
    time_created: i64,
    time_updated: i64,
    data: Value,
    invalid_json: bool,
}

pub(crate) fn opencode_message_part_rows(
    path: &Path,
    conn: &Connection,
    dialect: &OpenCodeSqliteDialect,
) -> Result<(Vec<OpenCodeMessageRow>, usize)> {
    let message_columns = sqlite_table_columns(conn, "message")?;
    ensure_sqlite_table_columns(
        &message_columns,
        &format!("{} SQLite message table", dialect.display_name),
        &["id", "session_id", "time_created", "time_updated", "data"],
    )?;
    let part_columns = sqlite_table_columns(conn, "part")?;
    ensure_sqlite_table_columns(
        &part_columns,
        &format!("{} SQLite part table", dialect.display_name),
        &[
            "id",
            "message_id",
            "session_id",
            "time_created",
            "time_updated",
            "data",
        ],
    )?;
    let oversized_messages = oversized_sqlite_data_row_ids(path, "message")?;
    let oversized_parts = oversized_sqlite_data_row_ids(path, "part")?;
    let mut skipped_oversized = oversized_parts.len();
    let part_type = optional_column_expr(&part_columns, "type", "NULL");
    let mut filters = Vec::new();
    if !oversized_messages.is_empty() {
        filters.push(format!(
            "m.id not in ({})",
            placeholders(oversized_messages.len())
        ));
    }
    if !oversized_parts.is_empty() {
        filters.push(format!(
            "p.id not in ({})",
            placeholders(oversized_parts.len())
        ));
    }
    let where_clause = if filters.is_empty() {
        String::new()
    } else {
        format!("where {}", filters.join(" and "))
    };
    let sql = format!(
        "select m.id, m.session_id, m.data, p.id, p.session_id, {part_type}, \
         p.time_created, p.time_updated, p.data \
         from message m join part p on p.message_id = m.id \
         {where_clause} \
         order by m.session_id, p.time_created, p.id",
    );
    let params = oversized_messages.iter().chain(oversized_parts.iter());
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(rusqlite::params_from_iter(params))?;
    let mut parts = Vec::new();
    while let Some(row) = rows.next()? {
        let message_id: String = row.get(0)?;
        let message_session_id: String = row.get(1)?;
        let part_id: String = row.get(3)?;
        let part_session_id: String = row.get(4)?;
        let session_id = if part_session_id.trim().is_empty() {
            message_session_id
        } else {
            part_session_id
        };
        let time_created = row.get(6)?;
        let time_updated = row.get(7)?;
        let seq = opencode_message_part_identity_index(&message_id, &part_id);
        let message_data: String = match row.get(2) {
            Ok(value) => value,
            Err(err) if sqlite_is_too_big(&err) => {
                skipped_oversized += 1;
                continue;
            }
            Err(err) => return Err(CaptureError::from(err)),
        };
        let part_data: String = match row.get(8) {
            Ok(value) => value,
            Err(err) if sqlite_is_too_big(&err) => {
                skipped_oversized += 1;
                continue;
            }
            Err(err) => return Err(CaptureError::from(err)),
        };
        let Ok(message_data) = serde_json::from_str::<Value>(&message_data) else {
            parts.push(OpenCodePartRow {
                message_id,
                session_id,
                role: "message".to_owned(),
                part_id,
                part_type: "part".to_owned(),
                seq,
                time_created,
                time_updated,
                data: Value::String(message_data),
                invalid_json: true,
            });
            continue;
        };
        let Ok(part_data) = serde_json::from_str::<Value>(&part_data) else {
            parts.push(OpenCodePartRow {
                message_id,
                session_id,
                role: opencode_message_type_from_data(&message_data)
                    .unwrap_or_else(|| "message".to_owned()),
                part_id,
                part_type: "part".to_owned(),
                seq,
                time_created,
                time_updated,
                data: Value::String(part_data),
                invalid_json: true,
            });
            continue;
        };
        let role =
            opencode_message_type_from_data(&message_data).unwrap_or_else(|| "message".to_owned());
        let part_type = opencode_part_type(row.get::<_, Option<String>>(5)?.as_deref(), &part_data);
        parts.push(OpenCodePartRow {
            message_id,
            session_id,
            role,
            part_id,
            part_type,
            seq,
            time_created,
            time_updated,
            data: part_data,
            invalid_json: false,
        });
    }

    let mut file_touches_by_message = BTreeMap::<String, Vec<Value>>::new();
    for part in &parts {
        let file_touches = opencode_part_file_touch_metadata(part);
        if !file_touches.is_empty() {
            file_touches_by_message
                .entry(part.message_id.clone())
                .or_default()
                .extend(file_touches);
        }
    }

    let mut out = Vec::new();
    let mut file_touches_emitted_by_message = BTreeSet::<String>::new();
    for part in parts {
        if part.invalid_json {
            out.push(OpenCodeMessageRow {
                id: format!("{}:{}", part.message_id, part.part_id),
                session_id: part.session_id,
                entry_type: part.role,
                seq: part.seq,
                time_created: part.time_created,
                time_updated: part.time_updated,
                data: part.data.as_str().unwrap_or_default().to_owned(),
            });
            continue;
        }
        let Some(text) = opencode_text_part_text(&part) else {
            if let Some(data) = opencode_tool_part_event_data(&part) {
                out.push(OpenCodeMessageRow {
                    id: format!("{}:{}", part.message_id, part.part_id),
                    session_id: part.session_id,
                    entry_type: "tool".to_owned(),
                    seq: part.seq,
                    time_created: part.time_created,
                    time_updated: part.time_updated,
                    data: data.to_string(),
                });
            }
            continue;
        };
        if !matches!(part.role.as_str(), "assistant" | "user" | "system") {
            continue;
        }
        if !text_has_real_content(Some(&text)) {
            continue;
        }
        let role = part.role.clone();
        let message_id = part.message_id.clone();
        let part_id = part.part_id.clone();
        let part_type = part.part_type.clone();
        let file_touches = if file_touches_emitted_by_message.insert(message_id.clone()) {
            file_touches_by_message
                .remove(&part.message_id)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let data = json!({
            "role": role,
            "time": { "created": part.time_created },
            "text": text,
            "source_table": "message+part",
            "message_id": message_id,
            "part_id": part_id,
            "part_type": part_type,
            "file_touches": file_touches,
        })
        .to_string();
        out.push(OpenCodeMessageRow {
            id: format!("{}:{}", part.message_id, part.part_id),
            session_id: part.session_id,
            entry_type: part.role,
            seq: part.seq,
            time_created: part.time_created,
            time_updated: part.time_updated,
            data,
        });
    }

    Ok((out, skipped_oversized))
}

fn placeholders(count: usize) -> String {
    std::iter::repeat_n("?", count)
        .collect::<Vec<_>>()
        .join(", ")
}

fn opencode_part_type(column_type: Option<&str>, data: &Value) -> String {
    column_type
        .filter(|value| !value.trim().is_empty())
        .or_else(|| data.get("type").and_then(Value::as_str))
        .unwrap_or("part")
        .to_owned()
}

fn opencode_text_part_text(part: &OpenCodePartRow) -> Option<String> {
    (part.part_type == "text")
        .then(|| part.data.get("text").and_then(Value::as_str))
        .flatten()
        .map(str::to_owned)
}

fn opencode_tool_part_event_data(part: &OpenCodePartRow) -> Option<Value> {
    if !matches!(part.part_type.as_str(), "tool" | "tool_result" | "result") {
        return None;
    }
    let tool_name = opencode_tool_part_name(&part.data);
    let status = opencode_tool_part_status(&part.data);
    let exit_code = opencode_tool_part_exit_code(&part.data);
    let is_error = opencode_tool_part_is_error(&part.data, status.as_deref(), exit_code);
    Some(json!({
        "role": "tool",
        "time": { "created": part.time_created },
        "source_table": "message+part",
        "message_id": part.message_id.clone(),
        "part_id": part.part_id.clone(),
        "part_type": part.part_type.clone(),
        "tool_name": tool_name,
        "status": status,
        "exit_code": exit_code,
        "is_error": is_error,
        "output_retention": "metadata_only",
    }))
}

fn opencode_tool_part_name(data: &Value) -> String {
    data.get("tool")
        .or_else(|| data.get("tool_name"))
        .or_else(|| data.get("name"))
        .and_then(Value::as_str)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("tool")
        .to_owned()
}

fn opencode_tool_part_status(data: &Value) -> Option<String> {
    data.pointer("/state/status")
        .or_else(|| data.get("status"))
        .and_then(Value::as_str)
        .filter(|status| !status.trim().is_empty())
        .map(str::to_owned)
}

fn opencode_tool_part_exit_code(data: &Value) -> Option<i64> {
    data.pointer("/state/metadata/exit")
        .or_else(|| data.pointer("/state/metadata/exit_code"))
        .or_else(|| data.pointer("/state/metadata/exitCode"))
        .or_else(|| data.get("exit_code"))
        .or_else(|| data.get("exitCode"))
        .and_then(Value::as_i64)
}

fn opencode_tool_part_is_error(data: &Value, status: Option<&str>, exit_code: Option<i64>) -> bool {
    data.get("is_error")
        .or_else(|| data.get("isError"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || exit_code.is_some_and(|code| code != 0)
        || status.is_some_and(|status| {
            matches!(
                status.trim().to_ascii_lowercase().as_str(),
                "failed" | "failure" | "error" | "errored" | "timeout" | "timed_out" | "timedout"
            )
        })
}

fn opencode_message_part_identity_index(message_id: &str, part_id: &str) -> i64 {
    let key = format!("message+part:{message_id}:{part_id}");
    let index = fnv1a64(key.as_bytes()) & 0x0000_ffff_ffff;
    index.max(1) as i64
}

fn opencode_part_file_touch_metadata(part: &OpenCodePartRow) -> Vec<Value> {
    if part.invalid_json || part.part_type != "patch" {
        return Vec::new();
    }
    let mut paths = BTreeSet::<String>::new();
    if let Some(path) = part
        .data
        .get("path")
        .and_then(Value::as_str)
        .and_then(normalize_file_path)
    {
        paths.insert(path);
    }
    if let Some(files) = part.data.get("files").and_then(Value::as_array) {
        for file in files {
            let path = match file {
                Value::String(path) => normalize_file_path(path),
                Value::Object(object) => object
                    .get("path")
                    .and_then(Value::as_str)
                    .and_then(normalize_file_path),
                _ => None,
            };
            if let Some(path) = path {
                paths.insert(path);
            }
        }
    }
    paths
        .into_iter()
        .map(|path| {
            json!({
                "path": path,
                "part_id": part.part_id,
                "part_type": part.part_type,
            })
        })
        .collect()
}

fn opencode_message_part_file_touch_drafts(data: &Value) -> Vec<FileTouchDraft> {
    data.get("file_touches")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|touch| {
            let path = touch
                .get("path")
                .and_then(Value::as_str)
                .and_then(normalize_file_path)?;
            Some(FileTouchDraft {
                path,
                old_path: None,
                change_kind: Some(FileChangeKind::Modified),
                confidence: Confidence::Explicit,
                metadata: json!({
                    "source": "opencode_message_part_metadata",
                    "part_id": touch.get("part_id").cloned(),
                    "part_type": touch.get("part_type").cloned(),
                }),
            })
        })
        .collect()
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
    let is_message_part = data.get("source_table").and_then(Value::as_str) == Some("message+part");
    let event_type = opencode_event_type(&row.entry_type, data);
    let role = Some(provider_role(Some(&row.entry_type)));
    let text = opencode_event_text(&row.entry_type, data, event_type, dialect);
    let body = if is_message_part {
        opencode_message_part_event_body(data)
    } else {
        data.clone()
    };
    let retained_text = provider_policy_event_text(event_type, &text, &body);
    let payload = if is_message_part {
        json!({
            "entry_type": row.entry_type,
            "message_id": data
                .get("message_id")
                .and_then(Value::as_str)
                .unwrap_or(&row.id),
            "part_id": data.get("part_id").cloned(),
            "part_type": data.get("part_type").cloned(),
            "session_message_seq": row.seq,
            "text": retained_text.text,
            "text_retention": retained_text.retention.as_json(),
            "body": provider_capped_json(&provider_policy_body(event_type, &body), PROVIDER_MAX_PREVIEW_CHARS),
        })
    } else {
        json!({
            "entry_type": row.entry_type,
            "message_id": row.id,
            "session_message_seq": row.seq,
            "text": retained_text.text,
            "text_retention": retained_text.retention.as_json(),
            "body": provider_capped_json(&provider_policy_body(event_type, &body), PROVIDER_MAX_PREVIEW_CHARS),
        })
    };
    let metadata = if is_message_part {
        json!({
            "source": dialect.source_format,
            "source_format": dialect.source_format,
            "session_message_id": row.id,
            "session_message_seq": row.seq,
            "message_id": data.get("message_id").cloned(),
            "part_id": data.get("part_id").cloned(),
            "part_type": data.get("part_type").cloned(),
            "time_created": row.time_created,
            "time_updated": row.time_updated,
            "model": data.get("model").cloned(),
            "tokens": data.get("tokens").cloned(),
            "cost": data.get("cost").cloned(),
            "finish": data.get("finish").cloned(),
            "error": data.get("error").cloned(),
        })
    } else {
        json!({
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
        })
    };
    ProviderEventEnvelope {
        provider_event_index,
        provider_event_hash: Some(row.id.clone()),
        cursor: Some(opencode_event_cursor(row, data)),
        event_type,
        role,
        occurred_at,
        fidelity: Fidelity::Imported,
        idempotency_key: Some(format!(
            "provider-event:{}:{}:{}",
            dialect.provider.as_str(),
            row.session_id,
            row.id
        )),
        artifacts: Vec::new(),
        payload,
        metadata,
    }
}

fn opencode_message_part_event_body(data: &Value) -> Value {
    json!({
        "role": data.get("role").cloned(),
        "time": data.get("time").cloned(),
        "text": data.get("text").cloned(),
        "source_table": data.get("source_table").cloned(),
        "message_id": data.get("message_id").cloned(),
        "part_id": data.get("part_id").cloned(),
        "part_type": data.get("part_type").cloned(),
        "tool_name": data.get("tool_name").cloned(),
        "status": data.get("status").cloned(),
        "exit_code": data.get("exit_code").cloned(),
        "is_error": data.get("is_error").cloned(),
        "output_retention": data.get("output_retention").cloned(),
    })
}

pub(crate) fn opencode_event_cursor(row: &OpenCodeMessageRow, data: &Value) -> String {
    if data.get("source_table").and_then(Value::as_str) == Some("message+part") {
        return format!(
            "message:{}:part:{}",
            data.get("message_id")
                .and_then(Value::as_str)
                .unwrap_or(&row.id),
            data.get("part_id")
                .and_then(Value::as_str)
                .unwrap_or(&row.id)
        );
    }
    format!("session_message:{}:seq:{}", row.session_id, row.seq)
}

pub(crate) fn opencode_event_type(entry_type: &str, data: &Value) -> EventType {
    match entry_type {
        "assistant" if opencode_content_has_tool(data) => EventType::ToolCall,
        "assistant" | "user" | "system" => EventType::Message,
        "tool" | "tool_result" => EventType::ToolOutput,
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
    if matches!(entry_type, "tool" | "tool_result") {
        let tool_name = data
            .get("tool_name")
            .and_then(Value::as_str)
            .unwrap_or("tool");
        let status = data
            .get("status")
            .and_then(Value::as_str)
            .map(|status| format!("\nstatus: {status}"))
            .unwrap_or_default();
        return format!("tool result: {tool_name}{status}");
    }
    if let Some(content) = data.get("content") {
        if let Some(text) = provider_value_text(content) {
            return text;
        }
    }
    if event_type == EventType::Notice {
        format!("{} event: {entry_type}", dialect.display_name)
    } else {
        String::new()
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
