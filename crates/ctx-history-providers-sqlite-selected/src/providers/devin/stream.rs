//! Bounded reads over a pinned Devin snapshot.
//!
//! Three access patterns, each riding an index the provider already declares,
//! so none of them sorts in SQLite temporary storage:
//!
//! - sessions, paged by the table's native rowid so a malformed dynamic-typed
//!   session key can be rejected without becoming the page cursor;
//! - one session's planning facts, ordered by `node_id` through
//!   `UNIQUE(session_id, node_id)`, reading only scalars so a planning pass
//!   never transfers a payload;
//! - the planned nodes' payloads, hydrated in bounded batches through the same
//!   index.

use std::collections::BTreeMap;

use ctx_history_capture_model::raw_object_keys_are_unique;
use rusqlite::{
    params_from_iter,
    types::{Value, ValueRef},
    Connection, Row,
};

use crate::{
    provider::sqlite::{
        optional_timestamp_millis_expr, sqlite_table_exists, SqliteLengthPreflightGuard,
        MAX_PROVIDER_SQLITE_VALUE_BYTES,
    },
    CaptureError, Result,
};

use super::{
    chain::{
        DevinNodeFacts, DevinSubagentHead, DEVIN_MAX_SESSION_NODES, DEVIN_SUBAGENT_AGENT_ID_BYTES,
    },
    schema::DevinNativeSchema,
};

/// Sessions read per keyset page.
pub(super) const DEVIN_SESSION_PAGE_ROWS: usize = 64;
/// Nodes hydrated per batch.
pub(super) const DEVIN_HYDRATION_BATCH_ROWS: usize = 64;
/// Payload bytes a hydration batch targets before rolling over.
pub(super) const DEVIN_HYDRATION_BATCH_BYTES: u64 = 8 * 1024 * 1024;
/// Total variable-width scalar text retained by one metadata/planning read.
const DEVIN_RETAINED_SCALAR_TEXT_BYTES: usize = 8 * 1024 * 1024;
/// Native identities are keys, not transcript payloads. Keep them small enough
/// to bind repeatedly without allowing one corrupt row to dominate memory.
const DEVIN_NATIVE_SESSION_ID_BYTES: usize = 16 * 1024;
/// A single node may exceed the batch target, but never the record contract.
/// The margin covers the fixed-width columns the bound is measured alongside.
pub(super) const DEVIN_HYDRATION_SINGLETON_MAX_BYTES: u64 =
    MAX_PROVIDER_SQLITE_VALUE_BYTES as u64 + 64;

/// One `sessions` row, as the importer needs it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct DevinSessionRow {
    pub(super) sqlite_rowid: i64,
    pub(super) id: String,
    pub(super) malformed_id: bool,
    pub(super) id_storage_class: String,
    pub(super) id_bytes: u64,
    pub(super) working_directory: String,
    pub(super) malformed_working_directory: bool,
    pub(super) main_chain_id: Option<i64>,
    pub(super) malformed_main_chain_id: bool,
    pub(super) created_at_ms: Option<i64>,
    pub(super) last_activity_at_ms: Option<i64>,
    pub(super) title: Option<String>,
    pub(super) malformed_title: bool,
    pub(super) model: Option<String>,
    pub(super) malformed_model: bool,
    pub(super) agent_mode: Option<String>,
    pub(super) malformed_agent_mode: bool,
    pub(super) hidden: bool,
    pub(super) malformed_hidden: bool,
}

/// A node omitted from planning because its structural parent pointer is not
/// an integer (or NULL) in the native row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DevinMalformedParentNode {
    pub(super) node_id: i64,
}

/// A node whose JSON object contains duplicate keys, so none of those keys can
/// establish an exact chain, splice, or subagent relationship.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DevinAmbiguousJsonNode {
    pub(super) node_id: i64,
    pub(super) metadata: bool,
    pub(super) chat_message: bool,
}

/// A message-node row whose SQLite storage classes cannot safely participate
/// in planning. `node_id` is absent when the native key itself is malformed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DevinMalformedNode {
    pub(super) node_id: Option<i64>,
    pub(super) malformed_node_id: bool,
    pub(super) malformed_parent_node_id: bool,
    pub(super) malformed_chat_message: bool,
    pub(super) malformed_created_at: bool,
    pub(super) malformed_metadata: bool,
    pub(super) oversized_payload: bool,
}

