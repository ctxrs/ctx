use rusqlite::{types::ValueRef, Connection, OptionalExtension};

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

pub(super) struct CrushCandidate {
    pub(super) rowid: i64,
    pub(super) observed_bytes: u64,
}

pub(super) fn next_candidate(
    conn: &Connection,
    schema: &CrushNativeSchema,
    frontier: &CrushNativeFrontier,
) -> Result<Option<CrushCandidate>> {
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
                return Ok(None);
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
                return Ok(None);
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
    let sql = format!("select {rowid}, {retained} from {from}{after} order by {rowid} limit 1");
    let _guard = SqliteLengthPreflightGuard::new(conn);
    let read = |row: &rusqlite::Row<'_>| {
        let rowid = row.get::<_, i64>(0)?;
        let retained = row.get::<_, i64>(1)?;
        Ok((rowid, retained))
    };
    let candidate = match frontier.after_rowid {
        Some(rowid) => conn.query_row(&sql, [rowid], read).optional()?,
        None => conn.query_row(&sql, [], read).optional()?,
    };
    let Some((rowid, retained)) = candidate else {
        return Ok(None);
    };
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
    Ok(Some(CrushCandidate {
        rowid,
        observed_bytes,
    }))
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
