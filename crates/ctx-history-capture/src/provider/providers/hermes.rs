use std::{collections::BTreeMap, path::Path};

use ctx_history_core::{AgentType, CaptureProvider, EventType, Fidelity, ProviderSourceTrust};
use rusqlite::Connection;
use serde_json::json;

use crate::provider::custom_history_jsonl::push_provider_import_failure;
use crate::provider::native::{
    hermes_decode_content, native_event, native_provider_capture, open_provider_sqlite_readonly,
    provider_json_text, provider_line_from_index, provider_nonnegative_i64_to_u64,
    provider_required_timestamp_seconds, provider_role, provider_value_text, NativeEventDraft,
    NativeSessionDraft,
};
use crate::provider::sqlite::{
    ensure_sqlite_table_columns, opencode_schema_fingerprint, optional_column_expr,
    sqlite_table_columns, sqlite_table_exists,
};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderNormalizationResult, Result,
    HERMES_SQLITE_SOURCE_FORMAT,
};

pub(crate) struct HermesSessionRow {
    pub(crate) id: String,
    pub(crate) source: String,
    pub(crate) parent_session_id: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) model_config: Option<String>,
    pub(crate) started_at: f64,
    pub(crate) ended_at: Option<f64>,
    pub(crate) end_reason: Option<String>,
    pub(crate) message_count: i64,
    pub(crate) tool_call_count: i64,
    pub(crate) input_tokens: i64,
    pub(crate) output_tokens: i64,
    pub(crate) cache_read_tokens: i64,
    pub(crate) cache_write_tokens: i64,
    pub(crate) reasoning_tokens: i64,
    pub(crate) cwd: Option<String>,
    pub(crate) git_branch: Option<String>,
    pub(crate) git_repo_root: Option<String>,
    pub(crate) billing_provider: Option<String>,
    pub(crate) billing_base_url: Option<String>,
    pub(crate) billing_mode: Option<String>,
    pub(crate) estimated_cost_usd: Option<f64>,
    pub(crate) actual_cost_usd: Option<f64>,
    pub(crate) title: Option<String>,
    pub(crate) archived: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct HermesMessageRow {
    pub(crate) id: i64,
    pub(crate) session_id: String,
    pub(crate) role: String,
    pub(crate) content: Option<String>,
    pub(crate) tool_call_id: Option<String>,
    pub(crate) tool_calls: Option<String>,
    pub(crate) tool_name: Option<String>,
    pub(crate) timestamp: f64,
    pub(crate) token_count: Option<i64>,
    pub(crate) finish_reason: Option<String>,
    pub(crate) reasoning: Option<String>,
    pub(crate) reasoning_content: Option<String>,
    pub(crate) reasoning_details: Option<String>,
    pub(crate) codex_reasoning_items: Option<String>,
    pub(crate) codex_message_items: Option<String>,
    pub(crate) platform_message_id: Option<String>,
    pub(crate) observed: i64,
    pub(crate) active: i64,
    pub(crate) compacted: i64,
}

