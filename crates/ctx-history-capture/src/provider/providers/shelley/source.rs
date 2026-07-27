use std::{collections::BTreeSet, path::Path};

use rusqlite::Connection;

use crate::provider::sqlite::{
    ensure_sqlite_table_columns, sqlite_table_columns, sqlite_table_exists,
    ProviderSqliteSourceSnapshot, SqliteLengthPreflightGuard,
};
use crate::{CaptureError, Result, MAX_PROVIDER_SQLITE_VALUE_BYTES};

use super::{
    SHELLEY_CAPTURE_REVISION, SHELLEY_POLICY_REVISION, SHELLEY_SQLITE_VALUE_OVERHEAD_BYTES,
};

pub(super) fn shelley_source_snapshot(path: &Path) -> Result<ProviderSqliteSourceSnapshot> {
    ProviderSqliteSourceSnapshot::read(
        path,
        "Shelley SQLite source must be a regular non-symlink file",
        "Shelley SQLite sidecar must be a regular non-symlink file",
    )
}

pub(super) fn shelley_source_revision(
    snapshot: &ProviderSqliteSourceSnapshot,
    user_version: i64,
    schema_fingerprint: &str,
) -> String {
    format!(
        "shelley-sqlite-snapshot-v1:capture={SHELLEY_CAPTURE_REVISION};policy={SHELLEY_POLICY_REVISION};user_version={user_version};schema={schema_fingerprint};{}",
        snapshot.revision_component(),
    )
}

