use std::{collections::BTreeMap, path::Path};

use chrono::{DateTime, Duration, Utc};
use ctx_history_core::{
    AgentType, CaptureProvider, EventRole, EventType, Fidelity, ProviderCaptureEnvelope,
    ProviderEventEnvelope, ProviderSourceTrust, RedactionState,
};
use rusqlite::Connection;
use serde_json::json;

use crate::provider::native::{
    native_provider_capture, open_provider_sqlite_readonly, provider_capped_json,
    provider_json_text, provider_line_from_index, provider_local_preview,
    provider_timestamp_seconds, text_id_index, NativeSessionDraft,
};
use crate::provider::sqlite::{
    ensure_sqlite_table_columns, opencode_schema_fingerprint, sqlite_table_columns,
    sqlite_table_exists,
};
use crate::{
    CaptureError, ProviderAdapterContext, ProviderNormalizationResult, Result,
    LINGMA_SQLITE_SOURCE_FORMAT, PROVIDER_MAX_PREVIEW_CHARS, PROVIDER_MAX_TEXT_CHARS,
};

pub(crate) struct LingmaChatRecordRow {
    pub(crate) rowid: i64,
    pub(crate) session_id: String,
    pub(crate) request_id: Option<String>,
    pub(crate) chat_prompt: String,
    pub(crate) summary: Option<String>,
    pub(crate) error_result: Option<String>,
    pub(crate) gmt_create: Option<i64>,
    pub(crate) extra: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct LingmaSessionInfo {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) started_at: DateTime<Utc>,
    pub(crate) ended_at: Option<DateTime<Utc>>,
    pub(crate) row_count: usize,
}

pub(crate) fn normalize_lingma_sqlite(
    path: &Path,
    context: &ProviderAdapterContext,
) -> Result<ProviderNormalizationResult> {
    let conn = open_provider_sqlite_readonly(path)?;
    let user_version: i64 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let schema_fingerprint = opencode_schema_fingerprint(&conn)?;
    let rows = lingma_chat_records(&conn)?;
    let raw_source_path = path.display().to_string();
    let sessions = lingma_session_infos(&rows, context.imported_at);
    let mut result = ProviderNormalizationResult::default();

    for row in rows {
        let Some(session) = sessions.get(&row.session_id) else {
            continue;
        };
        let occurred_at = lingma_timestamp(row.gmt_create, context.imported_at);
        let base_index = lingma_event_base_index(&row);
        let user_event = lingma_event(
            &row,
            LingmaEventDraft {
                provider_event_index: base_index,
                role: EventRole::User,
                event_type: EventType::Message,
                occurred_at,
                text: row.chat_prompt.clone(),
                body_kind: "chat_prompt",
                fidelity: Fidelity::Imported,
            },
        );
        result.captures.push((
            provider_line_from_index(base_index),
            lingma_capture(
                session,
                LingmaCaptureContext {
                    raw_source_path: &raw_source_path,
                    user_version,
                    schema_fingerprint: &schema_fingerprint,
                    event: Some(user_event),
                },
                context,
            ),
        ));

        if let Some((assistant_text, body_kind, event_type)) = lingma_assistant_text(&row) {
            let assistant_index = base_index.saturating_add(1);
            let assistant_event = lingma_event(
                &row,
                LingmaEventDraft {
                    provider_event_index: assistant_index,
                    role: EventRole::Assistant,
                    event_type,
                    occurred_at: occurred_at
                        .checked_add_signed(Duration::milliseconds(100))
                        .unwrap_or(occurred_at),
                    text: assistant_text,
                    body_kind,
                    fidelity: Fidelity::SummaryOnly,
                },
            );
            result.captures.push((
                provider_line_from_index(assistant_index),
                lingma_capture(
                    session,
                    LingmaCaptureContext {
                        raw_source_path: &raw_source_path,
                        user_version,
                        schema_fingerprint: &schema_fingerprint,
                        event: Some(assistant_event),
                    },
                    context,
                ),
            ));
        }
    }

    Ok(result)
}

pub(crate) struct LingmaCaptureContext<'a> {
    pub(crate) raw_source_path: &'a str,
    pub(crate) user_version: i64,
    pub(crate) schema_fingerprint: &'a str,
    pub(crate) event: Option<ProviderEventEnvelope>,
}

