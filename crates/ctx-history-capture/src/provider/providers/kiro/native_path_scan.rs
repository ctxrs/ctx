use std::{collections::HashMap, path::Path};

use rusqlite::{
    params_from_iter,
    types::{Value, ValueRef},
    Connection, Row, StatementStatus,
};

use crate::{
    provider_sources::SqliteSourceAccessError, CaptureError, MAX_PROVIDER_SQLITE_VALUE_BYTES,
};

use super::{
    super::history::KiroConversationRow,
    source_backed::{checked_add, KiroSourceBackedErrorV0, KiroSourceBackedResultV0},
    KiroPhase,
};

pub(super) const KIRO_ORDER_SCRATCH_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub(super) const KIRO_HYDRATION_BATCH_ROWS: usize = 64;
pub(super) const KIRO_HYDRATION_BATCH_BYTES: u64 = 8 * 1024 * 1024;
pub(super) const KIRO_KEY_BATCH_ROWS: usize = 64;
const KIRO_KEY_BATCH_BYTES: usize = 1024 * 1024;
const KIRO_KEY_COLUMNS: usize = 4;
const KIRO_ORDER_TABLE: &str = "create table kiro_row_order (
         source_rowid integer primary key,
         storage_class text not null,
         native_key,
         payload_bytes integer not null
     );
     create index kiro_row_order_idx on kiro_row_order (
         storage_class collate binary,
         native_key collate binary,
         source_rowid
     );
     begin immediate";
const KIRO_ORDER_SCAN: &str = "select source_rowid, payload_bytes
       from kiro_row_order indexed by kiro_row_order_idx
      order by storage_class collate binary, native_key collate binary, source_rowid";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct KiroOrderingStats {
    pub(super) rows: u64,
    pub(super) phases: u64,
    pub(super) scratch_bytes: u64,
    pub(super) data_statements: u64,
    pub(super) key_batches: u64,
    pub(super) hydration_batches: u64,
    pub(super) max_key_batch_rows: u64,
    pub(super) max_hydration_batch_rows: u64,
    pub(super) max_hydration_batch_bytes: u64,
}

pub(super) struct KiroRowOrderer<'a> {
    source: &'a Connection,
    scratch: &'a Connection,
    scratch_path: &'a Path,
    stats: KiroOrderingStats,
}

impl<'a> KiroRowOrderer<'a> {
    pub(super) fn new(
        source: &'a Connection,
        scratch: &'a Connection,
        scratch_path: &'a Path,
    ) -> KiroSourceBackedResultV0<Self> {
        scratch.execute_batch(KIRO_ORDER_TABLE).map_err(|source| {
            private_scratch_error("creating the private Kiro ordering index", source)
        })?;
        if query_plan_uses_temp_sort(scratch, KIRO_ORDER_SCAN).map_err(|source| {
            private_scratch_error("verifying the private Kiro ordering index", source)
        })? {
            return Err(CaptureError::SystemInvariant(
                "Kiro private ordering index would use SQLite temporary sorting",
            )
            .into());
        }
        Ok(Self {
            source,
            scratch,
            scratch_path,
            stats: KiroOrderingStats::default(),
        })
    }

    pub(super) fn stream_rows(
        &mut self,
        phase: KiroPhase,
        visit: &mut dyn FnMut(KiroConversationRow) -> KiroSourceBackedResultV0<()>,
    ) -> KiroSourceBackedResultV0<u64> {
        self.stats.phases = checked_add(self.stats.phases, 1)?;
        self.scratch
            .execute("delete from kiro_row_order", [])
            .map_err(|source| {
                private_scratch_error("resetting the private Kiro ordering index", source)
            })?;
        self.stats.data_statements = checked_add(self.stats.data_statements, 1)?;
        let key_sql = sort_key_sql(phase);
        if query_plan_uses_temp_sort(self.source, &key_sql)? {
            return Err(CaptureError::SystemInvariant(
                "Kiro source key discovery would use SQLite temporary sorting",
            )
            .into());
        }
        let hydration_sql = hydration_sql(phase, KIRO_HYDRATION_BATCH_ROWS);
        if query_plan_with_nulls_uses_temp_sort(
            self.source,
            &hydration_sql,
            KIRO_HYDRATION_BATCH_ROWS,
        )? {
            return Err(CaptureError::SystemInvariant(
                "Kiro batched payload hydration would use SQLite temporary sorting",
            )
            .into());
        }

        self.populate_order_index(&key_sql)?;
        let before = self.stats.rows;
        self.stream_ordered_payloads(phase, visit)?;
        let decoded = self
            .stats
            .rows
            .checked_sub(before)
            .ok_or(KiroSourceBackedErrorV0::CountOverflow)?;
        self.stats.data_statements = checked_add(self.stats.data_statements, 1)?;
        let indexed: i64 = self
            .scratch
            .query_row("select count(*) from kiro_row_order", [], |row| row.get(0))
            .map_err(|source| {
                private_scratch_error("counting the private Kiro ordering index", source)
            })?;
        if u64::try_from(indexed) != Ok(decoded) {
            return Err(CaptureError::SystemInvariant(
                "Kiro private ordering index did not preserve every source row",
            )
            .into());
        }
        Ok(decoded)
    }