/// The planning facts and any row-local shape rejections observed while
/// reading them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct DevinSessionFacts {
    pub(super) facts: BTreeMap<i64, DevinNodeFacts>,
    /// Rows observed through the capped indexed scan, including malformed
    /// rows and the overflow sentinel. This deliberately does not derive from
    /// `facts`, whose retained rows omit malformed nodes.
    pub(super) total_rows: u64,
    pub(super) overflowed: bool,
    pub(super) malformed_nodes: Vec<DevinMalformedNode>,
    pub(super) malformed_parent_nodes: Vec<DevinMalformedParentNode>,
    pub(super) ambiguous_json_nodes: Vec<DevinAmbiguousJsonNode>,
}

/// A durable subagent head whose scalar shape cannot establish a lineage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DevinMalformedSubagentHead {
    pub(super) agent_id: Option<String>,
    pub(super) malformed_agent_id: bool,
    pub(super) malformed_chain_node_id: bool,
    pub(super) malformed_updated_at: bool,
    pub(super) exceeded_bound: bool,
}

/// Durable heads that can participate in planning, plus row-local rejections.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct DevinSubagentHeads {
    pub(super) heads: Vec<DevinSubagentHead>,
    pub(super) malformed_heads: Vec<DevinMalformedSubagentHead>,
}

/// One planned node's payload.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct DevinNodeRow {
    pub(super) node_id: i64,
    pub(super) parent_node_id: Option<i64>,
    pub(super) chat_message: String,
    pub(super) created_at: i64,
    pub(super) metadata: Option<String>,
}

pub(super) const SUBAGENT_HEADS_SQL: &str = "select \
     case when typeof(agent_id) = 'text' and octet_length(agent_id) between 1 and ?3 then agent_id end, \
     case when typeof(agent_id) = 'text' and octet_length(agent_id) between 1 and ?3 then 0 else 1 end, \
     case when typeof(chain_node_id) = 'integer' then chain_node_id end, \
     case when typeof(chain_node_id) = 'integer' then 0 else 1 end, \
     case when typeof(updated_at) = 'integer' then updated_at end, \
     case when typeof(updated_at) = 'integer' then 0 else 1 end \
     from subagent_heads where session_id collate binary = ?1 \
     order by agent_id collate binary limit ?2";

const SESSION_COLUMNS: &str = "rowid, \
     case when typeof(id) = 'text' and octet_length(id) <= ?4 then id end, \
     typeof(id), coalesce(octet_length(id), 0), \
     case when typeof(working_directory) = 'text' and \
                    (case when typeof(working_directory) = 'text' then octet_length(working_directory) else 0 end + \
                     case when typeof(title) = 'text' then octet_length(title) else 0 end + \
                     case when typeof(model) = 'text' then octet_length(model) else 0 end + \
                     case when typeof(agent_mode) = 'text' then octet_length(agent_mode) else 0 end) <= ?3 \
          then working_directory end, \
     case when typeof(working_directory) = 'text' and \
                    (case when typeof(working_directory) = 'text' then octet_length(working_directory) else 0 end + \
                     case when typeof(title) = 'text' then octet_length(title) else 0 end + \
                     case when typeof(model) = 'text' then octet_length(model) else 0 end + \
                     case when typeof(agent_mode) = 'text' then octet_length(agent_mode) else 0 end) <= ?3 \
          then 0 else 1 end, \
     case when typeof(main_chain_id) = 'integer' then main_chain_id end, \
     case when typeof(main_chain_id) in ('integer', 'null') then 0 else 1 end, \
     case when typeof(title) = 'text' and \
                    (case when typeof(working_directory) = 'text' then octet_length(working_directory) else 0 end + \
                     case when typeof(title) = 'text' then octet_length(title) else 0 end + \
                     case when typeof(model) = 'text' then octet_length(model) else 0 end + \
                     case when typeof(agent_mode) = 'text' then octet_length(agent_mode) else 0 end) <= ?3 \
          then title end, \
     case when typeof(title) in ('text', 'null') and \
                    (case when typeof(working_directory) = 'text' then octet_length(working_directory) else 0 end + \
                     case when typeof(title) = 'text' then octet_length(title) else 0 end + \
                     case when typeof(model) = 'text' then octet_length(model) else 0 end + \
                     case when typeof(agent_mode) = 'text' then octet_length(agent_mode) else 0 end) <= ?3 \
          then 0 else 1 end, \
     case when typeof(model) = 'text' and \
                    (case when typeof(working_directory) = 'text' then octet_length(working_directory) else 0 end + \
                     case when typeof(title) = 'text' then octet_length(title) else 0 end + \
                     case when typeof(model) = 'text' then octet_length(model) else 0 end + \
                     case when typeof(agent_mode) = 'text' then octet_length(agent_mode) else 0 end) <= ?3 \
          then model end, \
     case when typeof(model) in ('text', 'null') and \
                    (case when typeof(working_directory) = 'text' then octet_length(working_directory) else 0 end + \
                     case when typeof(title) = 'text' then octet_length(title) else 0 end + \
                     case when typeof(model) = 'text' then octet_length(model) else 0 end + \
                     case when typeof(agent_mode) = 'text' then octet_length(agent_mode) else 0 end) <= ?3 \
          then 0 else 1 end, \
     case when typeof(agent_mode) = 'text' and \
                    (case when typeof(working_directory) = 'text' then octet_length(working_directory) else 0 end + \
                     case when typeof(title) = 'text' then octet_length(title) else 0 end + \
                     case when typeof(model) = 'text' then octet_length(model) else 0 end + \
                     case when typeof(agent_mode) = 'text' then octet_length(agent_mode) else 0 end) <= ?3 \
          then agent_mode end, \
     case when typeof(agent_mode) in ('text', 'null') and \
                    (case when typeof(working_directory) = 'text' then octet_length(working_directory) else 0 end + \
                     case when typeof(title) = 'text' then octet_length(title) else 0 end + \
                     case when typeof(model) = 'text' then octet_length(model) else 0 end + \
                     case when typeof(agent_mode) = 'text' then octet_length(agent_mode) else 0 end) <= ?3 \
          then 0 else 1 end, \
     case when typeof(hidden) = 'integer' then hidden end, \
     case when typeof(hidden) in ('integer', 'null') then 0 else 1 end";

