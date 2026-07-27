use std::{collections::BTreeSet, path::Path};

use rusqlite::{Connection, OptionalExtension};

use crate::provider::sqlite::{
    ensure_sqlite_table_columns, optional_text_column_expr, optional_timestamp_millis_expr,
    sqlite_table_columns, sqlite_table_exists, ProviderSqliteSourceSnapshot,
    SqliteLengthPreflightGuard,
};
use crate::{CaptureError, Result};

use super::model::{ConversationRow, LegacyOrderKey, PlatformMessageRow};
use super::{ASTRBOT_CAPTURE_REVISION, ASTRBOT_POLICY_REVISION};

pub(super) fn astrbot_source_snapshot(path: &Path) -> Result<ProviderSqliteSourceSnapshot> {
    ProviderSqliteSourceSnapshot::read(
        path,
        "AstrBot SQLite source must be a regular non-symlink file",
        "AstrBot SQLite sidecar must be a regular non-symlink file",
    )
}

pub(super) fn astrbot_source_revision(
    snapshot: &ProviderSqliteSourceSnapshot,
    user_version: i64,
    schema_fingerprint: &str,
) -> String {
    format!(
        "astrbot-sqlite-snapshot-v1:capture={ASTRBOT_CAPTURE_REVISION};policy={ASTRBOT_POLICY_REVISION};user_version={user_version};schema={schema_fingerprint};{}",
        snapshot.revision_component(),
    )
}