    pub(super) fn finish(mut self) -> KiroSourceBackedResultV0<KiroOrderingStats> {
        self.stats.scratch_bytes = measure_scratch_database(self.scratch, self.scratch_path)?;
        Ok(self.stats)
    }

    fn populate_order_index(&mut self, sql: &str) -> KiroSourceBackedResultV0<()> {
        self.stats.data_statements = checked_add(self.stats.data_statements, 1)?;
        let mut statement = self.source.prepare(sql)?;
        let mut rows = statement.query([])?;
        let mut pending = Vec::<Vec<Value>>::with_capacity(KIRO_KEY_BATCH_ROWS);
        let mut pending_bytes = 0_usize;
        while let Some(row) = rows.next()? {
            let values = (0..KIRO_KEY_COLUMNS)
                .map(|column| row.get::<_, Value>(column))
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let row_bytes = values.iter().map(value_memory_bytes).sum::<usize>();
            if !pending.is_empty()
                && (pending.len() == KIRO_KEY_BATCH_ROWS
                    || pending_bytes.saturating_add(row_bytes) > KIRO_KEY_BATCH_BYTES)
            {
                self.insert_key_batch(&pending)?;
                pending.clear();
                pending_bytes = 0;
            }
            pending_bytes = pending_bytes.saturating_add(row_bytes);
            pending.push(values);
        }
        if !pending.is_empty() {
            self.insert_key_batch(&pending)?;
        }
        drop(rows);
        if statement.get_status(StatementStatus::Sort) != 0 {
            return Err(CaptureError::SystemInvariant(
                "Kiro source key discovery unexpectedly used SQLite temporary sorting",
            )
            .into());
        }
        Ok(())
    }

