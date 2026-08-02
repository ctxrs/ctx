use std::path::Path;

use rusqlite::{params_from_iter, types::ToSqlOutput, Connection, StatementStatus};

use super::{
    checked_add, decode_source_event_row, OpenCodeSourceBackedError, OpenCodeSourceBackedResult,
    SourceEventRow,
};
use crate::{
    provider::providers::opencode::{
        native_path::{
            model::OpenCodeNativeSchemaFamily,
            query::{
                source_backed_fallback_event_by_rowid_sql, source_backed_fallback_sort_key_sql,
                source_backed_indexed_message_ids_sql, source_backed_indexed_part_rowids_sql,
            },
            schema::OpenCodeNativeSchema,
        },
        OpenCodeSqliteDialect,
    },
    provider_sources::SqliteSourceAccessError,
    CaptureError,
};

pub(super) const OPENCODE_FALLBACK_SCRATCH_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const OPENCODE_FALLBACK_ORDER_TABLE: &str = "create table opencode_fallback_order (
         source_rowid integer primary key,
         session_identity text not null,
         message_time integer not null,
         message_identity text not null,
         part_time integer not null,
         part_identity text not null
     );
     create index opencode_fallback_order_idx on opencode_fallback_order (
         session_identity collate binary,
         message_time,
         message_identity collate binary,
         part_time,
         part_identity collate binary,
         source_rowid
     );
     begin immediate";
const OPENCODE_FALLBACK_ORDER_INSERT: &str =
    "insert into opencode_fallback_order values (?1, ?2, ?3, ?4, ?5, ?6)";
const OPENCODE_FALLBACK_ORDER_SCAN: &str = "select source_rowid
       from opencode_fallback_order indexed by opencode_fallback_order_idx
      order by session_identity collate binary,
               message_time,
               message_identity collate binary,
               part_time,
               part_identity collate binary,
               source_rowid";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct FallbackSortStats {
    pub(super) rows: u64,
    pub(super) scratch_bytes: u64,
}

pub(super) fn message_part_requires_external_order(
    connection: &Connection,
    schema: &OpenCodeNativeSchema,
) -> OpenCodeSourceBackedResult<bool> {
    if schema.family != OpenCodeNativeSchemaFamily::MessagePart {
        return Ok(false);
    }
    if !schema.message_part_indexed_streaming {
        return Ok(true);
    }
    if query_plan_uses_temp_sort(connection, source_backed_indexed_message_ids_sql())? {
        return Ok(true);
    }
    Ok(query_plan_with_null_uses_temp_sort(
        connection,
        source_backed_indexed_part_rowids_sql(),
    )?)
}

fn query_plan_uses_temp_sort(connection: &Connection, sql: &str) -> rusqlite::Result<bool> {
    let explain = format!("EXPLAIN QUERY PLAN {sql}");
    let mut statement = connection.prepare(&explain)?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        if row.get::<_, String>(3)?.contains("USE TEMP B-TREE") {
            return Ok(true);
        }
    }
    Ok(false)
}

