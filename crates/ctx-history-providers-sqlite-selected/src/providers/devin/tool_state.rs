//! Optional ACP enrichment from `tool_call_state`.
//!
//! Devin documents `tool_call_json` as nullable, and many tool nodes have no
//! row at all — the schema comment says a result can be saved without the
//! commit that would have written one. Enrichment is therefore always optional
//! and never a reason to fail a node: a missing or half-written row leaves the
//! transcript exactly as the node recorded it.

use rusqlite::{types::ValueRef, Connection, OptionalExtension};
use serde_json::Value;
use sha2::Digest;

use ctx_history_capture_model::raw_object_keys_are_unique;

use crate::{
    fingerprint::hash_bytes,
    provider::sqlite::{SqliteLengthPreflightGuard, MAX_PROVIDER_SQLITE_VALUE_BYTES},
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
    tool_call_rejection: Option<&'static str>,
    tool_call_update_rejection: Option<&'static str>,
}

impl DevinToolState {
    pub(super) fn malformed_fields(&self) -> impl Iterator<Item = (&'static str, &'static str)> {
        [
            ("tool_call_json", self.tool_call_rejection),
            ("tool_call_update_json", self.tool_call_update_rejection),
        ]
        .into_iter()
        .filter_map(|(field, reason)| reason.map(|reason| (field, reason)))
    }
}

pub(super) const TOOL_STATE_SQL: &str = "select \
     case when octet_length(tool_call_json) <= ?3 \
          then tool_call_json end, \
     typeof(tool_call_json), coalesce(octet_length(tool_call_json), 0), \
     case when octet_length(tool_call_update_json) <= ?3 \
          then tool_call_update_json end, \
     typeof(tool_call_update_json), coalesce(octet_length(tool_call_update_json), 0) \
     from tool_call_state where session_id collate binary = ?1 \
     and tool_call_id collate binary = ?2";

/// Reads the ACP records for one call id, if the source has any.
pub(super) fn read_tool_state(
    conn: &Connection,
    session_id: &str,
    tool_call_id: &str,
) -> DevinResult<DevinToolState> {
    let _length_guard = SqliteLengthPreflightGuard::new(conn)?;
    let state = conn
        .query_row(
            TOOL_STATE_SQL,
            rusqlite::params![
                session_id,
                tool_call_id,
                MAX_PROVIDER_SQLITE_VALUE_BYTES as i64,
            ],
            |row| {
                let call_storage_class = row.get::<_, String>(1)?;
                let call_bytes = row.get::<_, i64>(2)?;
                let update_storage_class = row.get::<_, String>(4)?;
                let update_bytes = row.get::<_, i64>(5)?;
                let call =
                    decode_bounded_optional_text(row.get_ref(0)?, &call_storage_class, call_bytes);
                let update = decode_bounded_optional_text(
                    row.get_ref(3)?,
                    &update_storage_class,
                    update_bytes,
                );
                Ok((
                    parse_acp(call),
                    parse_acp(update),
                    tool_state_evidence(
                        tool_call_id,
                        row.get_ref(0)?,
                        &call_storage_class,
                        call_bytes,
                        row.get_ref(3)?,
                        &update_storage_class,
                        update_bytes,
                    ),
                ))
            },
        )
        .optional()?;
    let Some((
        (tool_call, tool_call_rejection),
        (tool_call_update, tool_call_update_rejection),
        evidence,
    )) = state
    else {
        return Ok(DevinToolState::default());
    };
    Ok(DevinToolState {
        tool_call,
        tool_call_update,
        evidence: Some(evidence),
        tool_call_rejection,
        tool_call_update_rejection,
    })
}

fn decode_bounded_optional_text(
    value: ValueRef<'_>,
    storage_class: &str,
    raw_bytes: i64,
) -> (Option<String>, Option<&'static str>) {
    match storage_class {
        "null" => (None, None),
        "text" if raw_bytes > MAX_PROVIDER_SQLITE_VALUE_BYTES as i64 => {
            (None, Some("exceeds the provider value byte bound"))
        }
        "text" => match value {
            ValueRef::Text(value) => match std::str::from_utf8(value) {
                Ok(value) => (Some(value.to_owned()), None),
                Err(_) => (None, Some("has invalid UTF-8 text")),
            },
            _ => (None, Some("has an invalid text projection")),
        },
        "integer" => (None, Some("has an integer SQLite scalar")),
        "real" => (None, Some("has a real SQLite scalar")),
        "blob" => (None, Some("has a BLOB SQLite scalar")),
        _ => (None, Some("has an unknown SQLite scalar")),
    }
}

/// Parses one ACP blob, declining anything ctx cannot read unambiguously.
///
/// A duplicate key or an oversized blob yields `None` rather than a partial
/// read; the node's own content is already the authoritative text, so
/// declining costs nothing.
fn parse_acp(
    (text, rejection): (Option<String>, Option<&'static str>),
) -> (Option<Value>, Option<&'static str>) {
    let Some(text) = text else {
        return (None, rejection);
    };
    if text.len() > MAX_PROVIDER_SQLITE_VALUE_BYTES {
        return (None, Some("exceeds the provider value byte bound"));
    }
    let value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(_) => return (None, Some("is not valid JSON")),
    };
    if !raw_object_keys_are_unique(text.as_bytes()) {
        return (None, Some("has ambiguous duplicate JSON keys"));
    }
    (Some(value), None)
}

/// Evidence over the bounded row projection, including rejected cells' type
/// and byte length without retaining their unbounded payloads.
fn tool_state_evidence(
    tool_call_id: &str,
    call: ValueRef<'_>,
    call_storage_class: &str,
    call_bytes: i64,
    update: ValueRef<'_>,
    update_storage_class: &str,
    update_bytes: i64,
) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    hash_bytes(&mut digest, tool_call_id.as_bytes());
    hash_bounded_sqlite_value(&mut digest, call, call_storage_class, call_bytes);
    hash_bounded_sqlite_value(&mut digest, update, update_storage_class, update_bytes);
    digest.finalize().into()
}

fn hash_bounded_sqlite_value(
    digest: &mut sha2::Sha256,
    value: ValueRef<'_>,
    storage_class: &str,
    raw_bytes: i64,
) {
    hash_bytes(digest, storage_class.as_bytes());
    digest.update(raw_bytes.to_be_bytes());
    match value {
        ValueRef::Null => {}
        ValueRef::Integer(value) => digest.update(value.to_be_bytes()),
        ValueRef::Real(value) => digest.update(value.to_bits().to_be_bytes()),
        ValueRef::Text(value) | ValueRef::Blob(value) => hash_bytes(digest, value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_optional_cells_are_rejected_and_fingerprinted_without_failing_the_lookup() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "create table tool_call_state (\
                 session_id text not null, tool_call_id text not null,\
                 tool_call_json, tool_call_update_json\
             );\
             insert into tool_call_state values ('session', 'call', x'00', '{');",
        )
        .unwrap();

        let state = read_tool_state(&conn, "session", "call").unwrap();
        assert_eq!(state.tool_call, None);
        assert_eq!(state.tool_call_update, None);
        assert_eq!(
            state.malformed_fields().collect::<Vec<_>>(),
            vec![
                ("tool_call_json", "has a BLOB SQLite scalar"),
                ("tool_call_update_json", "is not valid JSON"),
            ]
        );
        let evidence = state.evidence;
        conn.execute(
            "update tool_call_state set tool_call_json = x'01' where session_id = 'session'",
            [],
        )
        .unwrap();
        assert_ne!(
            read_tool_state(&conn, "session", "call").unwrap().evidence,
            evidence
        );
    }
}