pub(crate) fn normalize_hermes_sqlite(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    let conn = open_provider_sqlite_readonly(path)?;
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let schema_fingerprint = opencode_schema_fingerprint(&conn)?;
    let sessions = hermes_sessions(&conn)?;
    let messages = hermes_messages(&conn)?;
    let sessions_by_id = sessions
        .into_iter()
        .map(|session| (session.id.clone(), session))
        .collect::<BTreeMap<_, _>>();
    let mut result = ProviderNormalizationResult::default();

    for row in messages {
        let provider_event_index =
            match provider_nonnegative_i64_to_u64(row.id, "Hermes message id") {
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
                    "Hermes message {} references missing session {}",
                    row.id, row.session_id
                ),
            );
            continue;
        };
        let provider_session_id = session.id.clone();
        let occurred_at =
            match provider_required_timestamp_seconds(row.timestamp, "Hermes message timestamp") {
                Ok(timestamp) => timestamp,
                Err(err) => {
                    push_provider_import_failure(&mut result.summary, line, err.to_string());
                    continue;
                }
            };
        let started_at = match provider_required_timestamp_seconds(
            session.started_at,
            "Hermes session started_at",
        ) {
            Ok(timestamp) => timestamp,
            Err(err) => {
                push_provider_import_failure(&mut result.summary, line, err.to_string());
                continue;
            }
        };
        let ended_at = match session
            .ended_at
            .map(|timestamp| {
                provider_required_timestamp_seconds(timestamp, "Hermes session ended_at")
            })
            .transpose()
        {
            Ok(timestamp) => timestamp,
            Err(err) => {
                push_provider_import_failure(&mut result.summary, line, err.to_string());
                continue;
            }
        };
        let content = hermes_decode_content(row.content.as_deref());
        let text = provider_value_text(&content).unwrap_or_else(|| {
            row.tool_name
                .as_ref()
                .map(|name| format!("tool: {name}"))
                .unwrap_or_else(|| format!("Hermes {}", row.role))
        });
        let event_type = hermes_event_type(&row);
        let role = Some(provider_role(Some(&row.role)));
        let event = native_event(NativeEventDraft {
            provider: CaptureProvider::Hermes,
            source_format: HERMES_SQLITE_SOURCE_FORMAT,
            provider_session_id: provider_session_id.clone(),
            provider_event_index,
            provider_event_hash: Some(format!("message:{}", row.id)),
            cursor: format!("messages:id:{}", row.id),
            event_type,
            role,
            occurred_at,
            text,
            body: json!({
                "message_id": row.id,
                "role": row.role,
                "content": content,
                "tool_call_id": row.tool_call_id,
                "tool_calls": row.tool_calls.as_deref().map(provider_json_text),
                "tool_name": row.tool_name,
                "reasoning": row.reasoning,
                "reasoning_content": row.reasoning_content,
                "reasoning_details": row.reasoning_details.as_deref().map(provider_json_text),
                "codex_reasoning_items": row.codex_reasoning_items.as_deref().map(provider_json_text),
                "codex_message_items": row.codex_message_items.as_deref().map(provider_json_text),
            }),
            metadata: json!({
                "source": "hermes_state_db",
                "source_format": HERMES_SQLITE_SOURCE_FORMAT,
                "message_id": row.id,
                "platform_message_id": row.platform_message_id,
                "token_count": row.token_count,
                "finish_reason": row.finish_reason,
                "observed": row.observed != 0,
                "active": row.active != 0,
                "compacted": row.compacted != 0,
            }),
        });
        result.captures.push((
            line,
            native_provider_capture(
                NativeSessionDraft {
                    provider: CaptureProvider::Hermes,
                    source_format: HERMES_SQLITE_SOURCE_FORMAT,
                    provider_session_id: provider_session_id.clone(),
                    parent_provider_session_id: session.parent_session_id.clone(),
                    root_provider_session_id: None,
                    external_agent_id: Some(session.source.clone()),
                    agent_type: if session.parent_session_id.is_some() {
                        AgentType::Subagent
                    } else {
                        AgentType::Primary
                    },
                    role_hint: Some(session.source.clone()),
                    is_primary: session.parent_session_id.is_none(),
                    started_at,
                    ended_at,
                    cwd: session.cwd.clone(),
                    fidelity: Fidelity::Imported,
                    raw_source_path: path.display().to_string(),
                    trust: ProviderSourceTrust::ProviderNative,
                    source_metadata: json!({
                        "adapter": HERMES_SQLITE_SOURCE_FORMAT,
                        "sqlite_user_version": user_version,
                        "schema_fingerprint": schema_fingerprint,
                        "upstream_schema_version_at_research": 17,
                    }),
                    session_metadata: json!({
                        "source_format": HERMES_SQLITE_SOURCE_FORMAT,
                        "source": session.source,
                        "title": session.title,
                        "model": session.model,
                        "model_config": session.model_config.as_deref().map(provider_json_text),
                        "end_reason": session.end_reason,
                        "message_count": session.message_count,
                        "tool_call_count": session.tool_call_count,
                        "tokens": {
                            "input": session.input_tokens,
                            "output": session.output_tokens,
                            "cache_read": session.cache_read_tokens,
                            "cache_write": session.cache_write_tokens,
                            "reasoning": session.reasoning_tokens,
                        },
                        "git": {
                            "branch": session.git_branch,
                            "repo_root": session.git_repo_root,
                        },
                        "billing": {
                            "provider": session.billing_provider,
                            "base_url": session.billing_base_url,
                            "mode": session.billing_mode,
                            "estimated_cost_usd": session.estimated_cost_usd,
                            "actual_cost_usd": session.actual_cost_usd,
                        },
                        "archived": session.archived != 0,
                    }),
                },
                context,
                Some(event),
            ),
        ));
    }

    Ok(result)
}