pub(crate) fn lingma_capture(
    session: &LingmaSessionInfo,
    draft: LingmaCaptureContext<'_>,
    context: &ProviderAdapterContext,
) -> ProviderCaptureEnvelope {
    native_provider_capture(
        NativeSessionDraft {
            provider: CaptureProvider::Lingma,
            source_format: LINGMA_SQLITE_SOURCE_FORMAT,
            provider_session_id: session.id.clone(),
            parent_provider_session_id: None,
            root_provider_session_id: None,
            external_agent_id: None,
            agent_type: AgentType::Primary,
            role_hint: Some("primary".to_owned()),
            is_primary: true,
            started_at: session.started_at,
            ended_at: session.ended_at,
            cwd: None,
            fidelity: Fidelity::Partial,
            raw_source_path: draft.raw_source_path.to_owned(),
            trust: ProviderSourceTrust::ProviderNative,
            source_metadata: json!({
                "adapter": LINGMA_SQLITE_SOURCE_FORMAT,
                "sqlite_user_version": draft.user_version,
                "schema_fingerprint": draft.schema_fingerprint,
                "source_path": draft.raw_source_path,
                "source_table": "chat_record",
                "source_fidelity": "user prompts plus assistant summaries/errors",
                "assistant_content_caveat": "WayLog labels Lingma as summaries-only; original assistant answers may be encrypted, transformed, or unavailable in this DB."
            }),
            session_metadata: json!({
                "source_format": LINGMA_SQLITE_SOURCE_FORMAT,
                "session_id": session.id,
                "title": session.title,
                "row_count": session.row_count,
                "source_table": "chat_record",
                "source_fidelity": "partial",
                "assistant_content_caveat": "assistant events imported from summary/error_result, not guaranteed full assistant message bodies"
            }),
        },
        context,
        draft.event,
    )
}

pub(crate) struct LingmaEventDraft {
    pub(crate) provider_event_index: u64,
    pub(crate) role: EventRole,
    pub(crate) event_type: EventType,
    pub(crate) occurred_at: DateTime<Utc>,
    pub(crate) text: String,
    pub(crate) body_kind: &'static str,
    pub(crate) fidelity: Fidelity,
}

pub(crate) fn lingma_event(
    row: &LingmaChatRecordRow,
    draft: LingmaEventDraft,
) -> ProviderEventEnvelope {
    let (text, truncated) = provider_local_preview(&draft.text, PROVIDER_MAX_TEXT_CHARS);
    let role_name = match draft.role {
        EventRole::User => "user",
        EventRole::Assistant => "assistant",
        EventRole::System => "system",
        EventRole::Tool => "tool",
        EventRole::Unknown => "unknown",
    };
    ProviderEventEnvelope {
        provider_event_index: draft.provider_event_index,
        provider_event_hash: Some(format!(
            "{}:{}:{role_name}",
            row.session_id,
            lingma_request_identity(row)
        )),
        cursor: Some(format!(
            "chat_record:{}:rowid:{}:{role_name}",
            row.session_id, row.rowid
        )),
        event_type: draft.event_type,
        role: Some(draft.role),
        occurred_at: draft.occurred_at,
        fidelity: draft.fidelity,
        redaction_state: RedactionState::LocalPreview,
        idempotency_key: Some(format!(
            "provider-event:{}:{}:{}",
            CaptureProvider::Lingma.as_str(),
            row.session_id,
            draft.provider_event_index
        )),
        artifacts: Vec::new(),
        payload: json!({
            "text": text,
            "truncated": truncated,
            "source_format": LINGMA_SQLITE_SOURCE_FORMAT,
            "body": provider_capped_json(
                &json!({
                    "rowid": row.rowid,
                    "session_id": row.session_id,
                    "request_id": row.request_id,
                    "role": role_name,
                    "body_kind": draft.body_kind,
                    "chat_prompt": row.chat_prompt,
                    "summary": row.summary,
                    "error_result": row.error_result,
                    "gmt_create": row.gmt_create,
                    "extra": row.extra.as_deref().map(provider_json_text),
                }),
                PROVIDER_MAX_PREVIEW_CHARS,
            ),
        }),
        metadata: json!({
            "source": "lingma_chat_record",
            "source_format": LINGMA_SQLITE_SOURCE_FORMAT,
            "rowid": row.rowid,
            "session_id": row.session_id,
            "request_id": row.request_id,
            "body_kind": draft.body_kind,
            "gmt_create": row.gmt_create,
            "content_fidelity": if draft.fidelity == Fidelity::SummaryOnly { "summary_only" } else { "imported" },
            "assistant_content_caveat": if draft.role == EventRole::Assistant {
                Some("summary/error_result only; original assistant body may be encrypted or unavailable")
            } else {
                None
            },
        }),
    }
}

