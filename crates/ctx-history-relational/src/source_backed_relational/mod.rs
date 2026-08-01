//! Disposable relational metadata projection over one immutable Core generation.
//!
//! The materializer accepts source-grouped complete [`ctx_history_core::CoreRecord`]
//! values from one pinned Core reader. It stores identities, timestamps, source
//! health, and repository-scoped file/VCS observations. It never stores record
//! content, source locators, provider paths, or hydration state, and it never
//! opens provider inputs.
//!
//! Every successful receipt binds one Core generation, this SQLite schema, and
//! the materializer revision. An exact ready generation is a read-only no-op.
//! Replacement, deletion, and revision rebuilds run in one SQLite transaction;
//! failure leaves the prior coherent rows available and marks their frontier
//! behind the requested Core generation.

mod manifest;
mod materialization;
mod model;
mod publication;
mod raw_sql;
mod read;
mod schema;

pub use model::*;
pub use raw_sql::{
    RawSqlColumn, RawSqlLimits, RawSqlOptions, RawSqlResult, RawSqlTruncation, RawSqlValue,
    RAW_SQL_DEFAULT_MAX_COLUMNS, RAW_SQL_DEFAULT_MAX_ROWS, RAW_SQL_DEFAULT_MAX_SQL_BYTES,
    RAW_SQL_DEFAULT_MAX_VALUE_BYTES, RAW_SQL_DEFAULT_TIMEOUT, RAW_SQL_MAX_COLUMNS_CAP,
    RAW_SQL_MAX_RESULT_CELLS, RAW_SQL_MAX_RESULT_PREVIEW_BYTES, RAW_SQL_MAX_ROWS_CAP,
    RAW_SQL_MAX_SQL_BYTES_CAP, RAW_SQL_MAX_TIMEOUT, RAW_SQL_MAX_VALUE_BYTES_CAP,
};

use std::path::PathBuf;

use rusqlite::Connection;

pub struct SourceBackedRelationalProjection {
    path: PathBuf,
    conn: Connection,
    read_only: bool,
}

fn sqlite_i64(value: u64, field: &'static str) -> Result<i64> {
    i64::try_from(value).map_err(|_| RelationalProjectionError::CountOverflow(field))
}

fn sqlite_u64_ordered_text(value: u64) -> String {
    format!("{value:020}")
}

fn sqlite_u64(value: i64, field: &'static str) -> Result<u64> {
    u64::try_from(value).map_err(|_| RelationalProjectionError::CountOverflow(field))
}

fn sqlite_u32(value: i64, field: &'static str) -> Result<u32> {
    u32::try_from(value).map_err(|_| RelationalProjectionError::CountOverflow(field))
}

#[cfg(test)]
mod tests;
