use std::collections::HashMap;

use rusqlite::{params_from_iter, types::ValueRef, Connection};

use crate::{
    native_source::NativeSqliteValue, provider::sqlite::SqliteLengthPreflightGuard, CaptureError,
    Result,
};

use super::super::{
    capture::CRUSH_SQLITE_VALUE_OVERHEAD_BYTES,
    projection::{
        decode_file, decode_message_child, decode_read_file, decode_session, optional_text,
        CrushChildMessageRow, CrushSessionRow,
    },
    source::{
        file_projection, message_projection, message_session_join, optional_session_column,
        read_file_projection, retained_length_expr, session_projection,
    },
};
use super::{
    CrushHydratedRow, CrushNativeFrontier, CrushNativePhase, CrushNativeSchema,
    CRUSH_NATIVE_PAGE_OVERHEAD_BYTES,
};

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
    let (rowid, retained, from) = match frontier.phase {
        CrushNativePhase::Sessions => (
            "s.rowid".to_owned(),
            retained_length_expr(
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
            ),
            "sessions s".to_owned(),
        ),
        CrushNativePhase::Messages => {
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
                &["parent_session_id", "created_at", "updated_at"],
            );
            (
                "m.rowid".to_owned(),
                format!("{local} + {parent}"),
                message_session_join().to_owned(),
            )
        }
        CrushNativePhase::Files => {
            let Some(columns) = schema
                .file_columns
                .as_ref()
                .filter(|columns| columns.contains("session_id"))
            else {
                return Ok(Vec::new());
            };
            (
                "f.rowid".to_owned(),
                retained_length_expr(
                    columns,
                    "f",
                    &["session_id", "path", "version", "created_at", "updated_at"],
                ),
                "files f".to_owned(),
            )
        }
        CrushNativePhase::ReadFiles => {
            let Some(columns) = schema.read_file_columns.as_ref() else {
                return Ok(Vec::new());
            };
            (
                "r.rowid".to_owned(),
                retained_length_expr(columns, "r", &["session_id", "path", "read_at"]),
                "read_files r".to_owned(),
            )
        }
    };
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
            return Err(CaptureError::InvalidPayload(format!(
                "Crush {} keyset metadata is invalid",
                frontier.phase.label()
            )));
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