pub(crate) fn lingma_chat_records(conn: &Connection) -> Result<Vec<LingmaChatRecordRow>> {
    if !sqlite_table_exists(conn, "chat_record")? {
        return Err(CaptureError::InvalidPayload(
            "Lingma local.db is missing required chat_record table".into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "chat_record")?;
    ensure_sqlite_table_columns(
        &columns,
        "Lingma chat_record table",
        &[
            "session_id",
            "request_id",
            "chat_prompt",
            "summary",
            "error_result",
            "gmt_create",
            "extra",
        ],
    )?;
    let mut stmt = conn.prepare(
        "select rowid, CAST(session_id AS TEXT), CAST(request_id AS TEXT), \
         CAST(chat_prompt AS TEXT), CAST(summary AS TEXT), CAST(error_result AS TEXT), \
         CAST(gmt_create AS INTEGER), CAST(extra AS TEXT) \
         from chat_record \
         where chat_prompt is not null and trim(CAST(chat_prompt AS TEXT)) != '' \
         order by CAST(gmt_create AS INTEGER), rowid",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(LingmaChatRecordRow {
            rowid: row.get(0)?,
            session_id: row.get(1)?,
            request_id: row.get(2)?,
            chat_prompt: row.get(3)?,
            summary: row.get(4)?,
            error_result: row.get(5)?,
            gmt_create: row.get(6)?,
            extra: row.get(7)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(CaptureError::from)
}

pub(crate) fn lingma_session_infos(
    rows: &[LingmaChatRecordRow],
    fallback: DateTime<Utc>,
) -> BTreeMap<String, LingmaSessionInfo> {
    let mut sessions = BTreeMap::<String, LingmaSessionInfo>::new();
    for row in rows {
        let occurred_at = lingma_timestamp(row.gmt_create, fallback);
        sessions
            .entry(row.session_id.clone())
            .and_modify(|session| {
                if occurred_at < session.started_at {
                    session.started_at = occurred_at;
                }
                let row_end = occurred_at
                    .checked_add_signed(Duration::milliseconds(100))
                    .unwrap_or(occurred_at);
                session.ended_at = Some(session.ended_at.unwrap_or(row_end).max(row_end));
                session.row_count = session.row_count.saturating_add(1);
            })
            .or_insert_with(|| LingmaSessionInfo {
                id: row.session_id.clone(),
                title: lingma_title(&row.chat_prompt),
                started_at: occurred_at,
                ended_at: occurred_at.checked_add_signed(Duration::milliseconds(100)),
                row_count: 1,
            });
    }
    sessions
}

pub(crate) fn lingma_event_base_index(row: &LingmaChatRecordRow) -> u64 {
    let rowid = u64::try_from(row.rowid).unwrap_or_else(|_| text_id_index(&row.session_id, 0));
    rowid.saturating_sub(1).saturating_mul(2)
}

pub(crate) fn lingma_timestamp(raw: Option<i64>, fallback: DateTime<Utc>) -> DateTime<Utc> {
    raw.map(|timestamp| provider_timestamp_seconds(Some(timestamp as f64), fallback))
        .unwrap_or(fallback)
}

pub(crate) fn lingma_title(prompt: &str) -> String {
    let trimmed = prompt.trim();
    let title = trimmed.chars().take(50).collect::<String>();
    if title.is_empty() {
        "Lingma chat".to_owned()
    } else {
        title
    }
}

pub(crate) fn lingma_assistant_text(
    row: &LingmaChatRecordRow,
) -> Option<(String, &'static str, EventType)> {
    if let Some(summary) = row
        .summary
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        return Some((summary.to_owned(), "summary", EventType::Message));
    }
    row.error_result
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty() && *text != "{}")
        .map(|error| {
            (
                format!("Lingma error result: {error}"),
                "error_result",
                EventType::Notice,
            )
        })
}

pub(crate) fn lingma_request_identity(row: &LingmaChatRecordRow) -> String {
    row.request_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("rowid-{}", row.rowid))
}
