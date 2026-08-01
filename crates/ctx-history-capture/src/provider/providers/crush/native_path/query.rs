use std::collections::HashMap;

use rusqlite::{params_from_iter, types::ValueRef, Connection};

use crate::{
    native_source::NativeSqliteValue, provider::sqlite::SqliteLengthPreflightGuard, CaptureError,
    Result,
};

use super::super::{
    capture::CRUSH_SQLITE_VALUE_OVERHEAD_BYTES,
    projection::{decode_message_child, decode_session_at, optional_text},
    source::{
        message_projection, message_session_join, optional_session_column, retained_length_expr,
        session_projection,
    },
};
use super::{CrushLoadedRow, CrushNativeFrontier, CrushNativeSchema};

pub(super) fn row_decode_error_is_local(error: &CaptureError) -> bool {
    match error {
        CaptureError::InvalidPayload(_) | CaptureError::Json(_) => true,
        CaptureError::Sqlite(error) => matches!(
            error,
            rusqlite::Error::FromSqlConversionFailure(..)
                | rusqlite::Error::IntegralValueOutOfRange(..)
                | rusqlite::Error::Utf8Error(..)
                | rusqlite::Error::InvalidColumnType(..)
        ),
        _ => false,
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct CrushCandidate {
    pub(super) rowid: i64,
    pub(super) observed_bytes: u64,
}

#[cfg(test)]
pub(super) fn next_candidate(
    conn: &Connection,
    schema: &CrushNativeSchema,
    frontier: &CrushNativeFrontier,
) -> Result<Option<CrushCandidate>> {
    Ok(next_candidate_batch(conn, schema, frontier, 1)?
        .into_iter()
        .next())
}

pub(super) fn next_candidate_batch(
    conn: &Connection,
    schema: &CrushNativeSchema,
    frontier: &CrushNativeFrontier,
    limit: usize,
) -> Result<Vec<CrushCandidate>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let rowid = "m.rowid";
    let local = retained_length_expr(
        &schema.message_columns,
        "m",
        &[
            "id",
            "session_id",
            "role",
            "parts",
            "created_at",
            "updated_at",
            "provider",
            "model",
            "is_summary_message",
        ],
    );
    let parent = retained_length_expr(
        &schema.session_columns,
        "s",
        &[
            "id",
            "parent_session_id",
            "title",
            "created_at",
            "updated_at",
            "prompt_tokens",
            "completion_tokens",
            "cost",
            "summary_message_id",
        ],
    );
    let retained = format!("{local} + {parent}");
    let from = message_session_join();
    let after = if frontier.after_rowid.is_some() {
        format!(" where {rowid} > ?1")
    } else {
        String::new()
    };
    let sql =
        format!("select {rowid}, {retained} from {from}{after} order by {rowid} limit {limit}");
    let _guard = SqliteLengthPreflightGuard::new(conn);
    let mut statement = conn.prepare(&sql)?;
    let read = |row: &rusqlite::Row<'_>| {
        let rowid = row.get::<_, i64>(0)?;
        let retained = row.get::<_, i64>(1)?;
        Ok((rowid, retained))
    };
    let rows = match frontier.after_rowid {
        Some(rowid) => statement.query_map([rowid], read)?,
        None => statement.query_map([], read)?,
    };
    rows.map(|row| {
        let (rowid, retained) = row?;
        if rowid <= 0 || retained < 0 {
            return Err(CaptureError::InvalidPayload(
                "Crush message keyset metadata is invalid".to_owned(),
            ));
        }
        let retained = u64::try_from(retained).map_err(|_| {
            CaptureError::InvalidPayload("Crush retained byte count is invalid".to_owned())
        })?;
        let observed_bytes = CRUSH_SQLITE_VALUE_OVERHEAD_BYTES
            .checked_add(retained)
            .ok_or(CaptureError::SystemInvariant(
                "Crush retained byte count overflowed",
            ))?;
        Ok(CrushCandidate {
            rowid,
            observed_bytes,
        })
    })
    .collect()
}