/// Builds the session page query.
///
/// The two timestamps go through the shared normalizer, which dispatches on
/// `typeof` so an integer, a real, epoch text, or RFC3339 all reduce to
/// milliseconds. Devin writes integer seconds today, but the store is
/// undocumented and the normalizer costs nothing.
pub(super) fn session_page_sql(schema: &DevinNativeSchema, after_rowid: bool) -> String {
    let columns = schema.session_columns();
    let cursor = if after_rowid { "where rowid > ?1" } else { "" };
    format!(
        "select {SESSION_COLUMNS}, \
                case when typeof(created_at) in ('integer', 'real') \
                           or (typeof(created_at) = 'text' and octet_length(created_at) <= 256) \
                     then {created} end, \
                case when typeof(last_activity_at) in ('integer', 'real') \
                           or (typeof(last_activity_at) = 'text' and octet_length(last_activity_at) <= 256) \
                     then {activity} end \
         from sessions {cursor} order by rowid limit ?2",
        created = optional_timestamp_millis_expr(columns, "created_at", "NULL"),
        activity = optional_timestamp_millis_expr(columns, "last_activity_at", "NULL"),
    )
}

/// Reads one keyset page of sessions after `after_rowid`.
///
/// The native rowid is used only as the bounded scan cursor. This keeps a
/// malformed dynamic-typed `id` row local and avoids relying on the declared
/// column collation when the admitted unique key is explicitly BINARY.
pub(super) fn read_session_page(
    conn: &Connection,
    schema: &DevinNativeSchema,
    after_rowid: Option<i64>,
) -> Result<Vec<DevinSessionRow>> {
    let mut statement = conn.prepare(&session_page_sql(schema, after_rowid.is_some()))?;
    let _length_guard = SqliteLengthPreflightGuard::new(conn)?;
    let rows = statement.query_map(
        rusqlite::params![
            after_rowid,
            DEVIN_SESSION_PAGE_ROWS as i64,
            DEVIN_RETAINED_SCALAR_TEXT_BYTES as i64,
            DEVIN_NATIVE_SESSION_ID_BYTES as i64,
        ],
        decode_session_row,
    )?;
    let mut page = Vec::with_capacity(DEVIN_SESSION_PAGE_ROWS);
    let mut retained_text_bytes = 0_usize;
    for row in rows {
        let row = row?;
        let row_bytes = row.id.len()
            + row.working_directory.len()
            + row.title.as_ref().map_or(0, String::len)
            + row.model.as_ref().map_or(0, String::len)
            + row.agent_mode.as_ref().map_or(0, String::len);
        let next_retained_text_bytes =
            retained_text_bytes.checked_add(row_bytes).ok_or_else(|| {
                CaptureError::InvalidPayload("Devin session-page text size overflowed".to_owned())
            })?;
        if !page.is_empty() && next_retained_text_bytes > DEVIN_RETAINED_SCALAR_TEXT_BYTES {
            break;
        }
        retained_text_bytes = next_retained_text_bytes;
        page.push(row);
    }
    Ok(page)
}

