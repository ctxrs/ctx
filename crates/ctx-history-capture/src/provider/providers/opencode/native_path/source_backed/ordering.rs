use std::{collections::HashMap, path::Path};

use rusqlite::{params_from_iter, types::Value, Connection, StatementStatus};

use super::{
    checked_add, decode_source_event_row, OpenCodeSourceBackedError, OpenCodeSourceBackedResult,
    SourceEventRow,
};
use crate::{
    provider::providers::opencode::{
        native_path::{
            query::{
                source_backed_fallback_events_by_rowids_sql, source_backed_fallback_sort_key_sql,
            },
            schema::OpenCodeNativeSchema,
        },
        OpenCodeSqliteDialect,
    },
    provider_sources::SqliteSourceAccessError,
    CaptureError,
};

pub(super) const OPENCODE_FALLBACK_SCRATCH_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub(super) const OPENCODE_HYDRATION_BATCH_ROWS: usize = 64;
pub(super) const OPENCODE_HYDRATION_BATCH_BYTES: u64 = 8 * 1024 * 1024;
const OPENCODE_SORT_KEY_BATCH_ROWS: usize = 64;
const OPENCODE_SORT_KEY_BATCH_BYTES: usize = 1024 * 1024;
const OPENCODE_SORT_KEY_COLUMNS: usize = 7;
const OPENCODE_FALLBACK_ORDER_TABLE: &str = "create table opencode_fallback_order (
         source_rowid integer primary key,
         session_identity text not null,
         message_time integer not null,
         message_identity text not null,
         part_time integer not null,
         part_identity text not null,
         payload_bytes integer not null
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
const OPENCODE_FALLBACK_ORDER_SCAN: &str = "select source_rowid, payload_bytes
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
    pub(super) data_statements: u64,
    pub(super) sort_key_batches: u64,
    pub(super) hydration_batches: u64,
    pub(super) max_sort_key_batch_rows: u64,
    pub(super) max_hydration_batch_rows: u64,
    pub(super) max_hydration_batch_bytes: u64,
}

#[derive(Clone, Copy)]
struct HydrationRequest {
    source_rowid: i64,
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
    let maximum_hydration_sql =
        source_backed_fallback_events_by_rowids_sql(schema, OPENCODE_HYDRATION_BATCH_ROWS);
    if query_plan_with_nulls_uses_temp_sort(
        source,
        &maximum_hydration_sql,
        OPENCODE_HYDRATION_BATCH_ROWS,
    )? {
        return Err(CaptureError::SystemInvariant(
            "OpenCode batched payload hydration would use SQLite temporary sorting",
        )
        .into());
    }

    let mut stats = FallbackSortStats {
        data_statements: 1,
        ..FallbackSortStats::default()
    };
    let mut inserted_rows = 0_u64;
    let mut sort_keys = source.prepare(source_backed_fallback_sort_key_sql(schema))?;
    let mut sort_key_rows = sort_keys.query([])?;
    let mut pending_keys = Vec::<Vec<Value>>::with_capacity(OPENCODE_SORT_KEY_BATCH_ROWS);
    let mut pending_key_bytes = 0_usize;
    while let Some(row) = sort_key_rows.next()? {
        let values = (0..OPENCODE_SORT_KEY_COLUMNS)
            .map(|column| row.get::<_, Value>(column))
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let row_bytes = values.iter().map(value_memory_bytes).sum::<usize>();
        if !pending_keys.is_empty()
            && (pending_keys.len() == OPENCODE_SORT_KEY_BATCH_ROWS
                || pending_key_bytes.saturating_add(row_bytes) > OPENCODE_SORT_KEY_BATCH_BYTES)
        {
            insert_sort_key_batch(scratch, &pending_keys)?;
            inserted_rows = checked_add(
                inserted_rows,
                u64::try_from(pending_keys.len())
                    .map_err(|_| OpenCodeSourceBackedError::CountOverflow)?,
            )?;
            record_sort_key_batch(&mut stats, pending_keys.len())?;
            pending_keys.clear();
            pending_key_bytes = 0;
        }
        pending_key_bytes = pending_key_bytes.saturating_add(row_bytes);
        pending_keys.push(values);
    }
    if !pending_keys.is_empty() {
        insert_sort_key_batch(scratch, &pending_keys)?;
        inserted_rows = checked_add(
            inserted_rows,
            u64::try_from(pending_keys.len())
                .map_err(|_| OpenCodeSourceBackedError::CountOverflow)?,
        )?;
        record_sort_key_batch(&mut stats, pending_keys.len())?;
    }
    drop(sort_key_rows);
    if sort_keys.get_status(StatementStatus::Sort) != 0 {
        return Err(CaptureError::SystemInvariant(
            "OpenCode fallback key discovery unexpectedly used SQLite temporary sorting",
        )
        .into());
    }