fn query_plan_with_null_uses_temp_sort(
    connection: &Connection,
    sql: &str,
) -> rusqlite::Result<bool> {
    let explain = format!("EXPLAIN QUERY PLAN {sql}");
    let mut statement = connection.prepare(&explain)?;
    let mut rows = statement.query([rusqlite::types::Null])?;
    while let Some(row) = rows.next()? {
        if row.get::<_, String>(3)?.contains("USE TEMP B-TREE") {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn stream_indexed_message_part_events(
    source: &Connection,
    schema: &OpenCodeNativeSchema,
    dialect: &OpenCodeSqliteDialect,
    consume_event: &mut dyn FnMut(SourceEventRow) -> OpenCodeSourceBackedResult<()>,
) -> OpenCodeSourceBackedResult<()> {
    if !schema.message_part_indexed_streaming {
        return Err(CaptureError::SystemInvariant(
            "OpenCode direct message-part stream lacks its required provider indexes",
        )
        .into());
    }
    let mut messages = source.prepare(source_backed_indexed_message_ids_sql())?;
    let mut parts = source.prepare(source_backed_indexed_part_rowids_sql())?;
    let point_sql = source_backed_fallback_event_by_rowid_sql(schema);
    let mut point = source.prepare(&point_sql)?;
    let mut message_rows = messages.query([])?;
    while let Some(message_row) = message_rows.next()? {
        let message_id = message_row.get::<_, String>(0)?;
        let mut part_rows = parts.query([message_id])?;
        while let Some(part_row) = part_rows.next()? {
            let source_rowid = part_row.get::<_, i64>(0)?;
            let mut source_rows = point.query([source_rowid])?;
            let source_row = source_rows.next()?.ok_or_else(|| {
                CaptureError::SystemInvariant(
                    "OpenCode indexed source row disappeared from its pinned snapshot",
                )
            })?;
            consume_event(decode_source_event_row(source_row, schema, dialect, None)?)?;
        }
    }
    drop(message_rows);
    if messages.get_status(StatementStatus::Sort) != 0
        || parts.get_status(StatementStatus::Sort) != 0
    {
        return Err(CaptureError::SystemInvariant(
            "OpenCode indexed message-part stream unexpectedly used SQLite temporary sorting",
        )
        .into());
    }
    Ok(())
}

pub(super) fn stream_fallback_ordered_events(
    source: &Connection,
    scratch: &Connection,
    scratch_path: &Path,
    schema: &OpenCodeNativeSchema,
    dialect: &OpenCodeSqliteDialect,
    consume_event: &mut dyn FnMut(SourceEventRow) -> OpenCodeSourceBackedResult<()>,
) -> OpenCodeSourceBackedResult<FallbackSortStats> {
    scratch
        .execute_batch(OPENCODE_FALLBACK_ORDER_TABLE)
        .map_err(|source| {
            private_scratch_error("creating the private OpenCode ordering index", source)
        })?;
    if query_plan_uses_temp_sort(source, source_backed_fallback_sort_key_sql(schema))? {
        return Err(CaptureError::SystemInvariant(
            "OpenCode fallback key discovery would use SQLite temporary sorting",
        )
        .into());
    }
    if query_plan_uses_temp_sort(scratch, OPENCODE_FALLBACK_ORDER_SCAN).map_err(|source| {
        private_scratch_error("verifying the private OpenCode ordering index", source)
    })? {
        return Err(CaptureError::SystemInvariant(
            "OpenCode private ordering index would use SQLite temporary sorting",
        )
        .into());
    }
    let mut sort_keys = source.prepare(source_backed_fallback_sort_key_sql(schema))?;
    let mut sort_key_rows = sort_keys.query([])?;
    let mut insert = scratch
        .prepare(OPENCODE_FALLBACK_ORDER_INSERT)
        .map_err(|source| {
            private_scratch_error("preparing the private OpenCode ordering index", source)
        })?;
    let mut inserted_rows = 0_u64;
    while let Some(row) = sort_key_rows.next()? {
        let values = [
            ToSqlOutput::Borrowed(row.get_ref(0)?),
            ToSqlOutput::Borrowed(row.get_ref(1)?),
            ToSqlOutput::Borrowed(row.get_ref(2)?),
            ToSqlOutput::Borrowed(row.get_ref(3)?),
            ToSqlOutput::Borrowed(row.get_ref(4)?),
            ToSqlOutput::Borrowed(row.get_ref(5)?),
        ];
        insert.execute(params_from_iter(values)).map_err(|source| {
            private_scratch_error("writing the private OpenCode ordering index", source)
        })?;
        inserted_rows = checked_add(inserted_rows, 1)?;
    }
    drop(sort_key_rows);
    if sort_keys.get_status(StatementStatus::Sort) != 0 {
        return Err(CaptureError::SystemInvariant(
            "OpenCode fallback key discovery unexpectedly used SQLite temporary sorting",
        )
        .into());
    }
    drop(insert);

    let page_count: i64 = scratch
        .pragma_query_value(None, "page_count", |row| row.get(0))
        .map_err(|source| {
            private_scratch_error("measuring the private OpenCode ordering index", source)
        })?;
    let page_size: i64 = scratch
        .pragma_query_value(None, "page_size", |row| row.get(0))
        .map_err(|source| {
            private_scratch_error("measuring the private OpenCode ordering pages", source)
        })?;
    let logical_scratch_bytes = u64::try_from(page_count)
        .ok()
        .and_then(|pages| {
            u64::try_from(page_size)
                .ok()
                .and_then(|size| pages.checked_mul(size))
        })
        .ok_or(OpenCodeSourceBackedError::CountOverflow)?;
    let scratch_bytes = std::fs::metadata(scratch_path)
        .map_err(|source| SqliteSourceAccessError::Io {
            operation: "measuring the private OpenCode ordering database",
            path: scratch_path.to_path_buf(),
            source,
        })?
        .len();
    if scratch_bytes > logical_scratch_bytes {
        return Err(CaptureError::SystemInvariant(
            "OpenCode private ordering database exceeded its SQLite page allocation",
        )
        .into());
    }

    let point_sql = source_backed_fallback_event_by_rowid_sql(schema);
    let mut point = source.prepare(&point_sql)?;
    let mut ordered = scratch
        .prepare(OPENCODE_FALLBACK_ORDER_SCAN)
        .map_err(|source| {
            private_scratch_error("reading the private OpenCode ordering index", source)
        })?;
    let mut ordered_rows = ordered.query([]).map_err(|source| {
        private_scratch_error("opening the private OpenCode ordered row stream", source)
    })?;
    let mut hydrated_rows = 0_u64;
    while let Some(row) = ordered_rows.next().map_err(|source| {
        private_scratch_error("streaming the private OpenCode ordering index", source)
    })? {
        let source_rowid = row.get::<_, i64>(0).map_err(|source| {
            private_scratch_error("decoding the private OpenCode ordering index", source)
        })?;
        let mut source_rows = point.query([source_rowid])?;
        let source_row = source_rows.next()?.ok_or_else(|| {
            CaptureError::SystemInvariant(
                "OpenCode fallback source row disappeared from its pinned snapshot",
            )
        })?;
        let event = decode_source_event_row(source_row, schema, dialect, None)?;
        drop(source_rows);
        consume_event(event)?;
        hydrated_rows = checked_add(hydrated_rows, 1)?;
    }
    drop(ordered_rows);
    if ordered.get_status(StatementStatus::Sort) != 0 {
        return Err(CaptureError::SystemInvariant(
            "OpenCode private ordering index unexpectedly used SQLite temporary sorting",
        )
        .into());
    }
    if hydrated_rows != inserted_rows {
        return Err(CaptureError::SystemInvariant(
            "OpenCode private ordering index did not preserve every source row",
        )
        .into());
    }
    Ok(FallbackSortStats {
        rows: hydrated_rows,
        scratch_bytes,
    })
}

fn private_scratch_error(
    operation: &'static str,
    source: rusqlite::Error,
) -> OpenCodeSourceBackedError {
    SqliteSourceAccessError::Sqlite { operation, source }.into()
}