fn decode_session_row(row: &Row<'_>) -> rusqlite::Result<DevinSessionRow> {
    let (id, invalid_id_utf8) = decode_optional_text(row.get_ref(1)?);
    let id_storage_class = row.get::<_, String>(2)?;
    let raw_id_bytes = row.get::<_, i64>(3)?;
    let id_bytes = u64::try_from(raw_id_bytes)
        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(3, raw_id_bytes))?;
    let (working_directory, invalid_working_directory_utf8) = decode_optional_text(row.get_ref(4)?);
    let (title, invalid_title_utf8) = decode_optional_text(row.get_ref(8)?);
    let (model, invalid_model_utf8) = decode_optional_text(row.get_ref(10)?);
    let (agent_mode, invalid_agent_mode_utf8) = decode_optional_text(row.get_ref(12)?);
    Ok(DevinSessionRow {
        sqlite_rowid: row.get(0)?,
        id: id.unwrap_or_default(),
        malformed_id: id_storage_class != "text"
            || id_bytes == 0
            || id_bytes > DEVIN_NATIVE_SESSION_ID_BYTES as u64
            || invalid_id_utf8,
        id_storage_class,
        id_bytes,
        working_directory: working_directory.unwrap_or_default(),
        malformed_working_directory: row.get::<_, i64>(5)? != 0 || invalid_working_directory_utf8,
        main_chain_id: row.get(6)?,
        malformed_main_chain_id: row.get::<_, i64>(7)? != 0,
        title,
        malformed_title: row.get::<_, i64>(9)? != 0 || invalid_title_utf8,
        model,
        malformed_model: row.get::<_, i64>(11)? != 0 || invalid_model_utf8,
        agent_mode,
        malformed_agent_mode: row.get::<_, i64>(13)? != 0 || invalid_agent_mode_utf8,
        // `hidden` is a UI flag; a hidden session is still imported, and the
        // flag is retained so the fingerprint notices it changing.
        hidden: row.get::<_, Option<i64>>(14)?.unwrap_or(0) != 0,
        malformed_hidden: row.get::<_, i64>(15)? != 0,
        created_at_ms: row.get(16)?,
        last_activity_at_ms: row.get(17)?,
    })
}

pub(super) const SESSION_FACTS_SQL: &str = "select \
     case when typeof(node_id) = 'integer' then node_id end, \
     case when typeof(node_id) = 'integer' then 0 else 1 end, \
     case when typeof(parent_node_id) = 'integer' then parent_node_id end, \
     case when typeof(parent_node_id) in ('integer', 'null') then 0 else 1 end, \
     case when typeof(metadata) = 'text' and \
                    (case when typeof(metadata) = 'text' then octet_length(metadata) else 0 end + \
                     case when typeof(chat_message) = 'text' then octet_length(chat_message) else 0 end) <= ?3 and \
                    octet_length(metadata) <= ?4 and \
                    (typeof(chat_message) != 'text' or octet_length(chat_message) <= ?4) \
          then metadata end, \
     case when typeof(metadata) in ('text', 'null') then 0 else 1 end, \
     case when typeof(chat_message) = 'text' and \
                    (case when typeof(metadata) = 'text' then octet_length(metadata) else 0 end + \
                     case when typeof(chat_message) = 'text' then octet_length(chat_message) else 0 end) <= ?3 and \
                    octet_length(chat_message) <= ?4 and \
                    (typeof(metadata) != 'text' or octet_length(metadata) <= ?4) \
          then chat_message end, \
     case when typeof(chat_message) = 'text' then 0 else 1 end, \
     case when typeof(created_at) = 'integer' then created_at end, \
     case when typeof(created_at) = 'integer' then 0 else 1 end, \
     case when typeof(metadata) = 'text' and \
                    (case when typeof(metadata) = 'text' then octet_length(metadata) else 0 end + \
                     case when typeof(chat_message) = 'text' then octet_length(chat_message) else 0 end) <= ?3 and \
                    octet_length(metadata) <= ?4 and \
                    (typeof(chat_message) != 'text' or octet_length(chat_message) <= ?4) \
          then json_valid(metadata) else 0 end, \
     case when typeof(chat_message) = 'text' and \
                    (case when typeof(metadata) = 'text' then octet_length(metadata) else 0 end + \
                     case when typeof(chat_message) = 'text' then octet_length(chat_message) else 0 end) <= ?3 and \
                    octet_length(chat_message) <= ?4 and \
                    (typeof(metadata) != 'text' or octet_length(metadata) <= ?4) \
          then json_valid(chat_message) else 0 end, \
     case when typeof(metadata) = 'text' and \
                    (case when typeof(metadata) = 'text' then octet_length(metadata) else 0 end + \
                     case when typeof(chat_message) = 'text' then octet_length(chat_message) else 0 end) <= ?3 and \
                    octet_length(metadata) <= ?4 and \
                    (typeof(chat_message) != 'text' or octet_length(chat_message) <= ?4) \
                    and json_valid(metadata) then \
       case when json_type(metadata, '$.summarized_from') = 'integer' \
         and typeof(json_extract(metadata, '$.summarized_from')) = 'integer' \
         then json_extract(metadata, '$.summarized_from') end end, \
     case when typeof(chat_message) = 'text' and \
                    (case when typeof(metadata) = 'text' then octet_length(metadata) else 0 end + \
                     case when typeof(chat_message) = 'text' then octet_length(chat_message) else 0 end) <= ?3 and \
                    octet_length(chat_message) <= ?4 and \
                    (typeof(metadata) != 'text' or octet_length(metadata) <= ?4) \
                    and json_valid(chat_message) then \
       case when json_type(chat_message, '$.metadata.extensions.\"subagent/chain_node_id\"') = 'integer' \
         and typeof(json_extract(chat_message, '$.metadata.extensions.\"subagent/chain_node_id\"')) = 'integer' \
         then json_extract(chat_message, '$.metadata.extensions.\"subagent/chain_node_id\"') end end, \
     case when typeof(chat_message) = 'text' and \
                    (case when typeof(metadata) = 'text' then octet_length(metadata) else 0 end + \
                     case when typeof(chat_message) = 'text' then octet_length(chat_message) else 0 end) <= ?3 \
                    and json_valid(chat_message) then \
       case when json_type(chat_message, '$.metadata.extensions.\"subagent/agent_id\"') = 'text' \
         then case when octet_length(json_extract(chat_message, \
                              '$.metadata.extensions.\"subagent/agent_id\"')) between 1 and ?5 \
              then json_extract(chat_message, '$.metadata.extensions.\"subagent/agent_id\"') \
              else '' end end end, \
     case when \
                    (case when typeof(metadata) = 'text' then octet_length(metadata) else 0 end + \
                     case when typeof(chat_message) = 'text' then octet_length(chat_message) else 0 end) > ?3 or \
                    (typeof(metadata) = 'text' and octet_length(metadata) > ?4) or \
                    (typeof(chat_message) = 'text' and octet_length(chat_message) > ?4) \
          then 1 else 0 end \
     from message_nodes where session_id collate binary = ?1 \
     order by node_id collate binary limit ?2";