    stats.scratch_bytes = measure_scratch_database(scratch, scratch_path)?;
    stats.data_statements = checked_add(stats.data_statements, 1)?;
    let mut ordered = scratch
        .prepare(OPENCODE_FALLBACK_ORDER_SCAN)
        .map_err(|source| {
            private_scratch_error("reading the private OpenCode ordering index", source)
        })?;
    let mut ordered_rows = ordered.query([]).map_err(|source| {
        private_scratch_error("opening the private OpenCode ordered row stream", source)
    })?;
    let mut pending = Vec::with_capacity(OPENCODE_HYDRATION_BATCH_ROWS);
    let mut pending_bytes = 0_u64;
    while let Some(row) = ordered_rows.next().map_err(|source| {
        private_scratch_error("streaming the private OpenCode ordering index", source)
    })? {
        let source_rowid = row.get::<_, i64>(0).map_err(|source| {
            private_scratch_error("decoding the private OpenCode ordering index", source)
        })?;
        let payload_bytes = row.get::<_, i64>(1).map_err(|source| {
            private_scratch_error("decoding the private OpenCode payload bound", source)
        })?;
        let payload_bytes = u64::try_from(payload_bytes).map_err(|_| {
            CaptureError::SystemInvariant("OpenCode ordering payload bound became negative")
        })?;
        if !pending.is_empty()
            && (pending.len() == OPENCODE_HYDRATION_BATCH_ROWS
                || pending_bytes.saturating_add(payload_bytes) > OPENCODE_HYDRATION_BATCH_BYTES)
        {
            hydrate_requested_events(source, schema, dialect, &pending, consume_event)?;
            record_hydration_batch(&mut stats, pending.len(), pending_bytes)?;
            pending.clear();
            pending_bytes = 0;
        }
        pending.push(HydrationRequest { source_rowid });
        pending_bytes = pending_bytes.saturating_add(payload_bytes);
    }
    if !pending.is_empty() {
        hydrate_requested_events(source, schema, dialect, &pending, consume_event)?;
        record_hydration_batch(&mut stats, pending.len(), pending_bytes)?;
    }
    drop(ordered_rows);
    if ordered.get_status(StatementStatus::Sort) != 0 {
        return Err(CaptureError::SystemInvariant(
            "OpenCode private ordering index unexpectedly used SQLite temporary sorting",
        )
        .into());
    }
    if stats.rows != inserted_rows {
        return Err(CaptureError::SystemInvariant(
            "OpenCode private ordering index did not preserve every source row",
        )
        .into());
    }
    Ok(stats)
}

