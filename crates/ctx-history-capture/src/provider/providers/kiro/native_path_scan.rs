use rusqlite::{Connection, OptionalExtension};

use crate::{provider::sqlite::SqliteLengthPreflightGuard, Result};

use super::super::history::KiroConversationRow;
use super::KiroPhase;

pub(super) struct KiroCandidate {
    pub(super) phase: KiroPhase,
    pub(super) rowid: i64,
    // Preserve the admitted row ordinal in the candidate data shape for exact
    // cross-target scan diagnostics.
    #[allow(dead_code)]
    pub(super) row_ordinal: u64,
    pub(super) retained_bytes: u64,
    pub(super) type_valid: [bool; 5],
}

impl KiroCandidate {
    pub(super) fn rejection_reason(&self) -> Option<&'static str> {
        let [key, conversation_id, value, created_at, updated_at] = self.type_valid;
        if !key {
            return Some("Kiro conversation key has an unsupported SQLite storage class");
        }
        if self.phase == KiroPhase::V2 && !conversation_id {
            return Some(
                "Kiro conversations_v2.conversation_id has an unsupported SQLite storage class",
            );
        }
        if !value {
            return Some("Kiro conversation value has an unsupported SQLite storage class");
        }
        if self.phase == KiroPhase::V2 && !created_at {
            return Some(
                "Kiro conversations_v2.created_at has an unsupported SQLite storage class",
            );
        }
        if self.phase == KiroPhase::V2 && !updated_at {
            return Some(
                "Kiro conversations_v2.updated_at has an unsupported SQLite storage class",
            );
        }
        None
    }
}

pub(super) fn next_candidate(
    connection: &Connection,
    phase: KiroPhase,
    after_rowid: Option<i64>,
    row_ordinal: u64,
) -> Result<Option<KiroCandidate>> {
    let table = phase.table();
    let where_clause = if after_rowid.is_some() {
        " where rowid > ?1"
    } else {
        ""
    };
    let fields = match phase {
        KiroPhase::V2 => {
            "coalesce(octet_length(key), 0) + coalesce(octet_length(conversation_id), 0) + \
             coalesce(octet_length(value), 0), typeof(key) = 'text', \
             typeof(conversation_id) = 'text', typeof(value) = 'text', \
             typeof(created_at) in ('null', 'integer'), \
             typeof(updated_at) in ('null', 'integer')"
        }
        KiroPhase::Legacy => {
            "coalesce(octet_length(key), 0) + coalesce(octet_length(value), 0), \
             typeof(key) = 'text', 1, typeof(value) = 'text', 1, 1"
        }
    };
    let sql = format!("select rowid, {fields} from {table}{where_clause} order by rowid limit 1");
    let _guard = SqliteLengthPreflightGuard::new(connection);
    let mut statement = connection.prepare(&sql)?;
    let read = |row: &rusqlite::Row<'_>| {
        let bytes = row.get::<_, i64>(1)?;
        Ok(KiroCandidate {
            phase,
            rowid: row.get(0)?,
            row_ordinal,
            retained_bytes: u64::try_from(bytes).unwrap_or(u64::MAX),
            type_valid: [
                row.get::<_, i64>(2)? != 0,
                row.get::<_, i64>(3)? != 0,
                row.get::<_, i64>(4)? != 0,
                row.get::<_, i64>(5)? != 0,
                row.get::<_, i64>(6)? != 0,
            ],
        })
    };
    match after_rowid {
        Some(rowid) => statement
            .query_row([rowid], read)
            .optional()
            .map_err(Into::into),
        None => statement.query_row([], read).optional().map_err(Into::into),
    }
}

pub(super) fn candidate_at(
    connection: &Connection,
    phase: KiroPhase,
    rowid: i64,
    row_ordinal: u64,
) -> Result<Option<KiroCandidate>> {
    let candidate = match rowid.checked_sub(1) {
        Some(prior) => next_candidate(connection, phase, Some(prior), row_ordinal)?,
        None => next_candidate(connection, phase, None, row_ordinal)?,
    };
    Ok(candidate.filter(|candidate| candidate.rowid == rowid))
}

pub(super) fn hydrate_row(
    connection: &Connection,
    phase: KiroPhase,
    rowid: i64,
) -> Result<KiroConversationRow> {
    match phase {
        KiroPhase::V2 => connection.query_row(
            "select rowid, key, conversation_id, value, created_at, updated_at \
             from conversations_v2 where rowid = ?1",
            [rowid],
            |row| {
                Ok(KiroConversationRow {
                    table: "conversations_v2",
                    rowid: row.get(0)?,
                    key: row.get(1)?,
                    conversation_id: Some(row.get(2)?),
                    value: row.get(3)?,
                    created_at: row.get(4)?,
                    updated_at: row.get(5)?,
                })
            },
        ),
        KiroPhase::Legacy => connection.query_row(
            "select rowid, key, value from conversations where rowid = ?1",
            [rowid],
            |row| {
                Ok(KiroConversationRow {
                    table: "conversations",
                    rowid: row.get(0)?,
                    key: row.get(1)?,
                    conversation_id: None,
                    value: row.get(2)?,
                    created_at: None,
                    updated_at: None,
                })
            },
        ),
    }
    .map_err(Into::into)
}
