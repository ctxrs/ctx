use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

use ctx_history_core::{
    AgentType, CaptureProvider, EventRole, EventType, Fidelity, ProviderSourceTrust,
};
use rusqlite::Connection;
use serde_json::{json, Value};

use crate::provider::custom_history_jsonl::push_provider_import_failure;
use crate::provider::native::{
    native_event, native_provider_capture, open_provider_sqlite_readonly, provider_json_text,
    provider_nonnegative_i64_to_u64, provider_timestamp_millis, provider_value_text, text_id_index,
    NativeEventDraft, NativeSessionDraft,
};
use crate::provider::sqlite::{
    ensure_sqlite_table_columns, opencode_schema_fingerprint, optional_column_expr,
    sqlite_table_columns, sqlite_table_exists,
};
use crate::{
    fnv1a64, CaptureError, ProviderAdapterContext, ProviderNormalizationResult, Result,
    NANOCLAW_SOURCE_FORMAT,
};

pub(crate) struct NanoClawSessionRow {
    pub(crate) id: String,
    pub(crate) agent_group_id: String,
    pub(crate) messaging_group_id: Option<String>,
    pub(crate) thread_id: Option<String>,
    pub(crate) agent_provider: Option<String>,
    pub(crate) status: Option<String>,
    pub(crate) container_status: Option<String>,
    pub(crate) last_active: Option<i64>,
    pub(crate) created_at: Option<i64>,
    pub(crate) agent_group_name: Option<String>,
    pub(crate) agent_group_folder: Option<String>,
    pub(crate) messaging_channel_type: Option<String>,
    pub(crate) messaging_platform_id: Option<String>,
    pub(crate) messaging_instance: Option<String>,
    pub(crate) messaging_name: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct NanoClawMessageRow {
    pub(crate) source: &'static str,
    pub(crate) id: String,
    pub(crate) seq: Option<i64>,
    pub(crate) kind: Option<String>,
    pub(crate) timestamp: Option<i64>,
    pub(crate) status: Option<String>,
    pub(crate) in_reply_to: Option<String>,
    pub(crate) platform_id: Option<String>,
    pub(crate) channel_type: Option<String>,
    pub(crate) thread_id: Option<String>,
    pub(crate) content: Option<String>,
    pub(crate) trigger: Option<String>,
    pub(crate) source_session_id: Option<String>,
    pub(crate) on_wake: Option<i64>,
}

pub(crate) fn normalize_nanoclaw_project(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    let project_root = nanoclaw_project_root(path)?;
    let central_path = project_root.join("data").join("v2.db");
    let conn = open_provider_sqlite_readonly(&central_path)?;
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let schema_fingerprint = opencode_schema_fingerprint(&conn)?;
    let sessions = nanoclaw_sessions(&conn)?;
    let mut result = ProviderNormalizationResult::default();
    for session in sessions {
        let session_dir = project_root
            .join("data")
            .join("v2-sessions")
            .join(&session.agent_group_id)
            .join(&session.id);
        let mut messages = Vec::new();
        let inbound_path = session_dir.join("inbound.db");
        if inbound_path.is_file() {
            messages.extend(nanoclaw_inbound_messages(&inbound_path)?);
        }
        let outbound_path = session_dir.join("outbound.db");
        if outbound_path.is_file() {
            messages.extend(nanoclaw_outbound_messages(&outbound_path)?);
        }
        messages.sort_by_key(|message| {
            (
                message.timestamp.unwrap_or_default(),
                message.seq.unwrap_or_default(),
                message.source,
                message.id.clone(),
            )
        });
        for message in messages {
            let seq = match message
                .seq
                .map(|seq| provider_nonnegative_i64_to_u64(seq, "NanoClaw message seq"))
                .transpose()
            {
                Ok(seq) => seq,
                Err(err) => {
                    push_provider_import_failure(&mut result.summary, 0, err.to_string());
                    continue;
                }
            };
            let provider_session_id = format!("{}/{}", session.agent_group_id, session.id);
            let occurred_at = provider_timestamp_millis(message.timestamp, context.imported_at);
            let started_at = provider_timestamp_millis(session.created_at, occurred_at);
            let content = message
                .content
                .as_deref()
                .map(provider_json_text)
                .unwrap_or(Value::Null);
            let text = provider_value_text(&content).unwrap_or_else(|| {
                format!(
                    "NanoClaw {}",
                    message.kind.as_deref().unwrap_or(message.source)
                )
            });
            let event_index = nanoclaw_event_index(&message, seq);
            let role = if message.source == "inbound" {
                Some(EventRole::User)
            } else {
                Some(EventRole::Assistant)
            };
            let event = native_event(NativeEventDraft {
                provider: CaptureProvider::NanoClaw,
                source_format: NANOCLAW_SOURCE_FORMAT,
                provider_session_id: provider_session_id.clone(),
                provider_event_index: event_index,
                provider_event_hash: Some(format!("{}:{}", message.source, message.id)),
                cursor: format!(
                    "{}:{}:{}",
                    message.source,
                    session.id,
                    message.seq.unwrap_or_default()
                ),
                event_type: EventType::Message,
                role,
                occurred_at,
                text,
                body: json!({
                    "message_id": message.id,
                    "seq": message.seq,
                    "kind": message.kind,
                    "content": content,
                    "status": message.status,
                    "in_reply_to": message.in_reply_to,
                    "platform_id": message.platform_id,
                    "channel_type": message.channel_type,
                    "thread_id": message.thread_id,
                    "trigger": message.trigger,
                    "source_session_id": message.source_session_id,
                    "on_wake": message.on_wake,
                }),
                metadata: json!({
                    "source": format!("nanoclaw_{}", message.source),
                    "source_format": NANOCLAW_SOURCE_FORMAT,
                    "message_id": message.id,
                    "seq": message.seq,
                }),
            });
            result.captures.push((
                event_index.min(usize::MAX as u64) as usize,
                native_provider_capture(
                    NativeSessionDraft {
                        provider: CaptureProvider::NanoClaw,
                        source_format: NANOCLAW_SOURCE_FORMAT,
                        provider_session_id: provider_session_id.clone(),
                        parent_provider_session_id: None,
                        root_provider_session_id: None,
                        external_agent_id: session.agent_provider.clone(),
                        agent_type: AgentType::Primary,
                        role_hint: Some("container-session".to_owned()),
                        is_primary: true,
                        started_at,
                        ended_at: session.last_active.map(|timestamp| {
                            provider_timestamp_millis(Some(timestamp), context.imported_at)
                        }),
                        cwd: session.agent_group_folder.clone(),
                        fidelity: Fidelity::Partial,
                        raw_source_path: project_root.display().to_string(),
                        trust: ProviderSourceTrust::ProviderNative,
                        source_metadata: json!({
                            "adapter": NANOCLAW_SOURCE_FORMAT,
                            "central_db": central_path.display().to_string(),
                            "sqlite_user_version": user_version,
                            "schema_fingerprint": schema_fingerprint,
                            "support_level": "explicit",
                        }),
                        session_metadata: json!({
                            "source_format": NANOCLAW_SOURCE_FORMAT,
                            "session_id": session.id,
                            "agent_group_id": session.agent_group_id,
                            "agent_group_name": session.agent_group_name,
                            "agent_provider": session.agent_provider,
                            "status": session.status,
                            "container_status": session.container_status,
                            "messaging_group_id": session.messaging_group_id,
                            "messaging": {
                                "channel_type": session.messaging_channel_type,
                                "platform_id": session.messaging_platform_id,
                                "instance": session.messaging_instance,
                                "name": session.messaging_name,
                                "thread_id": session.thread_id,
                            },
                        }),
                    },
                    context,
                    Some(event),
                ),
            ));
        }
    }
    Ok(result)
}

pub(crate) fn nanoclaw_project_root(path: &Path) -> Result<PathBuf> {
    if path.is_dir() && path.join("data").join("v2.db").is_file() {
        return Ok(path.to_path_buf());
    }
    if path.file_name().and_then(|name| name.to_str()) == Some("v2.db") {
        if let Some(data_dir) = path.parent() {
            if let Some(root) = data_dir.parent() {
                return Ok(root.to_path_buf());
            }
        }
    }
    Err(CaptureError::InvalidProviderTranscriptPath {
        path: path.to_path_buf(),
        reason: "NanoClaw import path must be a project root or data/v2.db",
    })
}

pub(crate) fn nanoclaw_event_index(message: &NanoClawMessageRow, seq: Option<u64>) -> u64 {
    if let Some(seq) = seq {
        let source_bucket = if message.source == "outbound" {
            500_000
        } else {
            0
        };
        let row_bucket = fnv1a64(format!("{}:{}", message.source, message.id).as_bytes()) % 500_000;
        return seq
            .saturating_mul(1_000_000)
            .saturating_add(source_bucket)
            .saturating_add(row_bucket);
    }
    text_id_index(&format!("{}:{}", message.source, message.id), 2_000_000_000)
}

pub(crate) fn nanoclaw_sessions(conn: &Connection) -> Result<Vec<NanoClawSessionRow>> {
    if !sqlite_table_exists(conn, "sessions")? {
        return Err(CaptureError::InvalidPayload(
            "NanoClaw data/v2.db is missing required sessions table".into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "sessions")?;
    ensure_sqlite_table_columns(
        &columns,
        "NanoClaw sessions table",
        &["id", "agent_group_id"],
    )?;
    let messaging_group_id = optional_column_expr(&columns, "messaging_group_id", "NULL");
    let thread_id = optional_column_expr(&columns, "thread_id", "NULL");
    let agent_provider = optional_column_expr(&columns, "agent_provider", "NULL");
    let status = optional_column_expr(&columns, "status", "NULL");
    let container_status = optional_column_expr(&columns, "container_status", "NULL");
    let last_active = optional_column_expr(&columns, "last_active", "NULL");
    let created_at = optional_column_expr(&columns, "created_at", "NULL");
    let agent_group_columns = if sqlite_table_exists(conn, "agent_groups")? {
        sqlite_table_columns(conn, "agent_groups")?
    } else {
        BTreeSet::new()
    };
    let agent_group_name =
        if agent_group_columns.contains("id") && agent_group_columns.contains("name") {
            "(select name from agent_groups where agent_groups.id = sessions.agent_group_id)"
        } else {
            "NULL"
        };
    let agent_group_folder =
        if agent_group_columns.contains("id") && agent_group_columns.contains("folder") {
            "(select folder from agent_groups where agent_groups.id = sessions.agent_group_id)"
        } else {
            "NULL"
        };
    let (messaging_channel_type, messaging_platform_id, messaging_instance, messaging_name) =
        if columns.contains("messaging_group_id") && sqlite_table_exists(conn, "messaging_groups")?
        {
            let messaging_columns = sqlite_table_columns(conn, "messaging_groups")?;
            (
                if messaging_columns.contains("id") && messaging_columns.contains("channel_type") {
                    "(select channel_type from messaging_groups where messaging_groups.id = sessions.messaging_group_id)"
                } else {
                    "NULL"
                },
                if messaging_columns.contains("id") && messaging_columns.contains("platform_id") {
                    "(select platform_id from messaging_groups where messaging_groups.id = sessions.messaging_group_id)"
                } else {
                    "NULL"
                },
                if messaging_columns.contains("id") && messaging_columns.contains("instance") {
                    "(select instance from messaging_groups where messaging_groups.id = sessions.messaging_group_id)"
                } else {
                    "NULL"
                },
                if messaging_columns.contains("id") && messaging_columns.contains("name") {
                    "(select name from messaging_groups where messaging_groups.id = sessions.messaging_group_id)"
                } else {
                    "NULL"
                },
            )
        } else {
            ("NULL", "NULL", "NULL", "NULL")
        };
    let sql = format!(
        "select id, agent_group_id, {messaging_group_id}, {thread_id}, {agent_provider}, \
         {status}, {container_status}, {last_active}, {created_at}, {agent_group_name}, \
         {agent_group_folder}, {messaging_channel_type}, {messaging_platform_id}, \
         {messaging_instance}, {messaging_name} from sessions order by created_at, id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok(NanoClawSessionRow {
            id: row.get(0)?,
            agent_group_id: row.get(1)?,
            messaging_group_id: row.get(2)?,
            thread_id: row.get(3)?,
            agent_provider: row.get(4)?,
            status: row.get(5)?,
            container_status: row.get(6)?,
            last_active: row.get(7)?,
            created_at: row.get(8)?,
            agent_group_name: row.get(9)?,
            agent_group_folder: row.get(10)?,
            messaging_channel_type: row.get(11)?,
            messaging_platform_id: row.get(12)?,
            messaging_instance: row.get(13)?,
            messaging_name: row.get(14)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CaptureError::from)
}

pub(crate) fn nanoclaw_inbound_messages(path: &Path) -> Result<Vec<NanoClawMessageRow>> {
    let conn = open_provider_sqlite_readonly(path)?;
    if !sqlite_table_exists(&conn, "messages_in")? {
        return Ok(Vec::new());
    }
    let columns = sqlite_table_columns(&conn, "messages_in")?;
    ensure_sqlite_table_columns(&columns, "NanoClaw inbound messages table", &["id"])?;
    let seq = optional_column_expr(&columns, "seq", "NULL");
    let kind = optional_column_expr(&columns, "kind", "NULL");
    let timestamp = optional_column_expr(&columns, "timestamp", "NULL");
    let status = optional_column_expr(&columns, "status", "NULL");
    let trigger = optional_column_expr(&columns, "trigger", "NULL");
    let platform_id = optional_column_expr(&columns, "platform_id", "NULL");
    let channel_type = optional_column_expr(&columns, "channel_type", "NULL");
    let thread_id = optional_column_expr(&columns, "thread_id", "NULL");
    let content = optional_column_expr(&columns, "content", "NULL");
    let source_session_id = optional_column_expr(&columns, "source_session_id", "NULL");
    let on_wake = optional_column_expr(&columns, "on_wake", "NULL");
    let sql = format!(
        "select id, {seq}, {kind}, {timestamp}, {status}, {trigger}, {platform_id}, \
         {channel_type}, {thread_id}, {content}, {source_session_id}, {on_wake} \
         from messages_in order by {seq}, id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok(NanoClawMessageRow {
            source: "inbound",
            id: row.get(0)?,
            seq: row.get(1)?,
            kind: row.get(2)?,
            timestamp: row.get(3)?,
            status: row.get(4)?,
            trigger: row.get(5)?,
            platform_id: row.get(6)?,
            channel_type: row.get(7)?,
            thread_id: row.get(8)?,
            content: row.get(9)?,
            source_session_id: row.get(10)?,
            on_wake: row.get(11)?,
            in_reply_to: None,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CaptureError::from)
}

pub(crate) fn nanoclaw_outbound_messages(path: &Path) -> Result<Vec<NanoClawMessageRow>> {
    let conn = open_provider_sqlite_readonly(path)?;
    if !sqlite_table_exists(&conn, "messages_out")? {
        return Ok(Vec::new());
    }
    let columns = sqlite_table_columns(&conn, "messages_out")?;
    ensure_sqlite_table_columns(&columns, "NanoClaw outbound messages table", &["id"])?;
    let seq = optional_column_expr(&columns, "seq", "NULL");
    let kind = optional_column_expr(&columns, "kind", "NULL");
    let timestamp = optional_column_expr(&columns, "timestamp", "NULL");
    let in_reply_to = optional_column_expr(&columns, "in_reply_to", "NULL");
    let platform_id = optional_column_expr(&columns, "platform_id", "NULL");
    let channel_type = optional_column_expr(&columns, "channel_type", "NULL");
    let thread_id = optional_column_expr(&columns, "thread_id", "NULL");
    let content = optional_column_expr(&columns, "content", "NULL");
    let sql = format!(
        "select id, {seq}, {kind}, {timestamp}, {in_reply_to}, {platform_id}, \
         {channel_type}, {thread_id}, {content} from messages_out order by {seq}, id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok(NanoClawMessageRow {
            source: "outbound",
            id: row.get(0)?,
            seq: row.get(1)?,
            kind: row.get(2)?,
            timestamp: row.get(3)?,
            in_reply_to: row.get(4)?,
            platform_id: row.get(5)?,
            channel_type: row.get(6)?,
            thread_id: row.get(7)?,
            content: row.get(8)?,
            status: None,
            trigger: None,
            source_session_id: None,
            on_wake: None,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CaptureError::from)
}