pub(super) fn hydrate_message_batch(
    connection: &Connection,
    schema: &CrushNativeSchema,
    candidates: &[CrushCandidate],
) -> Result<HashMap<i64, Result<CrushHydratedRow>>> {
    if candidates.is_empty() {
        return Ok(HashMap::new());
    }
    let parent_created_at = optional_session_column(&schema.session_columns, "created_at");
    let parent_updated_at = optional_session_column(&schema.session_columns, "updated_at");
    let parent_session_id = optional_session_column(&schema.session_columns, "parent_session_id");
    let projection = message_projection(&schema.message_columns, "m");
    let placeholders = std::iter::repeat_n("?", candidates.len())
        .collect::<Vec<_>>()
        .join(", ");
    let mut statement = connection.prepare(&format!(
        "select m.rowid, s.rowid, {parent_created_at}, {parent_updated_at}, \
         {projection}, {parent_session_id} \
         from {} where m.rowid in ({placeholders}) order by m.rowid",
        message_session_join()
    ))?;
    let observed = candidates
        .iter()
        .map(|candidate| (candidate.rowid, candidate.observed_bytes))
        .collect::<HashMap<_, _>>();
    let rowids = candidates.iter().map(|candidate| candidate.rowid);
    let mut rows = statement.query(params_from_iter(rowids))?;
    let mut hydrated = HashMap::with_capacity(candidates.len());
    while let Some(row) = rows.next()? {
        let rowid = row.get::<_, i64>(0)?;
        let mut values = raw_sqlite_values_offset(row, 1, 14)?;
        let decoded = (|| {
            let parent_session_id = optional_text(&values, 13)?;
            values.pop();
            let child = decode_message_child(&values[..13])?;
            let retained_bytes = usize::try_from(
                *observed
                    .get(&rowid)
                    .ok_or(CaptureError::SourceChangedDuringCapture)?,
            )
            .unwrap_or(usize::MAX)
            .saturating_add(CRUSH_NATIVE_PAGE_OVERHEAD_BYTES);
            let session = child.parent_rowid.map(|_| CrushSessionRow {
                id: child.message.session_id.clone(),
                parent_session_id,
                title: None,
                created_at: child.parent_created_at,
                updated_at: child.parent_updated_at,
                prompt_tokens: None,
                completion_tokens: None,
                cost: None,
                summary_message_id: None,
            });
            Ok(CrushHydratedRow::Message {
                row: child.message,
                session,
                digest_values: values,
                retained_bytes,
            })
        })();
        hydrated.insert(rowid, decoded);
    }
    Ok(hydrated)
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

pub(super) fn hydrate_row_from_connection(
    connection: &Connection,
    schema: &CrushNativeSchema,
    phase: CrushNativePhase,
    rowid: i64,
    observed_bytes: u64,
) -> Result<CrushHydratedRow> {
    let retained_bytes = usize::try_from(observed_bytes)
        .unwrap_or(usize::MAX)
        .saturating_add(CRUSH_NATIVE_PAGE_OVERHEAD_BYTES);
    match phase {
        CrushNativePhase::Sessions => {
            let projection = session_projection(&schema.session_columns, "s");
            let values = connection.query_row(
                &format!("select s.rowid, {projection} from sessions s where s.rowid = ?1"),
                [rowid],
                |row| raw_sqlite_values(row, 10),
            )?;
            Ok(CrushHydratedRow::Session {
                row: decode_session(&values)?,
                retained_bytes,
            })
        }
        CrushNativePhase::Messages => {
            let parent_created_at = optional_session_column(&schema.session_columns, "created_at");
            let parent_updated_at = optional_session_column(&schema.session_columns, "updated_at");
            let projection = message_projection(&schema.message_columns, "m");
            let values = connection.query_row(
                &format!(
                    "select s.rowid, {parent_created_at}, \
                     {parent_updated_at}, {projection} \
                     from {} \
                     where m.rowid = ?1",
                    message_session_join()
                ),
                [rowid],
                |row| raw_sqlite_values(row, 13),
            )?;
            let child = decode_message_child(&values)?;
            let session = message_parent_session(connection, &schema.session_columns, &child)?;
            Ok(CrushHydratedRow::Message {
                row: child.message,
                session,
                digest_values: values,
                retained_bytes,
            })
        }
        CrushNativePhase::Files => {
            let columns = schema
                .file_columns
                .as_ref()
                .ok_or(CaptureError::SystemInvariant(
                    "Crush file phase has no schema",
                ))?;
            let projection = file_projection(columns, "f");
            let values = connection.query_row(
                &format!("select {projection} from files f where f.rowid = ?1"),
                [rowid],
                |row| raw_sqlite_values(row, 6),
            )?;
            Ok(CrushHydratedRow::File {
                row: decode_file(&values)?,
                retained_bytes,
            })
        }
        CrushNativePhase::ReadFiles => {
            let columns =
                schema
                    .read_file_columns
                    .as_ref()
                    .ok_or(CaptureError::SystemInvariant(
                        "Crush read-file phase has no schema",
                    ))?;
            let projection = read_file_projection(columns, "r");
            let values = connection.query_row(
                &format!("select {projection} from read_files r where r.rowid = ?1"),
                [rowid],
                |row| raw_sqlite_values(row, 4),
            )?;
            Ok(CrushHydratedRow::ReadFile {
                row: decode_read_file(&values)?,
                retained_bytes,
            })
        }
    }
}

// Keep the full NativePath row decoder linked for the direct single-row test
// seam while production source-backed hydration uses bounded message sets.
const _: fn(
    &Connection,
    &CrushNativeSchema,
    CrushNativePhase,
    i64,
    u64,
) -> Result<CrushHydratedRow> = hydrate_row_from_connection;

fn message_parent_session(
    conn: &Connection,
    columns: &std::collections::BTreeSet<String>,
    child: &CrushChildMessageRow,
) -> Result<Option<CrushSessionRow>> {
    let Some(parent_rowid) = child.parent_rowid else {
        return Ok(None);
    };
    let parent_session_id = if columns.contains("parent_session_id") {
        let values = conn.query_row(
            "select parent_session_id from sessions where rowid = ?1",
            [parent_rowid],
            |row| raw_sqlite_values(row, 1),
        )?;
        optional_text(&values, 0)?
    } else {
        None
    };
    Ok(Some(CrushSessionRow {
        id: child.message.session_id.clone(),
        parent_session_id,
        title: None,
        created_at: child.parent_created_at,
        updated_at: child.parent_updated_at,
        prompt_tokens: None,
        completion_tokens: None,
        cost: None,
        summary_message_id: None,
    }))
}

fn raw_sqlite_values(
    row: &rusqlite::Row<'_>,
    count: usize,
) -> rusqlite::Result<Vec<NativeSqliteValue>> {
    (0..count)
        .map(|index| row.get_ref(index).map(raw_sqlite_value))
        .collect()
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
