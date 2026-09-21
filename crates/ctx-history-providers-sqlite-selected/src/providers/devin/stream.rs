//! Bounded reads over a pinned Devin snapshot.
//!
//! Three access patterns, each riding an index the provider already declares,
//! so none of them sorts in SQLite temporary storage:
//!
//! - sessions, paged by keyset over the `sessions` primary key;
//! - one session's planning facts, ordered by `node_id` through
//!   `UNIQUE(session_id, node_id)`, reading only scalars so a planning pass
//!   never transfers a payload;
//! - the planned nodes' payloads, hydrated in bounded batches through the same
//!   index.

use std::collections::BTreeMap;

use rusqlite::{params_from_iter, types::Value, Connection, Row};

use crate::{
    provider::sqlite::{
        optional_timestamp_millis_expr, sqlite_table_exists, MAX_PROVIDER_SQLITE_VALUE_BYTES,
    },
    CaptureError, Result,
};

use super::{
    chain::{DevinNodeFacts, DevinSubagentHead, DEVIN_MAX_SESSION_NODES},
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
/// A single node may exceed the batch target, but never the record contract.
/// The margin covers the fixed-width columns the bound is measured alongside.
pub(super) const DEVIN_HYDRATION_SINGLETON_MAX_BYTES: u64 =
    MAX_PROVIDER_SQLITE_VALUE_BYTES as u64 + 64;

/// One `sessions` row, as the importer needs it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct DevinSessionRow {
    pub(super) id: String,
    pub(super) working_directory: String,
    pub(super) main_chain_id: Option<i64>,
    pub(super) created_at_ms: Option<i64>,
    pub(super) last_activity_at_ms: Option<i64>,
    pub(super) title: Option<String>,
    pub(super) model: Option<String>,
    pub(super) agent_mode: Option<String>,
    pub(super) hidden: bool,
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

pub(super) const SUBAGENT_HEADS_SQL: &str = "select agent_id, chain_node_id, updated_at \
     from subagent_heads where session_id = ?1 order by agent_id limit ?2";

const SESSION_COLUMNS: &str =
    "id, working_directory, main_chain_id, title, model, agent_mode, hidden";

/// Builds the session page query.
///
/// The two timestamps go through the shared normalizer, which dispatches on
/// `typeof` so an integer, a real, epoch text, or RFC3339 all reduce to
/// milliseconds. Devin writes integer seconds today, but the store is
/// undocumented and the normalizer costs nothing.
pub(super) fn session_page_sql(schema: &DevinNativeSchema) -> String {
    let columns = schema.session_columns();
    format!(
        "select {SESSION_COLUMNS}, {created}, {activity} \
         from sessions where id > ?1 order by id limit ?2",
        created = optional_timestamp_millis_expr(columns, "created_at", "NULL"),
        activity = optional_timestamp_millis_expr(columns, "last_activity_at", "NULL"),
    )
}

/// Reads one keyset page of sessions after `after_id`.
///
/// Keyset paging rather than offset paging so a page boundary cannot skip or
/// repeat a session if the snapshot's page cache is re-read.
pub(super) fn read_session_page(
    conn: &Connection,
    schema: &DevinNativeSchema,
    after_id: &str,
) -> Result<Vec<DevinSessionRow>> {
    let mut statement = conn.prepare(&session_page_sql(schema))?;
    let rows = statement.query_map(
        rusqlite::params![after_id, DEVIN_SESSION_PAGE_ROWS as i64],
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
        retained_text_bytes = retained_text_bytes.checked_add(row_bytes).ok_or_else(|| {
            CaptureError::InvalidPayload("Devin session-page text size overflowed".to_owned())
        })?;
        if retained_text_bytes > DEVIN_RETAINED_SCALAR_TEXT_BYTES {
            return Err(CaptureError::InvalidPayload(
                "Devin session page exceeds the retained text byte bound".to_owned(),
            ));
        }
        page.push(row);
    }
    Ok(page)
}

fn decode_session_row(row: &Row<'_>) -> rusqlite::Result<DevinSessionRow> {
    Ok(DevinSessionRow {
        id: row.get(0)?,
        working_directory: row.get(1)?,
        main_chain_id: row.get(2)?,
        title: row.get(3)?,
        model: row.get(4)?,
        agent_mode: row.get(5)?,
        // `hidden` is a UI flag; a hidden session is still imported, and the
        // flag is retained so the fingerprint notices it changing.
        hidden: row.get::<_, Option<i64>>(6)?.unwrap_or(0) != 0,
        created_at_ms: row.get(7)?,
        last_activity_at_ms: row.get(8)?,
    })
}

pub(super) const SESSION_FACTS_SQL: &str = "select node_id, parent_node_id, \
     case when json_valid(metadata) then \
       case when json_type(metadata, '$.summarized_from') = 'integer' \
         then json_extract(metadata, '$.summarized_from') end end, \
     case when json_valid(chat_message) then \
       case when json_type(chat_message, '$.metadata.extensions.\"subagent/chain_node_id\"') = 'integer' \
         then json_extract(chat_message, '$.metadata.extensions.\"subagent/chain_node_id\"') end end, \
     case when json_valid(chat_message) then \
       case when json_type(chat_message, '$.metadata.extensions.\"subagent/agent_id\"') = 'text' \
         then json_extract(chat_message, '$.metadata.extensions.\"subagent/agent_id\"') end end \
     from message_nodes where session_id = ?1 order by node_id limit ?2";

/// Reads every node's planning facts for one session, plus its main chain.
///
/// Only scalars cross the boundary: `json_extract` pulls the two fields
/// planning needs out of `chat_message` without transferring the payload,
/// which is what keeps a planning pass cheap on a large session.
pub(super) fn read_session_facts(
    conn: &Connection,
    session_id: &str,
) -> Result<(BTreeMap<i64, DevinNodeFacts>, Option<i64>)> {
    let main_chain_id = conn
        .query_row(
            "select main_chain_id from sessions where id = ?1",
            [session_id],
            |row| row.get::<_, Option<i64>>(0),
        )
        .map_err(CaptureError::from)?;
    let mut statement = conn.prepare(SESSION_FACTS_SQL)?;
    let row_limit = DEVIN_MAX_SESSION_NODES.saturating_add(1) as i64;
    let mut rows = statement.query(rusqlite::params![session_id, row_limit])?;
    let mut facts = BTreeMap::new();
    let mut planning_text_bytes = 0_usize;
    while let Some(row) = rows.next()? {
        let node_id: i64 = row.get(0)?;
        let entry = DevinNodeFacts {
            parent_node_id: row.get(1)?,
            summarized_from: row.get(2)?,
            subagent_chain_node_id: row.get(3)?,
            subagent_agent_id: row.get(4)?,
        };
        if facts.insert(node_id, entry).is_some() {
            // UNIQUE(session_id, node_id) makes this unreachable; kept so a
            // locally modified index cannot silently collapse two nodes.
            return Err(CaptureError::InvalidPayload(format!(
                "Devin session {session_id} contains duplicate node {node_id}"
            )));
        }
        if facts.len() > DEVIN_MAX_SESSION_NODES {
            // The planner needs only the sentinel entry to classify this
            // session as over-bound. Do not retain the rest of the forest.
            break;
        }
        planning_text_bytes = planning_text_bytes
            .checked_add(
                facts[&node_id]
                    .subagent_agent_id
                    .as_ref()
                    .map_or(0, String::len),
            )
            .ok_or_else(|| {
                CaptureError::InvalidPayload(format!(
                    "Devin session {session_id} planning text size overflowed"
                ))
            })?;
        if planning_text_bytes > DEVIN_RETAINED_SCALAR_TEXT_BYTES {
            return Err(CaptureError::InvalidPayload(format!(
                "Devin session {session_id} exceeds the planning text byte bound"
            )));
        }
    }
    Ok((facts, main_chain_id))
}

/// Reads one session's durable subagent pointers in native primary-key order.
pub(super) fn read_subagent_heads(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<DevinSubagentHead>> {
    if !sqlite_table_exists(conn, "subagent_heads")? {
        return Ok(Vec::new());
    }
    let row_limit = DEVIN_MAX_SESSION_NODES.saturating_add(1) as i64;
    let mut statement = conn.prepare(SUBAGENT_HEADS_SQL)?;
    let mut rows = statement.query(rusqlite::params![session_id, row_limit])?;
    let mut heads = Vec::new();
    let mut planning_text_bytes = 0_usize;
    while let Some(row) = rows.next()? {
        let agent_id: String = row.get(0)?;
        planning_text_bytes = planning_text_bytes
            .checked_add(agent_id.len())
            .ok_or_else(|| {
                CaptureError::InvalidPayload(format!(
                    "Devin session {session_id} durable-head text size overflowed"
                ))
            })?;
        if planning_text_bytes > DEVIN_RETAINED_SCALAR_TEXT_BYTES {
            return Err(CaptureError::InvalidPayload(format!(
                "Devin session {session_id} exceeds the durable-head text byte bound"
            )));
        }
        heads.push(DevinSubagentHead {
            agent_id,
            chain_node_id: row.get(1)?,
            updated_at: row.get(2)?,
        });
    }
    if heads.len() > DEVIN_MAX_SESSION_NODES {
        return Err(CaptureError::InvalidPayload(format!(
            "Devin session {session_id} exceeds the durable subagent head bound"
        )));
    }
    Ok(heads)
}

fn hydration_sql(rows: usize) -> String {
    let parameters = (2..=rows + 1)
        .map(|parameter| format!("?{parameter}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "select node_id, parent_node_id, chat_message, created_at, metadata \
         from message_nodes where session_id = ?1 and node_id in ({parameters}) order by node_id"
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
            let node = decode_node_row(row)?;
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

fn decode_node_row(row: &Row<'_>) -> rusqlite::Result<DevinNodeRow> {
    Ok(DevinNodeRow {
        node_id: row.get(0)?,
        parent_node_id: row.get(1)?,
        chat_message: row.get(2)?,
        created_at: row.get(3)?,
        metadata: row.get(4)?,
    })
}
