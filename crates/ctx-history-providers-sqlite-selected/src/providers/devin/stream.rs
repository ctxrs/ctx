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
    provider::sqlite::{optional_timestamp_millis_expr, MAX_PROVIDER_SQLITE_VALUE_BYTES},
    CaptureError, Result,
};

use super::{chain::DevinNodeFacts, schema::DevinNativeSchema};

/// Sessions read per keyset page.
pub(super) const DEVIN_SESSION_PAGE_ROWS: usize = 64;
/// Nodes hydrated per batch.
pub(super) const DEVIN_HYDRATION_BATCH_ROWS: usize = 64;
/// Payload bytes a hydration batch targets before rolling over.
pub(super) const DEVIN_HYDRATION_BATCH_BYTES: u64 = 8 * 1024 * 1024;
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
    pub(super) row_id: i64,
    pub(super) node_id: i64,
    pub(super) parent_node_id: Option<i64>,
    pub(super) chat_message: String,
    pub(super) created_at: i64,
    pub(super) metadata: Option<String>,
}

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
    for row in rows {
        page.push(row?);
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
     json_extract(metadata, '$.summarized_from'), \
     json_extract(chat_message, '$.metadata.extensions.\"subagent/chain_node_id\"'), \
     json_extract(chat_message, '$.metadata.extensions.\"subagent/agent_id\"') \
     from message_nodes where session_id = ?1 order by node_id";

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
    let mut rows = statement.query([session_id])?;
    let mut facts = BTreeMap::new();
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
    }
    Ok((facts, main_chain_id))
}

fn hydration_sql(rows: usize) -> String {
    let parameters = (2..=rows + 1)
        .map(|parameter| format!("?{parameter}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "select row_id, node_id, parent_node_id, chat_message, created_at, metadata \
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
        let mut decoded = BTreeMap::new();
        let mut batch_bytes = 0_u64;
        while let Some(row) = rows.next()? {
            let node = decode_node_row(row)?;
            let bytes = node.chat_message.len() as u64
                + node.metadata.as_ref().map_or(0, |value| value.len()) as u64;
            if bytes > DEVIN_HYDRATION_SINGLETON_MAX_BYTES {
                return Err(E::from(CaptureError::InvalidPayload(format!(
                    "Devin session {session_id} node {} exceeds the provider record bound",
                    node.node_id
                ))));
            }
            batch_bytes = batch_bytes.saturating_add(bytes);
            if decoded.insert(node.node_id, node).is_some() {
                return Err(E::from(CaptureError::SystemInvariant(
                    "Devin payload hydration returned one node twice",
                )));
            }
        }
        drop(rows);
        debug_assert!(batch_bytes <= DEVIN_HYDRATION_BATCH_BYTES.saturating_mul(4));
        for node_id in batch {
            let node = decoded
                .remove(node_id)
                .ok_or(CaptureError::SystemInvariant(
                    "Devin node disappeared from its pinned snapshot",
                ))?;
            visit(node)?;
        }
        if !decoded.is_empty() {
            return Err(E::from(CaptureError::SystemInvariant(
                "Devin payload hydration returned an unrequested node",
            )));
        }
    }
    Ok(())
}

fn decode_node_row(row: &Row<'_>) -> rusqlite::Result<DevinNodeRow> {
    Ok(DevinNodeRow {
        row_id: row.get(0)?,
        node_id: row.get(1)?,
        parent_node_id: row.get(2)?,
        chat_message: row.get(3)?,
        created_at: row.get(4)?,
        metadata: row.get(5)?,
    })
}
