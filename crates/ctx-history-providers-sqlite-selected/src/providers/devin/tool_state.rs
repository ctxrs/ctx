//! Optional ACP enrichment from `tool_call_state`.
//!
//! Devin documents `tool_call_json` as nullable, and many tool nodes have no
//! row at all — the schema comment says a result can be saved without the
//! commit that would have written one. Enrichment is therefore always optional
//! and never a reason to fail a node: a missing or half-written row leaves the
//! transcript exactly as the node recorded it.

use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

use ctx_history_capture_model::raw_object_keys_are_unique;

use crate::{
    fingerprint::{hash_bytes, hash_optional_text},
    provider::sqlite::MAX_PROVIDER_SQLITE_VALUE_BYTES,
};

use super::source_backed::DevinResult;

/// What one `tool_call_state` lookup yielded.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct DevinToolState {
    pub(super) tool_call: Option<Value>,
    pub(super) tool_call_update: Option<Value>,
    /// Evidence for the fingerprint, present whenever a row existed at all so
    /// that a row appearing, changing, or vanishing is observable.
    pub(super) evidence: Option<[u8; 32]>,
}

const TOOL_STATE_SQL: &str = "select tool_call_json, tool_call_update_json \
     from tool_call_state where session_id = ?1 and tool_call_id = ?2";

/// Reads the ACP records for one call id, if the source has any.
pub(super) fn read_tool_state(
    conn: &Connection,
    session_id: &str,
    tool_call_id: &str,
) -> DevinResult<DevinToolState> {
    let row = conn
        .query_row(TOOL_STATE_SQL, [session_id, tool_call_id], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
            ))
        })
        .optional()?;
    let Some((call_text, update_text)) = row else {
        return Ok(DevinToolState::default());
    };
    Ok(DevinToolState {
        tool_call: parse_acp(call_text.as_deref()),
        tool_call_update: parse_acp(update_text.as_deref()),
        evidence: Some(tool_state_evidence(
            tool_call_id,
            call_text.as_deref(),
            update_text.as_deref(),
        )),
    })
}

/// Parses one ACP blob, declining anything ctx cannot read unambiguously.
///
/// A duplicate key or an oversized blob yields `None` rather than a partial
/// read; the node's own content is already the authoritative text, so
/// declining costs nothing.
fn parse_acp(text: Option<&str>) -> Option<Value> {
    let text = text?;
    if text.len() > MAX_PROVIDER_SQLITE_VALUE_BYTES || !raw_object_keys_are_unique(text.as_bytes())
    {
        return None;
    }
    serde_json::from_str(text).ok()
}

/// Evidence over the row as stored, including the absence of either column.
fn tool_state_evidence(
    tool_call_id: &str,
    call_text: Option<&str>,
    update_text: Option<&str>,
) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    hash_bytes(&mut digest, tool_call_id.as_bytes());
    hash_optional_text(&mut digest, call_text);
    hash_optional_text(&mut digest, update_text);
    digest.finalize().into()
}
