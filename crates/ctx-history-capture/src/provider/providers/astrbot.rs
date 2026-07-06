use std::{collections::BTreeMap, path::Path};

use chrono::{DateTime, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventRole, EventType, Fidelity, ProviderCaptureEnvelope,
    ProviderEventEnvelope, ProviderSourceTrust,
};
use rusqlite::{Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::provider::custom_history_jsonl::push_provider_import_failure;
use crate::provider::native::{
    native_event, native_provider_capture, open_provider_sqlite_readonly, provider_json_text,
    provider_line_from_index, provider_nonnegative_i64_to_u64, provider_role,
    provider_timestamp_millis, provider_value_text, NativeEventDraft, NativeSessionDraft,
};
use crate::provider::sqlite::{
    ensure_sqlite_table_columns, opencode_schema_fingerprint, optional_column_expr,
    sqlite_table_columns, sqlite_table_exists,
};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderNormalizationResult, Result,
    ASTRBOT_SQLITE_SOURCE_FORMAT,
};

pub(crate) struct AstrBotConversationRow {
    pub(crate) row_id: i64,
    pub(crate) inner_conversation_id: Option<String>,
    pub(crate) conversation_id: String,
    pub(crate) platform_id: Option<String>,
    pub(crate) user_id: Option<String>,
    pub(crate) content: String,
    pub(crate) title: Option<String>,
    pub(crate) persona_id: Option<String>,
    pub(crate) token_usage: Option<String>,
    pub(crate) created_at: Option<i64>,
    pub(crate) updated_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub(crate) struct AstrBotPlatformMessageRow {
    pub(crate) id: i64,
    pub(crate) platform_id: Option<String>,
    pub(crate) user_id: Option<String>,
    pub(crate) sender_id: Option<String>,
    pub(crate) sender_name: Option<String>,
    pub(crate) content: Option<String>,
    pub(crate) llm_checkpoint_id: Option<String>,
    pub(crate) created_at: Option<i64>,
}

pub(crate) fn normalize_astrbot_sqlite(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    let conn = open_provider_sqlite_readonly(path)?;
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let schema_fingerprint = opencode_schema_fingerprint(&conn)?;
    let conversations = astrbot_conversations(&conn)?;
    let platform_messages = astrbot_platform_messages(&conn)?;
    let selected_conversation = astrbot_selected_conversation(&conn).ok().flatten();
    let mut result = ProviderNormalizationResult::default();
    let mut checkpoint_sessions = BTreeMap::<String, String>::new();

    for conversation in &conversations {
        let conversation_line = match provider_nonnegative_i64_to_u64(
            conversation.row_id,
            "AstrBot conversation row id",
        ) {
            Ok(value) => provider_line_from_index(value),
            Err(err) => {
                push_provider_import_failure(&mut result.summary, 0, err.to_string());
                continue;
            }
        };
        let provider_session_id = astrbot_provider_session_id(conversation);
        let started_at = provider_timestamp_millis(conversation.created_at, context.imported_at);
        let ended_at = conversation
            .updated_at
            .map(|timestamp| provider_timestamp_millis(Some(timestamp), context.imported_at));
        let content = provider_json_text(&conversation.content);
        if let Value::Array(items) = &content {
            for (index, item) in items.iter().enumerate() {
                if let Some(checkpoint) = astrbot_checkpoint_id(item) {
                    checkpoint_sessions.insert(checkpoint, provider_session_id.clone());
                    continue;
                }
                let role = astrbot_role(item);
                let text = astrbot_item_text(item)
                    .unwrap_or_else(|| "AstrBot conversation item".to_owned());
                let event = native_event(NativeEventDraft {
                    provider: CaptureProvider::AstrBot,
                    source_format: ASTRBOT_SQLITE_SOURCE_FORMAT,
                    provider_session_id: provider_session_id.clone(),
                    provider_event_index: index as u64,
                    provider_event_hash: astrbot_item_id(item)
                        .map(|id| format!("conversation:{id}")),
                    cursor: format!("conversation:{}:item:{index}", conversation.conversation_id),
                    event_type: EventType::Message,
                    role,
                    occurred_at: started_at,
                    text,
                    body: item.clone(),
                    metadata: json!({
                        "source": "astrbot_conversations",
                        "source_format": ASTRBOT_SQLITE_SOURCE_FORMAT,
                        "conversation_id": conversation.conversation_id,
                        "inner_conversation_id": conversation.inner_conversation_id,
                        "item_index": index,
                    }),
                });
                result.captures.push((
                    index + 1,
                    astrbot_capture(
                        AstrBotCaptureDraft {
                            conversation,
                            provider_session_id: &provider_session_id,
                            started_at,
                            ended_at,
                            path,
                            user_version,
                            schema_fingerprint: &schema_fingerprint,
                            selected_conversation: selected_conversation.as_deref(),
                            event: Some(event),
                        },
                        context,
                    ),
                ));
            }
        } else {
            let text =
                provider_value_text(&content).unwrap_or_else(|| "AstrBot conversation".to_owned());
            let event = native_event(NativeEventDraft {
                provider: CaptureProvider::AstrBot,
                source_format: ASTRBOT_SQLITE_SOURCE_FORMAT,
                provider_session_id: provider_session_id.clone(),
                provider_event_index: 0,
                provider_event_hash: Some(format!("conversation-row:{}", conversation.row_id)),
                cursor: format!("conversation:{}:content", conversation.conversation_id),
                event_type: EventType::Message,
                role: None,
                occurred_at: started_at,
                text,
                body: content.clone(),
                metadata: json!({
                    "source": "astrbot_conversations",
                    "source_format": ASTRBOT_SQLITE_SOURCE_FORMAT,
                    "conversation_id": conversation.conversation_id,
                }),
            });
            result.captures.push((
                conversation_line,
                astrbot_capture(
                    AstrBotCaptureDraft {
                        conversation,
                        provider_session_id: &provider_session_id,
                        started_at,
                        ended_at,
                        path,
                        user_version,
                        schema_fingerprint: &schema_fingerprint,
                        selected_conversation: selected_conversation.as_deref(),
                        event: Some(event),
                    },
                    context,
                ),
            ));
        }
    }

    let conversations_by_id = conversations
        .iter()
        .map(|conversation| (astrbot_provider_session_id(conversation), conversation))
        .collect::<BTreeMap<_, _>>();
    for message in platform_messages {
        let message_id =
            match provider_nonnegative_i64_to_u64(message.id, "AstrBot platform message id") {
                Ok(value) => value,
                Err(err) => {
                    push_provider_import_failure(&mut result.summary, 0, err.to_string());
                    continue;
                }
            };
        let provider_session_id = message
            .llm_checkpoint_id
            .as_ref()
            .and_then(|checkpoint| checkpoint_sessions.get(checkpoint))
            .cloned()
            .unwrap_or_else(|| {
                format!(
                    "platform/{}/{}",
                    message.platform_id.as_deref().unwrap_or("unknown"),
                    message.user_id.as_deref().unwrap_or("unknown")
                )
            });
        let conversation = conversations_by_id.get(&provider_session_id).copied();
        let started_at = conversation
            .and_then(|conversation| conversation.created_at)
            .map(|timestamp| provider_timestamp_millis(Some(timestamp), context.imported_at))
            .unwrap_or_else(|| provider_timestamp_millis(message.created_at, context.imported_at));
        let content = message
            .content
            .as_deref()
            .map(provider_json_text)
            .unwrap_or(Value::Null);
        let text =
            provider_value_text(&content).unwrap_or_else(|| "AstrBot platform message".to_owned());
        let role = if message.sender_id.as_deref() == message.user_id.as_deref() {
            Some(EventRole::User)
        } else {
            Some(EventRole::Assistant)
        };
        let event_index = 1_000_000u64.saturating_add(message_id);
        let event = native_event(NativeEventDraft {
            provider: CaptureProvider::AstrBot,
            source_format: ASTRBOT_SQLITE_SOURCE_FORMAT,
            provider_session_id: provider_session_id.clone(),
            provider_event_index: event_index,
            provider_event_hash: Some(format!("platform-message:{}", message.id)),
            cursor: format!("platform_message_history:id:{}", message.id),
            event_type: EventType::Message,
            role,
            occurred_at: provider_timestamp_millis(message.created_at, started_at),
            text,
            body: json!({
                "message_id": message.id,
                "platform_id": message.platform_id,
                "user_id": message.user_id,
                "sender_id": message.sender_id,
                "sender_name": message.sender_name,
                "content": content,
                "llm_checkpoint_id": message.llm_checkpoint_id,
            }),
            metadata: json!({
                "source": "astrbot_platform_message_history",
                "source_format": ASTRBOT_SQLITE_SOURCE_FORMAT,
                "message_id": message.id,
            }),
        });
        if let Some(conversation) = conversation {
            result.captures.push((
                event_index.min(usize::MAX as u64) as usize,
                astrbot_capture(
                    AstrBotCaptureDraft {
                        conversation,
                        provider_session_id: &provider_session_id,
                        started_at,
                        ended_at: conversation.updated_at.map(|timestamp| {
                            provider_timestamp_millis(Some(timestamp), context.imported_at)
                        }),
                        path,
                        user_version,
                        schema_fingerprint: &schema_fingerprint,
                        selected_conversation: selected_conversation.as_deref(),
                        event: Some(event),
                    },
                    context,
                ),
            ));
        } else {
            result.captures.push((
                event_index.min(usize::MAX as u64) as usize,
                native_provider_capture(
                    NativeSessionDraft {
                        provider: CaptureProvider::AstrBot,
                        source_format: ASTRBOT_SQLITE_SOURCE_FORMAT,
                        provider_session_id: provider_session_id.clone(),
                        parent_provider_session_id: None,
                        root_provider_session_id: None,
                        external_agent_id: message.platform_id.clone(),
                        agent_type: AgentType::Primary,
                        role_hint: Some("platform-history".to_owned()),
                        is_primary: true,
                        started_at,
                        ended_at: None,
                        cwd: None,
                        fidelity: Fidelity::Partial,
                        raw_source_path: path.display().to_string(),
                        trust: ProviderSourceTrust::ProviderNative,
                        source_metadata: json!({
                            "adapter": ASTRBOT_SQLITE_SOURCE_FORMAT,
                            "sqlite_user_version": user_version,
                            "schema_fingerprint": schema_fingerprint,
                            "support_level": "supported",
                        }),
                        session_metadata: json!({
                            "source_format": ASTRBOT_SQLITE_SOURCE_FORMAT,
                            "platform_id": message.platform_id,
                            "user_id": message.user_id,
                            "fidelity_gap": "platform history row was not linked to a conversations checkpoint",
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

pub(crate) fn astrbot_provider_session_id(conversation: &AstrBotConversationRow) -> String {
    conversation
        .inner_conversation_id
        .as_ref()
        .or(Some(&conversation.conversation_id))
        .cloned()
        .unwrap_or_else(|| format!("conversation-row-{}", conversation.row_id))
}

pub(crate) struct AstrBotCaptureDraft<'a> {
    pub(crate) conversation: &'a AstrBotConversationRow,
    pub(crate) provider_session_id: &'a str,
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) ended_at: Option<DateTime<Utc>>,
    pub(crate) path: &'a Path,
    pub(crate) user_version: i64,
    pub(crate) schema_fingerprint: &'a str,
    pub(crate) selected_conversation: Option<&'a str>,
    pub(crate) event: Option<ProviderEventEnvelope>,
}

pub(crate) fn astrbot_capture(
    draft: AstrBotCaptureDraft<'_>,
    context: &ProviderAdapterContext,
) -> ProviderCaptureEnvelope {
    let AstrBotCaptureDraft {
        conversation,
        provider_session_id,
        started_at,
        ended_at,
        path,
        user_version,
        schema_fingerprint,
        selected_conversation,
        event,
    } = draft;
    native_provider_capture(
        NativeSessionDraft {
            provider: CaptureProvider::AstrBot,
            source_format: ASTRBOT_SQLITE_SOURCE_FORMAT,
            provider_session_id: provider_session_id.to_owned(),
            parent_provider_session_id: None,
            root_provider_session_id: None,
            external_agent_id: conversation.platform_id.clone(),
            agent_type: AgentType::Primary,
            role_hint: Some("llm-context".to_owned()),
            is_primary: true,
            started_at,
            ended_at,
            cwd: None,
            fidelity: Fidelity::Partial,
            raw_source_path: path.display().to_string(),
            trust: ProviderSourceTrust::ProviderNative,
            source_metadata: json!({
                "adapter": ASTRBOT_SQLITE_SOURCE_FORMAT,
                "sqlite_user_version": user_version,
                "schema_fingerprint": schema_fingerprint,
                "support_level": "supported",
            }),
            session_metadata: json!({
                "source_format": ASTRBOT_SQLITE_SOURCE_FORMAT,
                "conversation_id": conversation.conversation_id,
                "inner_conversation_id": conversation.inner_conversation_id,
                "platform_id": conversation.platform_id,
                "user_id": conversation.user_id,
                "title": conversation.title,
                "persona_id": conversation.persona_id,
                "token_usage": conversation.token_usage.as_deref().map(provider_json_text),
                "selected_conversation": selected_conversation,
                "fidelity_gap": "The AstrBot importer reads local LLM context plus available platform history from data_v4.db; platform-native chats may still be partial when upstream stores non-LLM replies on the IM platform",
            }),
        },
        context,
        event,
    )
}

pub(crate) fn astrbot_item_id(item: &Value) -> Option<&str> {
    item.get("id")
        .or_else(|| item.get("message_id"))
        .or_else(|| item.get("checkpoint_id"))
        .and_then(Value::as_str)
}

pub(crate) fn astrbot_checkpoint_id(item: &Value) -> Option<String> {
    let item_type = item
        .get("type")
        .or_else(|| item.get("role"))
        .and_then(Value::as_str)?;
    if item_type != "_checkpoint" && item_type != "checkpoint" {
        return None;
    }
    astrbot_item_id(item).map(str::to_owned)
}

pub(crate) fn astrbot_role(item: &Value) -> Option<EventRole> {
    item.get("role")
        .or_else(|| item.get("type"))
        .and_then(Value::as_str)
        .map(|role| provider_role(Some(role)))
}

pub(crate) fn astrbot_item_text(item: &Value) -> Option<String> {
    item.get("content")
        .or_else(|| item.get("text"))
        .or_else(|| item.get("message"))
        .and_then(provider_value_text)
}

pub(crate) fn astrbot_conversations(conn: &Connection) -> Result<Vec<AstrBotConversationRow>> {
    if !sqlite_table_exists(conn, "conversations")? {
        return Err(CaptureError::InvalidPayload(
            "AstrBot data_v4.db is missing required conversations table".into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "conversations")?;
    ensure_sqlite_table_columns(&columns, "AstrBot conversations table", &["content"])?;
    let row_id = if columns.contains("id") {
        "id"
    } else {
        "rowid"
    };
    let inner_conversation_id = optional_column_expr(&columns, "inner_conversation_id", "NULL");
    let conversation_id = optional_column_expr(
        &columns,
        "conversation_id",
        optional_column_expr(&columns, "inner_conversation_id", "CAST(rowid AS TEXT)"),
    );
    let platform_id = optional_column_expr(&columns, "platform_id", "NULL");
    let user_id = optional_column_expr(&columns, "user_id", "NULL");
    let title = optional_column_expr(&columns, "title", "NULL");
    let persona_id = optional_column_expr(&columns, "persona_id", "NULL");
    let token_usage = optional_column_expr(&columns, "token_usage", "NULL");
    let created_at = optional_column_expr(&columns, "created_at", "NULL");
    let updated_at = optional_column_expr(&columns, "updated_at", "NULL");
    let sql = format!(
        "select {row_id}, {inner_conversation_id}, {conversation_id}, {platform_id}, \
         {user_id}, content, {title}, {persona_id}, {token_usage}, {created_at}, \
         {updated_at} from conversations order by {created_at}, {row_id}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok(AstrBotConversationRow {
            row_id: row.get(0)?,
            inner_conversation_id: row.get(1)?,
            conversation_id: row.get::<_, String>(2)?,
            platform_id: row.get(3)?,
            user_id: row.get(4)?,
            content: row.get(5)?,
            title: row.get(6)?,
            persona_id: row.get(7)?,
            token_usage: row.get(8)?,
            created_at: row.get(9)?,
            updated_at: row.get(10)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CaptureError::from)
}

pub(crate) fn astrbot_platform_messages(
    conn: &Connection,
) -> Result<Vec<AstrBotPlatformMessageRow>> {
    if !sqlite_table_exists(conn, "platform_message_history")? {
        return Ok(Vec::new());
    }
    let columns = sqlite_table_columns(conn, "platform_message_history")?;
    let id = if columns.contains("id") {
        "id"
    } else {
        "rowid"
    };
    let platform_id = optional_column_expr(&columns, "platform_id", "NULL");
    let user_id = optional_column_expr(&columns, "user_id", "NULL");
    let sender_id = optional_column_expr(&columns, "sender_id", "NULL");
    let sender_name = optional_column_expr(&columns, "sender_name", "NULL");
    let content = optional_column_expr(&columns, "content", "NULL");
    let llm_checkpoint_id = optional_column_expr(&columns, "llm_checkpoint_id", "NULL");
    let created_at = optional_column_expr(&columns, "created_at", "NULL");
    let sql = format!(
        "select {id}, {platform_id}, {user_id}, {sender_id}, {sender_name}, \
         {content}, {llm_checkpoint_id}, {created_at} from platform_message_history \
         order by {created_at}, {id}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok(AstrBotPlatformMessageRow {
            id: row.get(0)?,
            platform_id: row.get(1)?,
            user_id: row.get(2)?,
            sender_id: row.get(3)?,
            sender_name: row.get(4)?,
            content: row.get(5)?,
            llm_checkpoint_id: row.get(6)?,
            created_at: row.get(7)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CaptureError::from)
}

pub(crate) fn astrbot_selected_conversation(conn: &Connection) -> Result<Option<String>> {
    if !sqlite_table_exists(conn, "preferences")? {
        return Ok(None);
    }
    let columns = sqlite_table_columns(conn, "preferences")?;
    if !columns.contains("key") || !columns.contains("value") {
        return Ok(None);
    }
    let scope_filter = if columns.contains("scope") {
        "AND scope = 'umo'"
    } else {
        ""
    };
    let sql =
        format!("select value from preferences where key = 'sel_conv_id' {scope_filter} limit 1");
    let value = conn
        .query_row(&sql, [], |row| row.get::<_, Option<String>>(0))
        .optional()?
        .flatten();
    Ok(value)
}