const SESSION_NODE_COUNT_SQL: &str =
    "select count(*) from message_nodes where session_id collate binary = ?1";

/// Reads every node's planning facts for one session, plus its main chain.
///
/// The native index orders this capped scan. JSON passes through one row at a
/// time so Rust can reject duplicate object keys in linear time without a
/// correlated `json_tree` traversal.
#[cfg(test)]
pub(super) fn read_session_facts(
    conn: &Connection,
    session_id: &str,
) -> Result<(BTreeMap<i64, DevinNodeFacts>, Option<i64>)> {
    let main_chain_id = conn
        .query_row(
            "select case when typeof(main_chain_id) = 'integer' then main_chain_id end \
             from sessions where id collate binary = ?1",
            [session_id],
            |row| row.get::<_, Option<i64>>(0),
        )
        .map_err(CaptureError::from)?;
    let session_facts = read_session_facts_with_row_shape_rejections(conn, session_id)?;
    Ok((session_facts.facts, main_chain_id))
}

/// Reads planning facts without letting a malformed parent scalar abort the
/// entire source. The malformed node is deliberately absent from the forest:
/// an off-chain node stays local, while a chain walk reaches a hole and is
/// rejected by the planner.
pub(super) fn read_session_facts_with_row_shape_rejections(
    conn: &Connection,
    session_id: &str,
) -> Result<DevinSessionFacts> {
    read_session_facts_with_limit(conn, session_id, DEVIN_MAX_SESSION_NODES)
}

#[cfg(test)]
pub(super) fn read_session_facts_with_test_limit(
    conn: &Connection,
    session_id: &str,
    node_limit: usize,
) -> Result<DevinSessionFacts> {
    read_session_facts_with_limit(conn, session_id, node_limit)
}