fn insert_sort_key_batch(
    scratch: &Connection,
    rows: &[Vec<Value>],
) -> OpenCodeSourceBackedResult<()> {
    let mut parameter = 1_usize;
    let tuples = rows
        .iter()
        .map(|row| {
            debug_assert_eq!(row.len(), OPENCODE_SORT_KEY_COLUMNS);
            let tuple = (0..OPENCODE_SORT_KEY_COLUMNS)
                .map(|_| {
                    let placeholder = format!("?{parameter}");
                    parameter += 1;
                    placeholder
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("({tuple})")
        })
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!("insert into opencode_fallback_order values {tuples}");
    scratch
        .execute(&sql, params_from_iter(rows.iter().flatten()))
        .map(|_| ())
        .map_err(|source| {
            private_scratch_error("writing the private OpenCode ordering index", source)
        })
}

fn hydrate_requested_events(
    source: &Connection,
    schema: &OpenCodeNativeSchema,
    dialect: &OpenCodeSqliteDialect,
    requests: &[HydrationRequest],
    consume_event: &mut dyn FnMut(SourceEventRow) -> OpenCodeSourceBackedResult<()>,
) -> OpenCodeSourceBackedResult<()> {
    let sql = source_backed_fallback_events_by_rowids_sql(schema, requests.len());
    let mut point = source.prepare(&sql)?;
    let mut source_rows = point.query(params_from_iter(
        requests.iter().map(|request| request.source_rowid),
    ))?;
    let mut events = HashMap::with_capacity(requests.len());
    while let Some(source_row) = source_rows.next()? {
        let source_rowid = source_row.get::<_, i64>(11)?;
        let event = decode_source_event_row(source_row, schema, dialect, None)?;
        if events.insert(source_rowid, event).is_some() {
            return Err(CaptureError::SystemInvariant(
                "OpenCode batched payload hydration duplicated a source row",
            )
            .into());
        }
    }
    drop(source_rows);
    if point.get_status(StatementStatus::Sort) != 0 {
        return Err(CaptureError::SystemInvariant(
            "OpenCode batched payload hydration unexpectedly used SQLite temporary sorting",
        )
        .into());
    }
    for request in requests {
        let event = events.remove(&request.source_rowid).ok_or_else(|| {
            CaptureError::SystemInvariant(
                "OpenCode fallback source row disappeared from its pinned snapshot",
            )
        })?;
        consume_event(event)?;
    }
    if !events.is_empty() {
        return Err(CaptureError::SystemInvariant(
            "OpenCode batched payload hydration returned an unrequested source row",
        )
        .into());
    }
    Ok(())
}

fn record_sort_key_batch(
    stats: &mut FallbackSortStats,
    rows: usize,
) -> OpenCodeSourceBackedResult<()> {
    let rows = u64::try_from(rows).map_err(|_| OpenCodeSourceBackedError::CountOverflow)?;
    stats.sort_key_batches = checked_add(stats.sort_key_batches, 1)?;
    stats.data_statements = checked_add(stats.data_statements, 1)?;
    stats.max_sort_key_batch_rows = stats.max_sort_key_batch_rows.max(rows);
    Ok(())
}

fn record_hydration_batch(
    stats: &mut FallbackSortStats,
    rows: usize,
    bytes: u64,
) -> OpenCodeSourceBackedResult<()> {
    let rows = u64::try_from(rows).map_err(|_| OpenCodeSourceBackedError::CountOverflow)?;
    stats.rows = checked_add(stats.rows, rows)?;
    stats.hydration_batches = checked_add(stats.hydration_batches, 1)?;
    stats.data_statements = checked_add(stats.data_statements, 1)?;
    stats.max_hydration_batch_rows = stats.max_hydration_batch_rows.max(rows);
    stats.max_hydration_batch_bytes = stats.max_hydration_batch_bytes.max(bytes);
    Ok(())
}

fn measure_scratch_database(
    scratch: &Connection,
    scratch_path: &Path,
) -> OpenCodeSourceBackedResult<u64> {
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
    Ok(scratch_bytes)
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

fn query_plan_with_nulls_uses_temp_sort(
    connection: &Connection,
    sql: &str,
    parameters: usize,
) -> rusqlite::Result<bool> {
    let explain = format!("EXPLAIN QUERY PLAN {sql}");
    let mut statement = connection.prepare(&explain)?;
    let mut rows = statement.query(params_from_iter(std::iter::repeat_n(
        rusqlite::types::Null,
        parameters,
    )))?;
    while let Some(row) = rows.next()? {
        if row.get::<_, String>(3)?.contains("USE TEMP B-TREE") {
            return Ok(true);
        }
    }
    Ok(false)
}

fn value_memory_bytes(value: &Value) -> usize {
    match value {
        Value::Text(value) => value.len(),
        Value::Blob(value) => value.len(),
        Value::Null | Value::Integer(_) | Value::Real(_) => std::mem::size_of::<Value>(),
    }
}

fn private_scratch_error(
    operation: &'static str,
    source: rusqlite::Error,
) -> OpenCodeSourceBackedError {
    SqliteSourceAccessError::Sqlite { operation, source }.into()
}
