use std::{fs, io::Read, path::PathBuf, time::Duration as StdDuration};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Number, Value};

use ctx_history_core::database_path;
use ctx_history_store::{
    RawSqlOptions, RawSqlResult, RawSqlValue, RAW_SQL_MAX_SQL_BYTES_CAP, RAW_SQL_MAX_TIMEOUT,
};

use crate::output::{compact_json, print_share_safe_value, SqlFormat};
use crate::store_util::open_existing_store_read_only;
use crate::SqlArgs;

pub(crate) fn parse_sql_timeout(value: &str) -> std::result::Result<StdDuration, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("timeout must not be empty".to_owned());
    }
    let (number, multiplier_ms) = if let Some(number) = trimmed.strip_suffix("ms") {
        (number, 1.0)
    } else if let Some(number) = trimmed.strip_suffix('s') {
        (number, 1_000.0)
    } else if let Some(number) = trimmed.strip_suffix('m') {
        (number, 60_000.0)
    } else {
        (trimmed, 1_000.0)
    };
    let amount = number
        .parse::<f64>()
        .map_err(|err| format!("invalid timeout: {err}"))?;
    if !amount.is_finite() || amount <= 0.0 {
        return Err("timeout must be greater than zero".to_owned());
    }
    let millis = (amount * multiplier_ms).round();
    let max_millis = RAW_SQL_MAX_TIMEOUT.as_millis() as f64;
    if millis < 1.0 || millis > max_millis {
        return Err(format!(
            "timeout must be between 1ms and {}ms",
            RAW_SQL_MAX_TIMEOUT.as_millis()
        ));
    }
    Ok(StdDuration::from_millis(millis as u64))
}
pub(crate) fn run_sql(args: SqlArgs, data_root: PathBuf) -> Result<()> {
    let sql = read_sql_input(&args)?;
    let db_path = database_path(data_root);
    let store = open_existing_store_read_only(&db_path, "ctx sql")?;
    let result = store.raw_sql_query(
        &sql,
        RawSqlOptions {
            max_rows: args.max_rows,
            max_columns: args.max_columns,
            max_value_bytes: args.max_value_bytes,
            max_sql_bytes: args.max_sql_bytes,
            timeout: args.timeout,
        },
    )?;

    match args.output_format() {
        SqlFormat::Table => print_sql_table(&result),
        SqlFormat::Json => print_share_safe_value(raw_sql_result_json(&result)),
        SqlFormat::Csv => print_sql_csv(&result, args.no_header),
        SqlFormat::Raw => print_sql_raw(&result),
    }
}

pub(crate) fn read_sql_input(args: &SqlArgs) -> Result<String> {
    let max_sql_bytes = args.max_sql_bytes.min(RAW_SQL_MAX_SQL_BYTES_CAP);
    match (&args.sql, &args.file) {
        (Some(sql), None) if sql == "-" => {
            read_sql_limited(std::io::stdin().lock(), max_sql_bytes, "stdin")
        }
        (Some(sql), None) => Ok(sql.clone()),
        (None, Some(path)) => {
            let file = fs::File::open(path)
                .with_context(|| format!("read SQL from {}", path.display()))?;
            read_sql_limited(file, max_sql_bytes, &path.display().to_string())
        }
        (None, None) => Err(anyhow!(
            "SQL is required; pass a statement, --file <path>, or '-' for stdin"
        )),
        (Some(_), Some(_)) => unreachable!("clap rejects --file with inline SQL"),
    }
}

pub(crate) fn read_sql_limited(
    mut reader: impl Read,
    max_sql_bytes: usize,
    label: &str,
) -> Result<String> {
    let mut input = String::new();
    reader
        .by_ref()
        .take((max_sql_bytes as u64).saturating_add(1))
        .read_to_string(&mut input)
        .with_context(|| format!("read SQL from {label}"))?;
    if input.len() > max_sql_bytes {
        return Err(anyhow!(
            "SQL input from {label} exceeds max_sql_bytes ({max_sql_bytes})"
        ));
    }
    Ok(input)
}