pub(super) fn load_message_batch(
    connection: &Connection,
    schema: &CrushNativeSchema,
    candidates: &[CrushCandidate],
) -> Result<HashMap<i64, Result<CrushLoadedRow>>> {
    if candidates.is_empty() {
        return Ok(HashMap::new());
    }
    let parent_created_at = optional_session_column(&schema.session_columns, "created_at");
    let parent_updated_at = optional_session_column(&schema.session_columns, "updated_at");
    let projection = message_projection(&schema.message_columns, "m");
    let session = session_projection(&schema.session_columns, "s");
    let placeholders = std::iter::repeat_n("?", candidates.len())
        .collect::<Vec<_>>()
        .join(", ");
    let mut statement = connection.prepare(&format!(
        "select m.rowid, s.rowid, {parent_created_at}, {parent_updated_at}, \
         {projection}, {session} \
         from {} where m.rowid in ({placeholders}) order by m.rowid",
        message_session_join()
    ))?;
    let rowids = candidates.iter().map(|candidate| candidate.rowid);
    let mut rows = statement.query(params_from_iter(rowids))?;
    let mut loaded = HashMap::with_capacity(candidates.len());
    while let Some(row) = rows.next()? {
        let rowid = row.get::<_, i64>(0)?;
        let values = raw_sqlite_values_offset(row, 1, 22)?;
        let decoded = (|| {
            let child = decode_message_child(&values[..13])?;
            let session = if child.parent_rowid.is_some() {
                let session = decode_session_at(&values, 13)?
                    .ok_or(CaptureError::SourceChangedDuringCapture)?;
                if session.id != child.message.session_id {
                    return Err(CaptureError::SourceChangedDuringCapture);
                }
                Some(session)
            } else {
                None
            };
            Ok(CrushLoadedRow {
                row: child.message,
                session,
                digest_values: values,
            })
        })();
        loaded.insert(rowid, decoded);
    }
    Ok(loaded)
}

pub(super) fn load_session_parents(
    connection: &Connection,
    columns: &std::collections::BTreeSet<String>,
) -> Result<HashMap<String, Option<String>>> {
    const PAGE: usize = 256;
    let parent = optional_session_column(columns, "parent_session_id");
    let mut after = 0_i64;
    let mut parents = HashMap::new();
    loop {
        let mut statement = connection.prepare(&format!(
            "select s.rowid, s.id, {parent} from sessions s \
             where s.rowid > ?1 order by s.rowid limit {PAGE}"
        ))?;
        let page = statement
            .query_map([after], |row| {
                Ok((row.get::<_, i64>(0)?, raw_sqlite_values_offset(row, 1, 2)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if page.is_empty() {
            break;
        }
        for (rowid, values) in page {
            after = rowid;
            let NativeSqliteValue::Text(id) = &values[0] else {
                continue;
            };
            parents.insert(id.clone(), optional_text(&values, 1)?);
        }
    }
    Ok(parents)
}

fn raw_sqlite_values_offset(
    row: &rusqlite::Row<'_>,
    offset: usize,
    count: usize,
) -> rusqlite::Result<Vec<NativeSqliteValue>> {
    (offset..offset + count)
        .map(|index| row.get_ref(index).map(raw_sqlite_value))
        .collect()
}

fn raw_sqlite_value(value: ValueRef<'_>) -> NativeSqliteValue {
    match value {
        ValueRef::Null => NativeSqliteValue::Null,
        ValueRef::Integer(value) => NativeSqliteValue::Integer(value),
        ValueRef::Real(value) => NativeSqliteValue::from_real(value),
        ValueRef::Text(value) => std::str::from_utf8(value).map_or_else(
            |_| NativeSqliteValue::Blob(value.to_vec()),
            |value| NativeSqliteValue::Text(value.to_owned()),
        ),
        ValueRef::Blob(value) => NativeSqliteValue::Blob(value.to_vec()),
    }
}
