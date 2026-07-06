#[allow(unused_imports)]
use super::*;

pub const RAW_SQL_DEFAULT_MAX_ROWS: usize = 100;

pub const RAW_SQL_MAX_ROWS_CAP: usize = 10_000;

pub const RAW_SQL_DEFAULT_MAX_COLUMNS: usize = 64;

pub const RAW_SQL_MAX_COLUMNS_CAP: usize = 256;

pub const RAW_SQL_DEFAULT_MAX_VALUE_BYTES: usize = 512;

pub const RAW_SQL_MAX_VALUE_BYTES_CAP: usize = 1_048_576;

pub const RAW_SQL_MAX_RESULT_PREVIEW_BYTES: usize = 64 * 1024 * 1024;

pub const RAW_SQL_MAX_RESULT_CELLS: usize = 262_144;

pub(crate) const RAW_SQL_MIN_SQLITE_LENGTH_LIMIT_BYTES: usize = 64 * 1024;

pub(crate) const RAW_SQL_VALUE_LENGTH_MARGIN_BYTES: usize = 1024;

pub const RAW_SQL_DEFAULT_MAX_SQL_BYTES: usize = 64 * 1024;

pub const RAW_SQL_MAX_SQL_BYTES_CAP: usize = 1_048_576;

pub const RAW_SQL_DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

pub const RAW_SQL_MAX_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawSqlOptions {
    pub max_rows: usize,
    pub max_columns: usize,
    pub max_value_bytes: usize,
    pub max_sql_bytes: usize,
    pub timeout: Duration,
}