pub(super) struct AstrBotSql {
    pub(super) conversation_candidate_initial: String,
    pub(super) conversation_candidate_after: String,
    pub(super) conversation_hydration: String,
    pub(super) platform_message_candidate_initial: Option<String>,
    pub(super) platform_message_candidate_after: Option<String>,
    pub(super) platform_message_hydration: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct RowCandidate {
    pub(super) physical_rowid: i64,
    pub(super) retained_bytes: i64,
    pub(super) legacy_order: LegacyOrderKey,
}

impl RowCandidate {
    pub(super) fn observed_bytes(self) -> Result<u64> {
        u64::try_from(self.retained_bytes).map_err(|_| {
            CaptureError::InvalidPayload(
                "AstrBot retained SQLite byte count must be nonnegative".to_owned(),
            )
        })
    }
}

impl AstrBotSql {
    pub(super) fn new(conn: &Connection) -> Result<Self> {
        let conversation_columns = astrbot_conversation_columns(conn)?;
        let conversation_projection = astrbot_conversation_projection(&conversation_columns);
        let conversation_cte = format!(
            "with projected(physical_rowid, row_id, inner_conversation_id, conversation_id, \
             platform_id, user_id, content, title, persona_id, token_usage, created_at, \
             updated_at) as (select rowid, {conversation_projection} from conversations)"
        );
        let conversation_retained = astrbot_retained_length_expr(&[
            "row_id",
            "inner_conversation_id",
            "conversation_id",
            "platform_id",
            "user_id",
            "content",
            "title",
            "persona_id",
            "token_usage",
            "created_at",
            "updated_at",
        ]);
        let conversation_candidate_initial = format!(
            "{conversation_cte} select p.physical_rowid, {conversation_retained}, \
             p.created_at, p.row_id \
             from projected p order by p.physical_rowid limit 1"
        );
        let conversation_candidate_after = format!(
            "{conversation_cte} select p.physical_rowid, {conversation_retained}, \
             p.created_at, p.row_id \
             from projected p where p.physical_rowid > ?1 \
             order by p.physical_rowid limit 1"
        );
        let conversation_hydration = format!(
            "{conversation_cte} select row_id, inner_conversation_id, conversation_id, \
             platform_id, user_id, content, title, persona_id, token_usage, created_at, \
             updated_at from projected where physical_rowid = ?1"
        );
        let (
            platform_message_candidate_initial,
            platform_message_candidate_after,
            platform_message_hydration,
        ) = if sqlite_table_exists(conn, "platform_message_history")? {
            let columns = sqlite_table_columns(conn, "platform_message_history")?;
            let projection = astrbot_platform_message_projection(&columns);
            let cte = format!(
                "with projected(physical_rowid, id, platform_id, user_id, sender_id, \
                     sender_name, content, llm_checkpoint_id, created_at) as ( \
                         select rowid, {projection} from platform_message_history \
                     )"
            );
            let retained = astrbot_retained_length_expr(&[
                "id",
                "platform_id",
                "user_id",
                "sender_id",
                "sender_name",
                "content",
                "llm_checkpoint_id",
                "created_at",
            ]);
            (
                Some(format!(
                    "{cte} select p.physical_rowid, {retained}, p.created_at, p.id \
                         from projected p \
                         order by p.physical_rowid limit 1"
                )),
                Some(format!(
                    "{cte} select p.physical_rowid, {retained}, p.created_at, p.id \
                         from projected p \
                         where p.physical_rowid > ?1 order by p.physical_rowid limit 1"
                )),
                Some(format!(
                    "{cte} select id, platform_id, user_id, sender_id, sender_name, \
                         content, llm_checkpoint_id, created_at from projected \
                         where physical_rowid = ?1"
                )),
            )
        } else {
            (None, None, None)
        };

        Ok(Self {
            conversation_candidate_initial,
            conversation_candidate_after,
            conversation_hydration,
            platform_message_candidate_initial,
            platform_message_candidate_after,
            platform_message_hydration,
        })
    }
}

pub(super) fn astrbot_conversation_columns(conn: &Connection) -> Result<BTreeSet<String>> {
    if !sqlite_table_exists(conn, "conversations")? {
        return Err(CaptureError::InvalidPayload(
            "AstrBot data_v4.db is missing required conversations table".into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "conversations")?;
    ensure_sqlite_table_columns(&columns, "AstrBot conversations table", &["content"])?;
    Ok(columns)
}

fn astrbot_conversation_projection(columns: &BTreeSet<String>) -> String {
    let row_id = if columns.contains("id") {
        "id"
    } else {
        "rowid"
    };
    let inner_conversation_id = optional_text_column_expr(columns, "inner_conversation_id", "NULL");
    let conversation_id = if columns.contains("conversation_id") {
        "CAST(conversation_id AS TEXT)".to_owned()
    } else if columns.contains("inner_conversation_id") {
        "CAST(inner_conversation_id AS TEXT)".to_owned()
    } else {
        "CAST(rowid AS TEXT)".to_owned()
    };
    let platform_id = optional_text_column_expr(columns, "platform_id", "NULL");
    let user_id = optional_text_column_expr(columns, "user_id", "NULL");
    let title = optional_text_column_expr(columns, "title", "NULL");
    let persona_id = optional_text_column_expr(columns, "persona_id", "NULL");
    let token_usage = optional_text_column_expr(columns, "token_usage", "NULL");
    let created_at = optional_timestamp_millis_expr(columns, "created_at", "NULL");
    let updated_at = optional_timestamp_millis_expr(columns, "updated_at", "NULL");
    format!(
        "{row_id}, {inner_conversation_id}, {conversation_id}, {platform_id}, {user_id}, \
         content, {title}, {persona_id}, {token_usage}, {created_at}, {updated_at}"
    )
}

fn astrbot_platform_message_projection(columns: &BTreeSet<String>) -> String {
    let id = if columns.contains("id") {
        "id"
    } else {
        "rowid"
    };
    let platform_id = optional_text_column_expr(columns, "platform_id", "NULL");
    let user_id = optional_text_column_expr(columns, "user_id", "NULL");
    let sender_id = optional_text_column_expr(columns, "sender_id", "NULL");
    let sender_name = optional_text_column_expr(columns, "sender_name", "NULL");
    let content = optional_text_column_expr(columns, "content", "NULL");
    let llm_checkpoint_id = optional_text_column_expr(columns, "llm_checkpoint_id", "NULL");
    let created_at = optional_timestamp_millis_expr(columns, "created_at", "NULL");
    format!(
        "{id}, {platform_id}, {user_id}, {sender_id}, {sender_name}, {content}, \
         {llm_checkpoint_id}, {created_at}"
    )
}

fn astrbot_retained_length_expr(columns: &[&str]) -> String {
    // Keep the size probe on the source column: octet_length() can read the encoded byte count
    // lazily, while casting an oversize value to BLOB can trip SQLite's length limit first.
    columns
        .iter()
        .map(|column| format!("coalesce(octet_length(p.{column}), 0)"))
        .collect::<Vec<_>>()
        .join(" + ")
}

pub(super) fn with_astrbot_length_preflight<T>(
    conn: &Connection,
    query: impl FnOnce() -> rusqlite::Result<T>,
) -> Result<T> {
    // SQLITE_LIMIT_LENGTH can reject even integer-only octet_length inspection of an oversized
    // stored value. AstrBot candidate/setup queries return only rowids, order keys, and byte
    // counts, so lift the limit only around metadata preflight and restore it before hydration.
    let _guard = SqliteLengthPreflightGuard::new(conn);
    query().map_err(CaptureError::from)
}

pub(super) fn fetch_candidate(
    conn: &Connection,
    initial_sql: &str,
    after_sql: &str,
    after_rowid: Option<i64>,
) -> Result<Option<RowCandidate>> {
    let map_row = |row: &rusqlite::Row<'_>| {
        let physical_rowid = row.get(0)?;
        let timestamp = row.get::<_, Option<i64>>(2)?;
        Ok(RowCandidate {
            physical_rowid,
            retained_bytes: row.get(1)?,
            legacy_order: LegacyOrderKey {
                timestamp_is_present: timestamp.is_some(),
                timestamp: timestamp.unwrap_or(0),
                logical_id: row.get(3)?,
                physical_rowid,
            },
        })
    };
    with_astrbot_length_preflight(conn, || {
        match after_rowid {
            Some(rowid) => conn.query_row(after_sql, [rowid], map_row),
            None => conn.query_row(initial_sql, [], map_row),
        }
        .optional()
    })
}

pub(super) fn hydrate_conversation(
    conn: &Connection,
    sql: &str,
    physical_rowid: i64,
) -> Result<ConversationRow> {
    conn.query_row(sql, [physical_rowid], |row| {
        Ok(ConversationRow {
            row_id: row.get(0)?,
            inner_conversation_id: row.get(1)?,
            conversation_id: row.get(2)?,
            platform_id: row.get(3)?,
            user_id: row.get(4)?,
            content: row.get(5)?,
            title: row.get(6)?,
            persona_id: row.get(7)?,
            token_usage: row.get(8)?,
            created_at: row.get(9)?,
            updated_at: row.get(10)?,
        })
    })
    .map_err(CaptureError::from)
}

pub(super) fn hydrate_platform_message(
    conn: &Connection,
    sql: &str,
    physical_rowid: i64,
) -> Result<PlatformMessageRow> {
    conn.query_row(sql, [physical_rowid], |row| {
        Ok(PlatformMessageRow {
            id: row.get(0)?,
            platform_id: row.get(1)?,
            user_id: row.get(2)?,
            sender_id: row.get(3)?,
            sender_name: row.get(4)?,
            content: row.get(5)?,
            llm_checkpoint_id: row.get(6)?,
            created_at: row.get(7)?,
        })
    })
    .map_err(CaptureError::from)
}