pub(crate) fn print_sql_table(result: &RawSqlResult) -> Result<()> {
    let rows = result
        .rows
        .iter()
        .map(|row| row.iter().map(sql_table_cell).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    let mut widths = result
        .columns
        .iter()
        .map(|column| column.name.chars().count())
        .collect::<Vec<_>>();
    for row in &rows {
        for (index, cell) in row.iter().enumerate() {
            widths[index] = widths[index].max(cell.chars().count());
        }
    }

    let headers = result
        .columns
        .iter()
        .enumerate()
        .map(|(index, column)| pad_table_cell(&column.name, widths[index]))
        .collect::<Vec<_>>();
    println!("{}", headers.join(" | "));
    let separators = widths
        .iter()
        .map(|width| "-".repeat(*width))
        .collect::<Vec<_>>();
    println!("{}", separators.join(" | "));
    for row in &rows {
        let cells = row
            .iter()
            .enumerate()
            .map(|(index, cell)| pad_table_cell(cell, widths[index]))
            .collect::<Vec<_>>();
        println!("{}", cells.join(" | "));
    }
    if result.rows.is_empty() {
        println!("(0 rows)");
    }
    print_sql_truncation_notice(result);
    Ok(())
}

pub(crate) fn print_sql_csv(result: &RawSqlResult, no_header: bool) -> Result<()> {
    if !no_header {
        println!(
            "{}",
            result
                .columns
                .iter()
                .map(|column| csv_escape(&column.name))
                .collect::<Vec<_>>()
                .join(",")
        );
    }
    for row in &result.rows {
        println!(
            "{}",
            row.iter()
                .map(sql_csv_cell)
                .map(|cell| csv_escape(&cell))
                .collect::<Vec<_>>()
                .join(",")
        );
    }
    print_sql_truncation_notice(result);
    Ok(())
}

pub(crate) fn print_sql_raw(result: &RawSqlResult) -> Result<()> {
    if result.columns.len() != 1 {
        return Err(anyhow!(
            "--format raw requires exactly one selected column; got {}",
            result.columns.len()
        ));
    }
    for row in &result.rows {
        println!("{}", sql_raw_cell(&row[0]));
    }
    print_sql_truncation_notice(result);
    Ok(())
}

pub(crate) fn print_sql_truncation_notice(result: &RawSqlResult) {
    if result.truncated.rows {
        eprintln!(
            "warning: rows truncated at {}; rerun with --max-rows for more",
            result.limits.max_rows
        );
    }
    if result.truncated.values {
        eprintln!(
            "warning: values truncated at {} bytes; rerun with --max-value-bytes for more",
            result.limits.max_value_bytes
        );
    }
}

pub(crate) fn raw_sql_result_json(result: &RawSqlResult) -> Value {
    compact_json(json!({
        "schema_version": 1,
        "item_type": "sql_result",
        "read_only": true,
        "columns": result.columns.iter().map(|column| column.name.clone()).collect::<Vec<_>>(),
        "rows": result
            .rows
            .iter()
            .map(|row| row.iter().map(raw_sql_value_json).collect::<Vec<_>>())
            .collect::<Vec<_>>(),
        "returned_rows": result.returned_rows,
        "truncated": {
            "rows": result.truncated.rows,
            "values": result.truncated.values,
        },
        "limits": {
            "max_rows": result.limits.max_rows,
            "max_columns": result.limits.max_columns,
            "max_value_bytes": result.limits.max_value_bytes,
            "max_sql_bytes": result.limits.max_sql_bytes,
            "timeout_ms": result.limits.timeout_ms,
        },
        "elapsed_ms": result.elapsed.as_millis(),
    }))
}

pub(crate) fn raw_sql_value_json(value: &RawSqlValue) -> Value {
    match value {
        RawSqlValue::Null => Value::Null,
        RawSqlValue::Integer(value) => json!(value),
        RawSqlValue::Real(value) => Number::from_f64(*value)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        RawSqlValue::Text {
            value,
            bytes,
            truncated,
        } if *truncated => json!({
            "type": "text",
            "value": value,
            "bytes": bytes,
            "truncated": true,
        }),
        RawSqlValue::Text { value, .. } => Value::String(value.clone()),
        RawSqlValue::Blob {
            bytes,
            preview_hex,
            truncated,
        } => json!({
            "type": "blob",
            "bytes": bytes,
            "preview_hex": preview_hex,
            "truncated": truncated,
        }),
    }
}

pub(crate) fn sql_table_cell(value: &RawSqlValue) -> String {
    truncate_table_cell(&sql_display_cell(value), 96)
}

pub(crate) fn sql_csv_cell(value: &RawSqlValue) -> String {
    sql_display_cell(value)
}

pub(crate) fn sql_raw_cell(value: &RawSqlValue) -> String {
    match value {
        RawSqlValue::Null => String::new(),
        RawSqlValue::Integer(value) => value.to_string(),
        RawSqlValue::Real(value) => value.to_string(),
        RawSqlValue::Text { value, .. } => value.clone(),
        RawSqlValue::Blob { preview_hex, .. } => preview_hex.clone(),
    }
}

pub(crate) fn sql_display_cell(value: &RawSqlValue) -> String {
    match value {
        RawSqlValue::Null => "NULL".to_owned(),
        RawSqlValue::Integer(value) => value.to_string(),
        RawSqlValue::Real(value) => value.to_string(),
        RawSqlValue::Text {
            value, truncated, ..
        } => {
            let mut value = value.replace('\n', "\\n").replace('\r', "\\r");
            if *truncated {
                value.push_str("...");
            }
            value
        }
        RawSqlValue::Blob {
            bytes,
            preview_hex,
            truncated,
        } => {
            if *truncated {
                format!("[blob {bytes} bytes {preview_hex}...]")
            } else {
                format!("[blob {bytes} bytes {preview_hex}]")
            }
        }
    }
}

pub(crate) fn truncate_table_cell(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    let keep = max_chars.saturating_sub(3);
    let mut truncated = value.chars().take(keep).collect::<String>();
    truncated.push_str("...");
    truncated
}

pub(crate) fn pad_table_cell(value: &str, width: usize) -> String {
    let len = value.chars().count();
    if len >= width {
        value.to_owned()
    } else {
        format!("{value}{}", " ".repeat(width - len))
    }
}

pub(crate) fn csv_escape(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}