    fn insert_key_batch(&mut self, rows: &[Vec<Value>]) -> KiroSourceBackedResultV0<()> {
        let mut parameter = 1_usize;
        let tuples = rows
            .iter()
            .map(|row| {
                debug_assert_eq!(row.len(), KIRO_KEY_COLUMNS);
                let tuple = (0..KIRO_KEY_COLUMNS)
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
        let sql = format!("insert into kiro_row_order values {tuples}");
        self.scratch
            .execute(&sql, params_from_iter(rows.iter().flatten()))
            .map_err(|source| {
                private_scratch_error("writing the private Kiro ordering index", source)
            })?;
        let rows = u64::try_from(rows.len()).map_err(|_| KiroSourceBackedErrorV0::CountOverflow)?;
        self.stats.key_batches = checked_add(self.stats.key_batches, 1)?;
        self.stats.data_statements = checked_add(self.stats.data_statements, 1)?;
        self.stats.max_key_batch_rows = self.stats.max_key_batch_rows.max(rows);
        Ok(())
    }

    fn stream_ordered_payloads(
        &mut self,
        phase: KiroPhase,
        visit: &mut dyn FnMut(KiroConversationRow) -> KiroSourceBackedResultV0<()>,
    ) -> KiroSourceBackedResultV0<()> {
        self.stats.data_statements = checked_add(self.stats.data_statements, 1)?;
        let mut ordered = self.scratch.prepare(KIRO_ORDER_SCAN).map_err(|source| {
            private_scratch_error("reading the private Kiro ordering index", source)
        })?;
        let mut rows = ordered.query([]).map_err(|source| {
            private_scratch_error("opening the private Kiro ordered row stream", source)
        })?;
        let mut pending = Vec::with_capacity(KIRO_HYDRATION_BATCH_ROWS);
        let mut pending_bytes = 0_u64;
        while let Some(row) = rows.next().map_err(|source| {
            private_scratch_error("streaming the private Kiro ordering index", source)
        })? {
            let source_rowid = row.get::<_, i64>(0).map_err(|source| {
                private_scratch_error("decoding the private Kiro ordering index", source)
            })?;
            let payload_bytes = row.get::<_, i64>(1).map_err(|source| {
                private_scratch_error("decoding the private Kiro payload bound", source)
            })?;
            let payload_bytes = u64::try_from(payload_bytes).map_err(|_| {
                CaptureError::SystemInvariant("Kiro ordering payload bound became negative")
            })?;
            if !pending.is_empty()
                && (pending.len() == KIRO_HYDRATION_BATCH_ROWS
                    || pending_bytes.saturating_add(payload_bytes) > KIRO_HYDRATION_BATCH_BYTES)
            {
                self.hydrate_batch(phase, &pending, visit)?;
                self.record_hydration_batch(pending.len(), pending_bytes)?;
                pending.clear();
                pending_bytes = 0;
            }
            pending.push(source_rowid);
            pending_bytes = pending_bytes.saturating_add(payload_bytes);
        }
        if !pending.is_empty() {
            self.hydrate_batch(phase, &pending, visit)?;
            self.record_hydration_batch(pending.len(), pending_bytes)?;
        }
        drop(rows);
        if ordered.get_status(StatementStatus::Sort) != 0 {
            return Err(CaptureError::SystemInvariant(
                "Kiro private ordering index unexpectedly used SQLite temporary sorting",
            )
            .into());
        }
        Ok(())
    }

    fn hydrate_batch(
        &self,
        phase: KiroPhase,
        requests: &[i64],
        visit: &mut dyn FnMut(KiroConversationRow) -> KiroSourceBackedResultV0<()>,
    ) -> KiroSourceBackedResultV0<()> {
        let sql = hydration_sql(phase, requests.len());
        let mut statement = self.source.prepare(&sql)?;
        let mut rows = statement.query(params_from_iter(requests.iter()))?;
        let mut decoded = HashMap::with_capacity(requests.len());
        while let Some(row) = rows.next()? {
            let row = decode_row(row, phase)?;
            if decoded.insert(row.rowid, row).is_some() {
                return Err(CaptureError::SystemInvariant(
                    "Kiro batched payload hydration duplicated a source row",
                )
                .into());
            }
        }
        drop(rows);
        if statement.get_status(StatementStatus::Sort) != 0 {
            return Err(CaptureError::SystemInvariant(
                "Kiro batched payload hydration unexpectedly used SQLite temporary sorting",
            )
            .into());
        }
        for source_rowid in requests {
            let row = decoded.remove(source_rowid).ok_or_else(|| {
                CaptureError::SystemInvariant(
                    "Kiro source row disappeared from its pinned snapshot",
                )
            })?;
            visit(row)?;
        }
        if !decoded.is_empty() {
            return Err(CaptureError::SystemInvariant(
                "Kiro batched payload hydration returned an unrequested source row",
            )
            .into());
        }
        Ok(())
    }

    fn record_hydration_batch(&mut self, rows: usize, bytes: u64) -> KiroSourceBackedResultV0<()> {
        let rows = u64::try_from(rows).map_err(|_| KiroSourceBackedErrorV0::CountOverflow)?;
        self.stats.rows = checked_add(self.stats.rows, rows)?;
        self.stats.hydration_batches = checked_add(self.stats.hydration_batches, 1)?;
        self.stats.data_statements = checked_add(self.stats.data_statements, 1)?;
        self.stats.max_hydration_batch_rows = self.stats.max_hydration_batch_rows.max(rows);
        self.stats.max_hydration_batch_bytes = self.stats.max_hydration_batch_bytes.max(bytes);
        Ok(())
    }
}

fn sort_key_sql(phase: KiroPhase) -> String {
    let payload_bytes = match phase {
        KiroPhase::V2 => {
            "coalesce(length(cast(key as blob)), 0) + \
             coalesce(length(cast(conversation_id as blob)), 0) + \
             coalesce(length(cast(value as blob)), 0) + 16"
        }
        KiroPhase::Legacy => {
            "coalesce(length(cast(key as blob)), 0) + \
             coalesce(length(cast(value as blob)), 0)"
        }
    };
    format!(
        "select rowid, typeof(key), key, {payload_bytes} from {}",
        phase.table()
    )
}

fn hydration_sql(phase: KiroPhase, rows: usize) -> String {
    let parameters = (1..=rows)
        .map(|parameter| format!("?{parameter}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "select {} from {} where rowid in ({parameters})",
        selected_columns(phase),
        phase.table()
    )
}

fn selected_columns(phase: KiroPhase) -> &'static str {
    match phase {
        KiroPhase::V2 => "rowid, key, conversation_id, value, created_at, updated_at",
        KiroPhase::Legacy => "rowid, key, value",
    }
}

fn decode_row(row: &Row<'_>, phase: KiroPhase) -> KiroSourceBackedResultV0<KiroConversationRow> {
    let rowid = row.get::<_, i64>(0)?;
    let decoded = match phase {
        KiroPhase::V2 => KiroConversationRow {
            table: phase.table(),
            rowid,
            key: required_text(row, 1, phase, rowid, "key")?,
            conversation_id: Some(required_text(row, 2, phase, rowid, "conversation_id")?),
            value: required_text(row, 3, phase, rowid, "value")?,
            created_at: optional_integer(row, 4, phase, rowid, "created_at")?,
            updated_at: optional_integer(row, 5, phase, rowid, "updated_at")?,
        },
        KiroPhase::Legacy => KiroConversationRow {
            table: phase.table(),
            rowid,
            key: required_text(row, 1, phase, rowid, "key")?,
            conversation_id: None,
            value: required_text(row, 2, phase, rowid, "value")?,
            created_at: None,
            updated_at: None,
        },
    };
    let retained_bytes = decoded
        .key
        .len()
        .checked_add(decoded.value.len())
        .and_then(|bytes| match decoded.conversation_id.as_ref() {
            Some(value) => bytes.checked_add(value.len()),
            None => Some(bytes),
        })
        .ok_or(KiroSourceBackedErrorV0::CountOverflow)?;
    if retained_bytes > MAX_PROVIDER_SQLITE_VALUE_BYTES {
        return Err(KiroSourceBackedErrorV0::UncertifiableRow {
            relation: phase.table(),
            rowid,
            reason: "row exceeds the provider SQLite value bound",
        });
    }
    Ok(decoded)
}

fn required_text(
    row: &Row<'_>,
    index: usize,
    phase: KiroPhase,
    rowid: i64,
    field: &'static str,
) -> KiroSourceBackedResultV0<String> {
    match row.get_ref(index)? {
        ValueRef::Text(value) => std::str::from_utf8(value).map(str::to_owned).map_err(|_| {
            KiroSourceBackedErrorV0::UncertifiableRow {
                relation: phase.table(),
                rowid,
                reason: "text column contains invalid UTF-8",
            }
        }),
        _ => Err(KiroSourceBackedErrorV0::UncertifiableRow {
            relation: phase.table(),
            rowid,
            reason: match field {
                "key" => "Kiro conversation key has an unsupported SQLite storage class",
                "conversation_id" => {
                    "Kiro conversations_v2.conversation_id has an unsupported SQLite storage class"
                }
                _ => "Kiro conversation value has an unsupported SQLite storage class",
            },
        }),
    }
}

fn optional_integer(
    row: &Row<'_>,
    index: usize,
    phase: KiroPhase,
    rowid: i64,
    field: &'static str,
) -> KiroSourceBackedResultV0<Option<i64>> {
    match row.get_ref(index)? {
        ValueRef::Null => Ok(None),
        ValueRef::Integer(value) => Ok(Some(value)),
        _ => Err(KiroSourceBackedErrorV0::UncertifiableRow {
            relation: phase.table(),
            rowid,
            reason: match field {
                "created_at" => {
                    "Kiro conversations_v2.created_at has an unsupported SQLite storage class"
                }
                _ => "Kiro conversations_v2.updated_at has an unsupported SQLite storage class",
            },
        }),
    }
}

fn measure_scratch_database(
    scratch: &Connection,
    scratch_path: &Path,
) -> KiroSourceBackedResultV0<u64> {
    let page_count: i64 = scratch
        .pragma_query_value(None, "page_count", |row| row.get(0))
        .map_err(|source| {
            private_scratch_error("measuring the private Kiro ordering index", source)
        })?;
    let page_size: i64 = scratch
        .pragma_query_value(None, "page_size", |row| row.get(0))
        .map_err(|source| {
            private_scratch_error("measuring the private Kiro ordering pages", source)
        })?;
    let logical_bytes = u64::try_from(page_count)
        .ok()
        .and_then(|pages| {
            u64::try_from(page_size)
                .ok()
                .and_then(|size| pages.checked_mul(size))
        })
        .ok_or(KiroSourceBackedErrorV0::CountOverflow)?;
    let physical_bytes = std::fs::metadata(scratch_path)
        .map_err(|source| SqliteSourceAccessError::Io {
            operation: "measuring the private Kiro ordering database",
            path: scratch_path.to_path_buf(),
            source,
        })?
        .len();
    if physical_bytes > logical_bytes {
        return Err(CaptureError::SystemInvariant(
            "Kiro private ordering database exceeded its SQLite page allocation",
        )
        .into());
    }
    Ok(physical_bytes)
}

fn query_plan_uses_temp_sort(connection: &Connection, sql: &str) -> rusqlite::Result<bool> {
    let mut statement = connection.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
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
    let mut statement = connection.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
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
) -> KiroSourceBackedErrorV0 {
    SqliteSourceAccessError::Sqlite { operation, source }.into()
}