fn read_session_facts_with_limit(
    conn: &Connection,
    session_id: &str,
    node_limit: usize,
) -> Result<DevinSessionFacts> {
    let mut statement = conn.prepare(SESSION_FACTS_SQL)?;
    let _length_guard = SqliteLengthPreflightGuard::new(conn)?;
    let row_limit = i64::try_from(node_limit.saturating_add(1)).map_err(|_| {
        CaptureError::InvalidPayload("Devin session-node row limit exceeds i64".to_owned())
    })?;
    let payload_limit = i64::try_from(DEVIN_HYDRATION_SINGLETON_MAX_BYTES)
        .map_err(|_| CaptureError::SystemInvariant("Devin payload byte bound exceeds i64"))?;
    let mut rows = statement.query(rusqlite::params![
        session_id,
        row_limit,
        payload_limit,
        MAX_PROVIDER_SQLITE_VALUE_BYTES as i64,
        DEVIN_SUBAGENT_AGENT_ID_BYTES as i64,
    ])?;
    let mut facts = BTreeMap::new();
    let mut total_rows = 0_u64;
    let mut overflowed = false;
    let mut malformed_nodes = Vec::new();
    let mut malformed_parent_nodes = Vec::new();
    let mut ambiguous_json_nodes = Vec::new();
    let mut planning_text_bytes = 0_usize;
    while let Some(row) = rows.next()? {
        total_rows = total_rows.checked_add(1).ok_or_else(|| {
            CaptureError::InvalidPayload(format!("Devin session {session_id} row count overflowed"))
        })?;
        let node_id = row.get::<_, Option<i64>>(0)?;
        let malformed_node_id = row.get::<_, i64>(1)? != 0;
        let malformed_parent_node_id = row.get::<_, i64>(3)? != 0;
        let (metadata, invalid_metadata_utf8) = decode_optional_text(row.get_ref(4)?);
        let malformed_metadata = row.get::<_, i64>(5)? != 0 || invalid_metadata_utf8;
        let (chat_message, invalid_chat_message_utf8) = decode_optional_text(row.get_ref(6)?);
        let malformed_chat_message = row.get::<_, i64>(7)? != 0 || invalid_chat_message_utf8;
        let malformed_created_at = row.get::<_, i64>(9)? != 0;
        let metadata_json_valid = row.get::<_, i64>(10)? != 0;
        let chat_message_json_valid = row.get::<_, i64>(11)? != 0;
        let oversized_payload = row.get::<_, i64>(15)? != 0;
        let malformed = DevinMalformedNode {
            node_id,
            malformed_node_id,
            malformed_parent_node_id,
            malformed_chat_message,
            malformed_created_at,
            malformed_metadata,
            oversized_payload,
        };
        if malformed_node_id
            || malformed_parent_node_id
            || malformed_chat_message
            || malformed_created_at
            || malformed_metadata
            || oversized_payload
        {
            if malformed_parent_node_id {
                if let Some(node_id) = malformed.node_id {
                    malformed_parent_nodes.push(DevinMalformedParentNode { node_id });
                }
            }
            malformed_nodes.push(malformed);
        }
        if total_rows > node_limit as u64 {
            overflowed = true;
            break;
        }
        if malformed_node_id || malformed_parent_node_id {
            continue;
        }
        let node_id = node_id.ok_or_else(|| {
            CaptureError::SystemInvariant("Devin integer node_id was projected as NULL")
        })?;
        let usable_metadata = !malformed_metadata && !oversized_payload;
        let usable_chat_message = !malformed_chat_message && !oversized_payload;
        let unique_metadata = usable_metadata
            && (!metadata_json_valid
                || metadata
                    .as_ref()
                    .is_none_or(|value| raw_object_keys_are_unique(value.as_bytes())));
        let unique_chat_message = usable_chat_message
            && (!chat_message_json_valid
                || chat_message
                    .as_ref()
                    .is_none_or(|value| raw_object_keys_are_unique(value.as_bytes())));
        let mut entry = DevinNodeFacts {
            parent_node_id: row.get(2)?,
            summarized_from: if unique_metadata { row.get(12)? } else { None },
            subagent_chain_node_id: if unique_chat_message {
                row.get(13)?
            } else {
                None
            },
            subagent_agent_id: if unique_chat_message {
                row.get(14)?
            } else {
                None
            },
        };
        let agent_id_bytes = entry.subagent_agent_id.as_ref().map_or(0, String::len);
        match planning_text_bytes.checked_add(agent_id_bytes) {
            Some(next) if next <= DEVIN_RETAINED_SCALAR_TEXT_BYTES => {
                planning_text_bytes = next;
            }
            _ => {
                // Preserve the relationship's structural presence without
                // retaining an unbounded identity. The planner rejects the
                // empty sentinel as a lineage-local invalid identity.
                entry.subagent_agent_id = Some(String::new());
            }
        }
        let ambiguous_metadata = usable_metadata && metadata_json_valid && !unique_metadata;
        let ambiguous_chat_message =
            usable_chat_message && chat_message_json_valid && !unique_chat_message;
        if ambiguous_metadata || ambiguous_chat_message {
            ambiguous_json_nodes.push(DevinAmbiguousJsonNode {
                node_id,
                metadata: ambiguous_metadata,
                chat_message: ambiguous_chat_message,
            });
        }
        if facts.insert(node_id, entry).is_some() {
            // UNIQUE(session_id, node_id) makes this unreachable; kept so a
            // locally modified index cannot silently collapse two nodes.
            return Err(CaptureError::InvalidPayload(format!(
                "Devin session {session_id} contains duplicate node {node_id}"
            )));
        }
    }
    drop(rows);
    drop(statement);
    if overflowed {
        total_rows = conn
            .query_row(SESSION_NODE_COUNT_SQL, [session_id], |row| row.get(0))
            .map_err(CaptureError::from)?;
    }
    Ok(DevinSessionFacts {
        facts,
        total_rows,
        overflowed,
        malformed_nodes,
        malformed_parent_nodes,
        ambiguous_json_nodes,
    })
}