pub(crate) fn hermes_event_type(row: &HermesMessageRow) -> EventType {
    if row.role == "tool" {
        EventType::ToolOutput
    } else if row
        .tool_calls
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        || row
            .tool_name
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
    {
        EventType::ToolCall
    } else {
        EventType::Message
    }
}

pub(crate) fn hermes_sessions(conn: &Connection) -> Result<Vec<HermesSessionRow>> {
    if !sqlite_table_exists(conn, "sessions")? {
        return Err(CaptureError::InvalidPayload(
            "Hermes state.db is missing required sessions table".into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "sessions")?;
    ensure_sqlite_table_columns(
        &columns,
        "Hermes sessions table",
        &["id", "source", "started_at"],
    )?;
    let parent_session_id = optional_column_expr(&columns, "parent_session_id", "NULL");
    let model = optional_column_expr(&columns, "model", "NULL");
    let model_config = optional_column_expr(&columns, "model_config", "NULL");
    let ended_at = optional_column_expr(&columns, "ended_at", "NULL");
    let end_reason = optional_column_expr(&columns, "end_reason", "NULL");
    let message_count = optional_column_expr(&columns, "message_count", "0");
    let tool_call_count = optional_column_expr(&columns, "tool_call_count", "0");
    let input_tokens = optional_column_expr(&columns, "input_tokens", "0");
    let output_tokens = optional_column_expr(&columns, "output_tokens", "0");
    let cache_read_tokens = optional_column_expr(&columns, "cache_read_tokens", "0");
    let cache_write_tokens = optional_column_expr(&columns, "cache_write_tokens", "0");
    let reasoning_tokens = optional_column_expr(&columns, "reasoning_tokens", "0");
    let cwd = optional_column_expr(&columns, "cwd", "NULL");
    let git_branch = optional_column_expr(&columns, "git_branch", "NULL");
    let git_repo_root = optional_column_expr(&columns, "git_repo_root", "NULL");
    let billing_provider = optional_column_expr(&columns, "billing_provider", "NULL");
    let billing_base_url = optional_column_expr(&columns, "billing_base_url", "NULL");
    let billing_mode = optional_column_expr(&columns, "billing_mode", "NULL");
    let estimated_cost_usd = optional_column_expr(&columns, "estimated_cost_usd", "NULL");
    let actual_cost_usd = optional_column_expr(&columns, "actual_cost_usd", "NULL");
    let title = optional_column_expr(&columns, "title", "NULL");
    let archived = optional_column_expr(&columns, "archived", "0");
    let sql = format!(
        "select id, source, {parent_session_id}, {model}, {model_config}, started_at, \
         {ended_at}, {end_reason}, {message_count}, {tool_call_count}, {input_tokens}, \
         {output_tokens}, {cache_read_tokens}, {cache_write_tokens}, {reasoning_tokens}, \
         {cwd}, {git_branch}, {git_repo_root}, {billing_provider}, {billing_base_url}, \
         {billing_mode}, {estimated_cost_usd}, {actual_cost_usd}, {title}, {archived} \
         from sessions order by started_at, id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok(HermesSessionRow {
            id: row.get(0)?,
            source: row.get(1)?,
            parent_session_id: row.get(2)?,
            model: row.get(3)?,
            model_config: row.get(4)?,
            started_at: row.get(5)?,
            ended_at: row.get(6)?,
            end_reason: row.get(7)?,
            message_count: row.get(8)?,
            tool_call_count: row.get(9)?,
            input_tokens: row.get(10)?,
            output_tokens: row.get(11)?,
            cache_read_tokens: row.get(12)?,
            cache_write_tokens: row.get(13)?,
            reasoning_tokens: row.get(14)?,
            cwd: row.get(15)?,
            git_branch: row.get(16)?,
            git_repo_root: row.get(17)?,
            billing_provider: row.get(18)?,
            billing_base_url: row.get(19)?,
            billing_mode: row.get(20)?,
            estimated_cost_usd: row.get(21)?,
            actual_cost_usd: row.get(22)?,
            title: row.get(23)?,
            archived: row.get(24)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CaptureError::from)
}

pub(crate) fn hermes_messages(conn: &Connection) -> Result<Vec<HermesMessageRow>> {
    if !sqlite_table_exists(conn, "messages")? {
        return Err(CaptureError::InvalidPayload(
            "Hermes state.db is missing required messages table".into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "messages")?;
    ensure_sqlite_table_columns(
        &columns,
        "Hermes messages table",
        &["id", "session_id", "role", "timestamp"],
    )?;
    let content = optional_column_expr(&columns, "content", "NULL");
    let tool_call_id = optional_column_expr(&columns, "tool_call_id", "NULL");
    let tool_calls = optional_column_expr(&columns, "tool_calls", "NULL");
    let tool_name = optional_column_expr(&columns, "tool_name", "NULL");
    let token_count = optional_column_expr(&columns, "token_count", "NULL");
    let finish_reason = optional_column_expr(&columns, "finish_reason", "NULL");
    let reasoning = optional_column_expr(&columns, "reasoning", "NULL");
    let reasoning_content = optional_column_expr(&columns, "reasoning_content", "NULL");
    let reasoning_details = optional_column_expr(&columns, "reasoning_details", "NULL");
    let codex_reasoning_items = optional_column_expr(&columns, "codex_reasoning_items", "NULL");
    let codex_message_items = optional_column_expr(&columns, "codex_message_items", "NULL");
    let platform_message_id = optional_column_expr(&columns, "platform_message_id", "NULL");
    let observed = optional_column_expr(&columns, "observed", "0");
    let active = optional_column_expr(&columns, "active", "1");
    let compacted = optional_column_expr(&columns, "compacted", "0");
    let visibility = if columns.contains("active") || columns.contains("compacted") {
        format!("where ({active} = 1 or {compacted} = 1)")
    } else {
        String::new()
    };
    let sql = format!(
        "select id, session_id, role, {content}, {tool_call_id}, {tool_calls}, {tool_name}, \
         timestamp, {token_count}, {finish_reason}, {reasoning}, {reasoning_content}, \
         {reasoning_details}, {codex_reasoning_items}, {codex_message_items}, \
         {platform_message_id}, {observed}, {active}, {compacted} \
         from messages {visibility} order by session_id, id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok(HermesMessageRow {
            id: row.get(0)?,
            session_id: row.get(1)?,
            role: row.get(2)?,
            content: row.get(3)?,
            tool_call_id: row.get(4)?,
            tool_calls: row.get(5)?,
            tool_name: row.get(6)?,
            timestamp: row.get(7)?,
            token_count: row.get(8)?,
            finish_reason: row.get(9)?,
            reasoning: row.get(10)?,
            reasoning_content: row.get(11)?,
            reasoning_details: row.get(12)?,
            codex_reasoning_items: row.get(13)?,
            codex_message_items: row.get(14)?,
            platform_message_id: row.get(15)?,
            observed: row.get(16)?,
            active: row.get(17)?,
            compacted: row.get(18)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CaptureError::from)
}