impl Default for RawSqlOptions {
    fn default() -> Self {
        Self {
            max_rows: RAW_SQL_DEFAULT_MAX_ROWS,
            max_columns: RAW_SQL_DEFAULT_MAX_COLUMNS,
            max_value_bytes: RAW_SQL_DEFAULT_MAX_VALUE_BYTES,
            max_sql_bytes: RAW_SQL_DEFAULT_MAX_SQL_BYTES,
            timeout: RAW_SQL_DEFAULT_TIMEOUT,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawSqlColumn {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RawSqlValue {
    Null,
    Integer(i64),
    Real(f64),
    Text {
        value: String,
        bytes: usize,
        truncated: bool,
    },
    Blob {
        bytes: usize,
        preview_hex: String,
        truncated: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawSqlTruncation {
    pub rows: bool,
    pub values: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawSqlLimits {
    pub max_rows: usize,
    pub max_columns: usize,
    pub max_value_bytes: usize,
    pub max_sql_bytes: usize,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RawSqlResult {
    pub columns: Vec<RawSqlColumn>,
    pub rows: Vec<Vec<RawSqlValue>>,
    pub returned_rows: usize,
    pub truncated: RawSqlTruncation,
    pub elapsed: Duration,
    pub limits: RawSqlLimits,
}

impl RawSqlValue {
    pub(crate) fn is_truncated(&self) -> bool {
        match self {
            Self::Text { truncated, .. } | Self::Blob { truncated, .. } => *truncated,
            Self::Null | Self::Integer(_) | Self::Real(_) => false,
        }
    }
}

pub(crate) fn validate_raw_sql_options(options: &RawSqlOptions) -> Result<()> {
    validate_raw_sql_usize("max_rows", options.max_rows, 1, RAW_SQL_MAX_ROWS_CAP)?;
    validate_raw_sql_usize(
        "max_columns",
        options.max_columns,
        1,
        RAW_SQL_MAX_COLUMNS_CAP,
    )?;
    validate_raw_sql_usize(
        "max_value_bytes",
        options.max_value_bytes,
        1,
        RAW_SQL_MAX_VALUE_BYTES_CAP,
    )?;
    validate_raw_sql_usize(
        "max_sql_bytes",
        options.max_sql_bytes,
        1,
        RAW_SQL_MAX_SQL_BYTES_CAP,
    )?;
    let timeout_ms = duration_ms(options.timeout);
    if timeout_ms == 0 || options.timeout > RAW_SQL_MAX_TIMEOUT {
        return Err(StoreError::RawSqlLimitOutOfRange {
            field: "timeout_ms",
            value: usize::try_from(timeout_ms).unwrap_or(usize::MAX),
            min: 1,
            max: usize::try_from(duration_ms(RAW_SQL_MAX_TIMEOUT)).unwrap_or(usize::MAX),
        });
    }
    Ok(())
}

pub(crate) fn validate_raw_sql_statement_bytes(sql: &str, options: &RawSqlOptions) -> Result<()> {
    validate_raw_sql_usize("sql_bytes", sql.len(), 1, options.max_sql_bytes)
}

pub(crate) fn validate_raw_sql_result_preview_budget(
    options: &RawSqlOptions,
    column_count: usize,
) -> Result<()> {
    let estimated_cells = options.max_rows.saturating_mul(column_count);
    let per_cell_bytes = options
        .max_value_bytes
        .saturating_mul(4)
        .saturating_add(64)
        .max(128);
    let estimated_bytes = options
        .max_rows
        .saturating_mul(column_count)
        .saturating_mul(per_cell_bytes);
    if estimated_cells > RAW_SQL_MAX_RESULT_CELLS
        || estimated_bytes > RAW_SQL_MAX_RESULT_PREVIEW_BYTES
    {
        return Err(StoreError::RawSqlResultBudgetTooLarge {
            estimated_bytes,
            max_result_bytes: RAW_SQL_MAX_RESULT_PREVIEW_BYTES,
        });
    }
    Ok(())
}

pub(crate) struct RawSqlLimitGuard<'a> {
    pub(crate) conn: &'a Connection,
    pub(crate) length: i32,
    pub(crate) sql_length: i32,
    pub(crate) column: i32,
}

impl<'a> RawSqlLimitGuard<'a> {
    pub(crate) fn apply(conn: &'a Connection, options: &RawSqlOptions) -> Result<Self> {
        let length_limit = raw_sql_length_limit(options)?;
        let sql_length_limit = i32::try_from(options.max_sql_bytes).map_err(|_| {
            StoreError::RawSqlLimitOutOfRange {
                field: "max_sql_bytes",
                value: options.max_sql_bytes,
                min: 1,
                max: RAW_SQL_MAX_SQL_BYTES_CAP,
            }
        })?;
        let column_limit =
            i32::try_from(options.max_columns).map_err(|_| StoreError::RawSqlLimitOutOfRange {
                field: "max_columns",
                value: options.max_columns,
                min: 1,
                max: RAW_SQL_MAX_COLUMNS_CAP,
            })?;
        let guard = Self {
            conn,
            length: conn.set_limit(Limit::SQLITE_LIMIT_LENGTH, length_limit),
            sql_length: conn.set_limit(Limit::SQLITE_LIMIT_SQL_LENGTH, sql_length_limit),
            column: conn.set_limit(Limit::SQLITE_LIMIT_COLUMN, column_limit),
        };
        Ok(guard)
    }
}

impl Drop for RawSqlLimitGuard<'_> {
    fn drop(&mut self) {
        self.conn.set_limit(Limit::SQLITE_LIMIT_LENGTH, self.length);
        self.conn
            .set_limit(Limit::SQLITE_LIMIT_SQL_LENGTH, self.sql_length);
        self.conn.set_limit(Limit::SQLITE_LIMIT_COLUMN, self.column);
    }
}

pub(crate) fn raw_sql_length_limit(options: &RawSqlOptions) -> Result<i32> {
    let bytes = options
        .max_value_bytes
        .saturating_add(RAW_SQL_VALUE_LENGTH_MARGIN_BYTES);
    let bytes = bytes.max(RAW_SQL_MIN_SQLITE_LENGTH_LIMIT_BYTES);
    i32::try_from(bytes).map_err(|_| StoreError::RawSqlLimitOutOfRange {
        field: "max_value_bytes",
        value: options.max_value_bytes,
        min: 1,
        max: RAW_SQL_MAX_VALUE_BYTES_CAP,
    })
}

pub(crate) fn validate_raw_sql_usize(
    field: &'static str,
    value: usize,
    min: usize,
    max: usize,
) -> Result<()> {
    if (min..=max).contains(&value) {
        Ok(())
    } else {
        Err(StoreError::RawSqlLimitOutOfRange {
            field,
            value,
            min,
            max,
        })
    }
}

pub(crate) fn reject_sql_tail(conn: &Connection, sql: &str) -> Result<()> {
    let c_sql = CString::new(sql).map_err(|_| StoreError::RawSqlInteriorNul)?;
    let mut stmt = ptr::null_mut();
    let mut tail: *const c_char = ptr::null();
    let rc =
        unsafe { ffi::sqlite3_prepare_v2(conn.handle(), c_sql.as_ptr(), -1, &mut stmt, &mut tail) };
    if !stmt.is_null() {
        unsafe {
            ffi::sqlite3_finalize(stmt);
        }
    }
    if rc != ffi::SQLITE_OK || tail.is_null() {
        return Ok(());
    }

    let start = c_sql.as_ptr() as usize;
    let tail_offset = (tail as usize).saturating_sub(start);
    let sql_bytes = c_sql.as_bytes();
    if tail_offset < sql_bytes.len() && sql_tail_has_statement(&sql[tail_offset..]) {
        return Err(StoreError::Sql(rusqlite::Error::MultipleStatement));
    }
    Ok(())
}

pub(crate) fn raw_sql_value(value: ValueRef<'_>, max_value_bytes: usize) -> RawSqlValue {
    match value {
        ValueRef::Null => RawSqlValue::Null,
        ValueRef::Integer(value) => RawSqlValue::Integer(value),
        ValueRef::Real(value) => RawSqlValue::Real(value),
        ValueRef::Text(bytes) => {
            let truncated = bytes.len() > max_value_bytes;
            let preview = if truncated {
                String::from_utf8_lossy(&bytes[..max_value_bytes]).into_owned()
            } else {
                String::from_utf8_lossy(bytes).into_owned()
            };
            RawSqlValue::Text {
                value: preview,
                bytes: bytes.len(),
                truncated,
            }
        }
        ValueRef::Blob(bytes) => {
            let truncated = bytes.len() > max_value_bytes;
            let preview_len = bytes.len().min(max_value_bytes);
            RawSqlValue::Blob {
                bytes: bytes.len(),
                preview_hex: hex_preview(&bytes[..preview_len]),
                truncated,
            }
        }
    }
}