fn decode_optional_text(value: ValueRef<'_>) -> (Option<String>, bool) {
    match value {
        ValueRef::Null => (None, false),
        ValueRef::Text(value) => match std::str::from_utf8(value) {
            Ok(value) => (Some(value.to_owned()), false),
            Err(_) => (None, true),
        },
        ValueRef::Integer(_) | ValueRef::Real(_) | ValueRef::Blob(_) => (None, false),
    }
}

/// Reads one session's durable subagent pointers in native primary-key order.
#[cfg(test)]
pub(super) fn read_subagent_heads(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<DevinSubagentHead>> {
    Ok(read_subagent_heads_with_row_shape_rejections(conn, session_id)?.heads)
}

/// Reads durable heads without allowing one malformed scalar to discard the
/// primary transcript or another agent's valid lineage.
pub(super) fn read_subagent_heads_with_row_shape_rejections(
    conn: &Connection,
    session_id: &str,
) -> Result<DevinSubagentHeads> {
    if !sqlite_table_exists(conn, "subagent_heads")? {
        return Ok(DevinSubagentHeads::default());
    }
    let row_limit = DEVIN_MAX_SESSION_NODES.saturating_add(1) as i64;
    let mut statement = conn.prepare(SUBAGENT_HEADS_SQL)?;
    let _length_guard = SqliteLengthPreflightGuard::new(conn)?;
    let mut rows = statement.query(rusqlite::params![
        session_id,
        row_limit,
        DEVIN_SUBAGENT_AGENT_ID_BYTES as i64,
    ])?;
    let mut heads = Vec::new();
    let mut malformed_heads = Vec::new();
    let mut planning_text_bytes = 0_usize;
    while let Some(row) = rows.next()? {
        let (mut agent_id, invalid_agent_id_utf8) = decode_optional_text(row.get_ref(0)?);
        let mut malformed_agent_id = row.get::<_, i64>(1)? != 0 || invalid_agent_id_utf8;
        let mut exceeded_bound = false;
        if heads.len().saturating_add(malformed_heads.len()) >= DEVIN_MAX_SESSION_NODES {
            exceeded_bound = true;
        } else if !malformed_agent_id {
            match planning_text_bytes.checked_add(agent_id.as_ref().map_or(0, String::len)) {
                Some(next) if next <= DEVIN_RETAINED_SCALAR_TEXT_BYTES => {
                    planning_text_bytes = next;
                }
                _ => {
                    agent_id = None;
                    malformed_agent_id = true;
                    exceeded_bound = true;
                }
            }
        }
        let malformed_chain_node_id = row.get::<_, i64>(3)? != 0;
        let malformed_updated_at = row.get::<_, i64>(5)? != 0;
        if malformed_agent_id || malformed_chain_node_id || malformed_updated_at || exceeded_bound {
            malformed_heads.push(DevinMalformedSubagentHead {
                agent_id,
                malformed_agent_id,
                malformed_chain_node_id,
                malformed_updated_at,
                exceeded_bound,
            });
            if exceeded_bound
                && heads.len().saturating_add(malformed_heads.len()) > DEVIN_MAX_SESSION_NODES
            {
                break;
            }
        } else {
            heads.push(DevinSubagentHead {
                agent_id: agent_id.ok_or(CaptureError::SystemInvariant(
                    "Devin text subagent id was projected as NULL",
                ))?,
                chain_node_id: row.get(2)?,
                updated_at: row.get(4)?,
            });
        }
    }
    Ok(DevinSubagentHeads {
        heads,
        malformed_heads,
    })
}

pub(super) fn hydration_sql(rows: usize) -> String {
    let parameters = (2..=rows + 1)
        .map(|parameter| format!("?{parameter}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "select \
         case when typeof(node_id) = 'integer' then node_id end, \
         case when typeof(node_id) = 'integer' then 0 else 1 end, \
         case when typeof(parent_node_id) = 'integer' then parent_node_id end, \
         case when typeof(parent_node_id) in ('integer', 'null') then 0 else 1 end, \
         case when typeof(chat_message) = 'text' then chat_message end, \
         case when typeof(chat_message) = 'text' then 0 else 1 end, \
         case when typeof(created_at) = 'integer' then created_at end, \
         case when typeof(created_at) = 'integer' then 0 else 1 end, \
         case when typeof(metadata) = 'text' then metadata end, \
         case when typeof(metadata) in ('text', 'null') then 0 else 1 end \
         from message_nodes \
         where session_id collate binary = ?1 \
         and node_id collate binary in ({parameters}) order by node_id collate binary"
    )
}

/// Hydrates the given nodes in `node_id` order, in bounded batches.
///
/// The visitor sees nodes in the order requested. A batch is capped by both
/// row count and payload bytes so one large node cannot make a batch
/// unbounded, and any single node beyond the record contract fails the source
/// rather than being truncated.
pub(super) fn hydrate_nodes<E>(
    conn: &Connection,
    session_id: &str,
    node_ids: &[i64],
    visit: &mut dyn FnMut(DevinNodeRow) -> std::result::Result<(), E>,
) -> std::result::Result<(), E>
where
    E: From<CaptureError> + From<rusqlite::Error>,
{
    for batch in node_ids.chunks(DEVIN_HYDRATION_BATCH_ROWS) {
        let sql = hydration_sql(batch.len());
        let mut statement = conn.prepare(&sql)?;
        let mut parameters: Vec<Value> = Vec::with_capacity(batch.len().saturating_add(1));
        parameters.push(Value::Text(session_id.to_owned()));
        parameters.extend(batch.iter().map(|&node_id| node_id.into()));
        let mut rows = statement.query(params_from_iter(parameters))?;
        let mut decoded = Vec::new();
        let mut batch_bytes = 0_u64;
        let mut expected = 0_usize;
        while let Some(row) = rows.next()? {
            let node = decode_node_row(row).map_err(E::from)?;
            if batch.get(expected) != Some(&node.node_id) {
                return Err(E::from(CaptureError::SystemInvariant(
                    "Devin payload hydration returned a missing, duplicate, or unrequested node",
                )));
            }
            expected += 1;
            let bytes = node.chat_message.len() as u64
                + node.metadata.as_ref().map_or(0, |value| value.len()) as u64;
            if bytes > DEVIN_HYDRATION_SINGLETON_MAX_BYTES {
                return Err(E::from(CaptureError::InvalidPayload(format!(
                    "Devin session {session_id} node {} exceeds the provider record bound",
                    node.node_id
                ))));
            }
            if !decoded.is_empty()
                && batch_bytes.saturating_add(bytes) > DEVIN_HYDRATION_BATCH_BYTES
            {
                flush_hydrated(&mut decoded, visit)?;
                batch_bytes = 0;
            }
            batch_bytes = batch_bytes.saturating_add(bytes);
            decoded.push(node);
        }
        drop(rows);
        if expected != batch.len() {
            return Err(E::from(CaptureError::SystemInvariant(
                "Devin node disappeared from its pinned snapshot",
            )));
        }
        flush_hydrated(&mut decoded, visit)?;
    }
    Ok(())
}

fn flush_hydrated<E>(
    decoded: &mut Vec<DevinNodeRow>,
    visit: &mut dyn FnMut(DevinNodeRow) -> std::result::Result<(), E>,
) -> std::result::Result<(), E> {
    for node in decoded.drain(..) {
        visit(node)?;
    }
    Ok(())
}

fn decode_node_row(row: &Row<'_>) -> Result<DevinNodeRow> {
    if row.get::<_, i64>(1)? != 0
        || row.get::<_, i64>(3)? != 0
        || row.get::<_, i64>(5)? != 0
        || row.get::<_, i64>(7)? != 0
        || row.get::<_, i64>(9)? != 0
    {
        return Err(CaptureError::InvalidPayload(
            "Devin planned node changed to an invalid SQLite scalar shape".to_owned(),
        ));
    }
    Ok(DevinNodeRow {
        node_id: row
            .get::<_, Option<i64>>(0)?
            .ok_or(CaptureError::SystemInvariant(
                "Devin integer node_id was projected as NULL during hydration",
            ))?,
        parent_node_id: row.get(2)?,
        chat_message: row
            .get::<_, Option<String>>(4)?
            .ok_or(CaptureError::SystemInvariant(
                "Devin text chat_message was projected as NULL during hydration",
            ))?,
        created_at: row
            .get::<_, Option<i64>>(6)?
            .ok_or(CaptureError::SystemInvariant(
                "Devin integer created_at was projected as NULL during hydration",
            ))?,
        metadata: row.get(8)?,
    })
}