pub(crate) fn shelley_conversation_columns(conn: &Connection) -> Result<BTreeSet<String>> {
    if !sqlite_table_exists(conn, "conversations")? {
        return Err(CaptureError::InvalidPayload(
            "Shelley shelley.db is missing required conversations table".into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "conversations")?;
    ensure_sqlite_table_columns(
        &columns,
        "Shelley conversations table",
        &["conversation_id"],
    )?;
    Ok(columns)
}

pub(super) fn shelley_has_conversations(conn: &Connection) -> Result<bool> {
    conn.query_row(
        "select exists (select 1 from conversations limit 1)",
        [],
        |row| row.get(0),
    )
    .map_err(CaptureError::from)
}

pub(crate) fn shelley_message_columns(conn: &Connection) -> Result<BTreeSet<String>> {
    if !sqlite_table_exists(conn, "messages")? {
        return Err(CaptureError::InvalidPayload(
            "Shelley shelley.db is missing required messages table".into(),
        ));
    }
    let columns = sqlite_table_columns(conn, "messages")?;
    ensure_sqlite_table_columns(
        &columns,
        "Shelley messages table",
        &["message_id", "conversation_id", "type"],
    )?;
    Ok(columns)
}

pub(super) fn shelley_require_message_index(conn: &Connection, has_sequence: bool) -> Result<()> {
    let mut indexes = conn.prepare(
        "select name from pragma_index_list('messages') where partial = 0 order by name",
    )?;
    let index_names = indexes
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut index_columns = conn.prepare(
        "select name, desc, coll from pragma_index_xinfo(?1)
         where key = 1 order by seqno",
    )?;
    // Migration 002 has the conversation-only index; migration 003 introduces sequence_id.
    let expected_columns: &[&str] = if has_sequence {
        &["conversation_id", "sequence_id"]
    } else {
        &["conversation_id"]
    };
    for index_name in index_names {
        let columns = index_columns
            .query_map([index_name], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let compatible = columns.len() == expected_columns.len()
            && columns
                .iter()
                .zip(expected_columns)
                .all(|((name, _, _), expected)| name.as_deref() == Some(*expected))
            && columns.iter().all(|(_, descending, collation)| {
                *descending == 0
                    && collation
                        .as_deref()
                        .is_some_and(|value| value.eq_ignore_ascii_case("binary"))
            });
        if compatible {
            return Ok(());
        }
    }
    let expected_index = expected_columns.join(", ");
    Err(CaptureError::InvalidPayload(
        format!(
            "Shelley messages table requires a non-partial ascending BINARY index on ({expected_index})"
        ),
    ))
}

pub(super) fn shelley_qualified_optional_column(
    columns: &BTreeSet<String>,
    alias: &str,
    column: &str,
    fallback: &str,
) -> String {
    if columns.contains(column) {
        format!("{alias}.{column}")
    } else {
        fallback.to_owned()
    }
}

pub(crate) fn shelley_conversation_select_expressions(
    columns: &BTreeSet<String>,
    alias: &str,
) -> Vec<String> {
    [
        format!("{alias}.rowid"),
        format!("{alias}.conversation_id"),
        shelley_qualified_optional_column(columns, alias, "slug", "NULL"),
        shelley_qualified_optional_column(columns, alias, "user_initiated", "1"),
        shelley_qualified_optional_column(columns, alias, "created_at", "NULL"),
        shelley_qualified_optional_column(columns, alias, "updated_at", "NULL"),
        shelley_qualified_optional_column(columns, alias, "cwd", "NULL"),
        shelley_qualified_optional_column(columns, alias, "archived", "0"),
        shelley_qualified_optional_column(columns, alias, "parent_conversation_id", "NULL"),
        shelley_qualified_optional_column(columns, alias, "model", "NULL"),
        shelley_qualified_optional_column(columns, alias, "conversation_options", "NULL"),
        shelley_qualified_optional_column(columns, alias, "current_generation", "NULL"),
        shelley_qualified_optional_column(columns, alias, "agent_working", "0"),
        shelley_qualified_optional_column(columns, alias, "tags", "NULL"),
        shelley_qualified_optional_column(columns, alias, "is_draft", "0"),
        shelley_qualified_optional_column(columns, alias, "draft", "NULL"),
        shelley_qualified_optional_column(columns, alias, "queued_messages", "NULL"),
    ]
    .into_iter()
    .collect()
}

pub(crate) fn shelley_message_select_expressions(
    columns: &BTreeSet<String>,
    alias: &str,
) -> Vec<String> {
    [
        format!("{alias}.rowid"),
        format!("{alias}.message_id"),
        format!("{alias}.conversation_id"),
        shelley_qualified_optional_column(columns, alias, "sequence_id", &format!("{alias}.rowid")),
        format!("{alias}.type"),
        shelley_qualified_optional_column(columns, alias, "llm_data", "NULL"),
        shelley_qualified_optional_column(columns, alias, "user_data", "NULL"),
        shelley_qualified_optional_column(columns, alias, "usage_data", "NULL"),
        shelley_qualified_optional_column(columns, alias, "created_at", "NULL"),
        shelley_qualified_optional_column(columns, alias, "display_data", "NULL"),
        shelley_qualified_optional_column(columns, alias, "excluded_from_context", "0"),
        shelley_qualified_optional_column(columns, alias, "generation", "NULL"),
        shelley_qualified_optional_column(columns, alias, "llm_api_url", "NULL"),
        shelley_qualified_optional_column(columns, alias, "model_name", "NULL"),
        shelley_qualified_optional_column(columns, alias, "forked_from_message_id", "NULL"),
    ]
    .into_iter()
    .collect()
}

pub(super) fn shelley_message_key_candidate_sql(after_rowid: bool) -> String {
    let predicate = if after_rowid {
        "where m.rowid > ?1"
    } else {
        ""
    };
    format!(
        "select m.rowid, coalesce(octet_length(m.conversation_id), 0),
                case when typeof(m.conversation_id) = 'text' then 1 else 0 end,
                case when not exists (
                    select 1 from messages later where later.rowid > m.rowid limit 1
                ) then 1 else 0 end
         from messages m
         {predicate}
         order by m.rowid limit 1"
    )
}

pub(super) fn shelley_message_candidate_sql(
    message_lengths: &str,
    after_rowid: bool,
    has_sequence: bool,
) -> String {
    let anchor_sequence = if has_sequence {
        "anchor.sequence_id as sequence_id,"
    } else {
        ""
    };
    let native_order = if has_sequence {
        "m.conversation_id, m.sequence_id, m.rowid"
    } else {
        "m.conversation_id, m.rowid"
    };
    let guarded_resume_predicate = if has_sequence {
        format!(
            "and case when octet_length(m.conversation_id)
                            <= {MAX_PROVIDER_SQLITE_VALUE_BYTES}
                            and typeof(m.conversation_id) = 'text'
                  then (m.conversation_id, m.sequence_id, m.rowid) >
                       (a.conversation_id, a.sequence_id, a.rowid)
                  else 0 end"
        )
    } else {
        format!(
            "and case when octet_length(m.conversation_id)
                            <= {MAX_PROVIDER_SQLITE_VALUE_BYTES}
                            and typeof(m.conversation_id) = 'text'
                  then (m.conversation_id, m.rowid) > (a.conversation_id, a.rowid)
                  else 0 end"
        )
    };
    let (anchor, from_clause, resume) = if after_rowid {
        (
            format!(
                "with a as (
                     select case
                                when octet_length(anchor.conversation_id)
                                     <= {MAX_PROVIDER_SQLITE_VALUE_BYTES}
                                     and typeof(anchor.conversation_id) = 'text'
                                then anchor.conversation_id
                                else null
                            end as conversation_id,
                            {anchor_sequence}
                            anchor.rowid as rowid
                     from messages anchor
                     where anchor.rowid = ?1
                 )"
            ),
            "from a cross join messages m",
            guarded_resume_predicate,
        )
    } else {
        (String::new(), "from messages m", String::new())
    };
    format!(
        "{anchor}
         select m.rowid, {message_lengths},
                0,
                case
                    when octet_length(m.conversation_id)
                         > {MAX_PROVIDER_SQLITE_VALUE_BYTES}
                         or typeof(m.conversation_id) <> 'text' then null
                    else (select c.rowid from conversations c
                          where c.conversation_id = m.conversation_id)
                end
         {from_clause}
         where octet_length(m.conversation_id) <= {MAX_PROVIDER_SQLITE_VALUE_BYTES}
           and typeof(m.conversation_id) = 'text'
         {resume}
         order by {native_order}
         limit 1"
    )
}

pub(super) fn shelley_same_group_message_candidate_sql(
    message_lengths: &str,
    has_sequence: bool,
) -> String {
    let (predicate, native_order) = if has_sequence {
        (
            "m.conversation_id = ?1 and m.sequence_id = ?2 and m.rowid > ?3",
            "m.rowid",
        )
    } else {
        ("m.conversation_id = ?1 and m.rowid > ?2", "m.rowid")
    };
    shelley_bound_message_candidate_sql(message_lengths, predicate, native_order)
}

pub(super) fn shelley_later_sequence_message_candidate_sql(message_lengths: &str) -> String {
    shelley_bound_message_candidate_sql(
        message_lengths,
        "m.conversation_id = ?1 and m.sequence_id > ?2",
        "m.sequence_id, m.rowid",
    )
}

pub(super) fn shelley_later_conversation_message_candidate_sql(
    message_lengths: &str,
    has_sequence: bool,
) -> String {
    let native_order = if has_sequence {
        "m.conversation_id, m.sequence_id, m.rowid"
    } else {
        "m.conversation_id, m.rowid"
    };
    shelley_bound_message_candidate_sql(message_lengths, "m.conversation_id > ?1", native_order)
}

pub(super) fn shelley_bound_message_candidate_sql(
    message_lengths: &str,
    predicate: &str,
    native_order: &str,
) -> String {
    format!(
        "select m.rowid, {message_lengths},
                0,
                (select c.rowid from conversations c
                 where c.conversation_id = m.conversation_id)
         from messages m
         where {predicate}
         order by {native_order}
         limit 1"
    )
}

pub(super) fn shelley_previous_message_same_conversation_sql(has_sequence: bool) -> String {
    let earlier = if has_sequence {
        "(previous.sequence_id, previous.rowid) < (m.sequence_id, m.rowid)"
    } else {
        "previous.rowid < m.rowid"
    };
    format!(
        "select exists (
             select 1 from messages previous
             where previous.conversation_id = case
                       when coalesce(octet_length(m.conversation_id), 0)
                            <= {MAX_PROVIDER_SQLITE_VALUE_BYTES}
                            and typeof(m.conversation_id) = 'text'
                       then m.conversation_id
                       else null
                   end
               and coalesce(octet_length(previous.conversation_id), 0)
                   <= {MAX_PROVIDER_SQLITE_VALUE_BYTES}
               and typeof(previous.conversation_id) = 'text'
               and {earlier}
             limit 1
         )
         from messages m where m.rowid = ?1"
    )
}

pub(super) fn shelley_conversation_candidate_sql(
    retained_lengths: &str,
    after_rowid: bool,
) -> String {
    let predicate = if after_rowid {
        "where c.rowid > ?1"
    } else {
        ""
    };
    format!(
        "select c.rowid, {retained_lengths},
                case when not exists (
                    select 1 from conversations later where later.rowid > c.rowid limit 1
                ) then 1 else 0 end,
                case when octet_length(c.conversation_id) > {MAX_PROVIDER_SQLITE_VALUE_BYTES}
                     then 0
                     else exists (
                         select 1 from messages m
                         where m.conversation_id = c.conversation_id limit 1
                     )
                end
         from conversations c
         {predicate}
         order by c.rowid limit 1"
    )
}

pub(super) fn shelley_observed_bytes(retained_bytes: i64) -> Result<u64> {
    let payload = u64::try_from(retained_bytes).map_err(|_| {
        CaptureError::InvalidPayload(
            "Shelley SQLite retained byte count must be nonnegative".to_owned(),
        )
    })?;
    SHELLEY_SQLITE_VALUE_OVERHEAD_BYTES
        .checked_add(payload)
        .ok_or(CaptureError::SystemInvariant(
            "Shelley SQLite retained byte count overflowed",
        ))
}

pub(super) fn with_shelley_length_preflight<T>(
    conn: &Connection,
    query: impl FnOnce() -> rusqlite::Result<T>,
) -> Result<T> {
    // SQLITE_LIMIT_LENGTH rejects even integer-only octet_length inspection of
    // an oversized stored record. The preflight SQL returns no raw TEXT/BLOB;
    // restore the provider cap before any hydration statement can run.
    let _guard = SqliteLengthPreflightGuard::new(conn);
    query().map_err(CaptureError::from)
}

pub(super) fn shelley_retained_length_expr(expressions: &[String]) -> String {
    // Unlike a cast to BLOB, octet_length can inspect large TEXT/BLOB columns without
    // materializing them through the bounded connection's SQLITE_LIMIT_LENGTH.
    expressions
        .iter()
        .map(|expression| format!("coalesce(octet_length({expression}), 0)"))
        .collect::<Vec<_>>()
        .join(" + ")
}
