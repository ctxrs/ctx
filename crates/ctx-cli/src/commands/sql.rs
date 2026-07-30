use std::{
    fs,
    io::{Read, Write as _},
    path::PathBuf,
    time::Duration as StdDuration,
};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Number, Value};

use ctx_history_relational::{
    RawSqlOptions, RawSqlResult, RawSqlValue, RAW_SQL_MAX_SQL_BYTES_CAP, RAW_SQL_MAX_TIMEOUT,
};

use crate::analytics::{count_bucket, duration_bucket, SqlTelemetry};
use crate::local_usage::{CliUsage, ResultObservationAction};
use crate::output::{compact_json, print_json, SqlFormat};
use crate::source_sql::SqlCompatibility;
use crate::ui::{
    diagnostic, empty_state, outcome, section, table, Diagnostic, DiagnosticLevel, Document,
    EmptyState, Field, Outcome, OutcomeState, RenderContext, Table, Ui,
};
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
pub(crate) fn run_sql(
    args: SqlArgs,
    data_root: PathBuf,
    telemetry: &mut SqlTelemetry,
    local_usage: &mut CliUsage,
    ui: &mut Ui,
) -> Result<()> {
    let sql = read_sql_input(&args)?;
    let compatibility = SqlCompatibility::open_for_data_root(data_root)?;
    let result = compatibility.query(
        &sql,
        RawSqlOptions {
            max_rows: args.max_rows,
            max_columns: args.max_columns,
            max_value_bytes: args.max_value_bytes,
            max_sql_bytes: args.max_sql_bytes,
            timeout: args.timeout,
        },
    )?;
    telemetry.returned_rows = Some(count_bucket(result.returned_rows as u64));
    telemetry.returned_columns = Some(count_bucket(result.columns.len() as u64));
    telemetry.rows_truncated = Some(result.truncated.rows);
    telemetry.values_truncated = Some(result.truncated.values);
    telemetry.query_duration = Some(duration_bucket(result.elapsed));

    let rows = result
        .rows
        .iter()
        .map(|row| row.iter().map(raw_sql_value_json).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    let content_bytes = serde_json::to_vec(&rows)?.len();
    let output_bytes = match args.output_format() {
        SqlFormat::Table => print_sql_table(&result, ui),
        SqlFormat::Json => {
            let value = raw_sql_result_json(&result);
            let output_bytes = serde_json::to_string_pretty(&value)?
                .len()
                .saturating_add(1);
            print_json(value)?;
            Ok(output_bytes)
        }
        SqlFormat::Csv => print_sql_csv(&result, args.no_header),
        SqlFormat::Raw => print_sql_raw(&result),
    }?;
    local_usage.set_result_observation(
        ResultObservationAction::Sql,
        result.returned_rows,
        0,
        content_bytes,
    );
    local_usage.set_measured_output_bytes(output_bytes);
    Ok(())
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

pub(crate) fn print_sql_table(result: &RawSqlResult, ui: &mut Ui) -> Result<usize> {
    let document = render_sql_table(ui.stdout_context(), result);
    let output_bytes = document.render_plain().len();
    ui.write_stdout(&document)?;
    if let Some(warning) = render_sql_truncation(ui.stderr_context(), result) {
        ui.write_stderr(&warning)?;
    }
    Ok(output_bytes)
}

fn render_sql_table(context: &RenderContext, result: &RawSqlResult) -> Document {
    if result.rows.is_empty() {
        let mut document = empty_state(
            context,
            EmptyState {
                title: "No rows returned",
                detail: "The read-only query completed successfully.",
                action: None,
            },
        );
        let mut columns = Table::new(["Position", "Column"]);
        for (index, column) in result.columns.iter().enumerate() {
            columns.push_row([(index + 1).to_string(), column.name.clone()]);
        }
        document.push_blank();
        document.append(section("Selected columns", table(context, &columns)));
        return document;
    }

    let title = match result.returned_rows {
        1 => "1 row returned".to_owned(),
        count => format!("{count} rows returned"),
    };
    let mut document = outcome(
        context,
        Outcome {
            state: OutcomeState::Success,
            title: &title,
            detail: None,
        },
    );
    let mut results = Table::new(result.columns.iter().map(|column| column.name.clone()));
    for row in &result.rows {
        results.push_row(row.iter().map(sql_table_cell));
    }
    document.push_blank();
    document.append(section("Results", table(context, &results)));
    document
}

fn render_sql_truncation(context: &RenderContext, result: &RawSqlResult) -> Option<Document> {
    if !result.truncated.rows && !result.truncated.values {
        return None;
    }
    let rows = result.limits.max_rows.to_string();
    let value_bytes = result.limits.max_value_bytes.to_string();
    let mut values = Vec::new();
    if result.truncated.rows {
        values.push(Field::new("Row limit", rows.as_str()));
    }
    if result.truncated.values {
        values.push(Field::new("Value-byte limit", value_bytes.as_str()));
    }
    Some(diagnostic(
        context,
        Diagnostic {
            level: DiagnosticLevel::Warning,
            summary: "SQL results were truncated",
            detail: Some("Increase the relevant query limit to return more data."),
            fields: &values,
            action: None,
        },
    ))
}

pub(crate) fn print_sql_csv(result: &RawSqlResult, no_header: bool) -> Result<usize> {
    let mut body = String::new();
    if !no_header {
        body.push_str(
            &result
                .columns
                .iter()
                .map(|column| csv_escape(&column.name))
                .collect::<Vec<_>>()
                .join(","),
        );
        body.push('\n');
    }
    for row in &result.rows {
        body.push_str(
            &row.iter()
                .map(sql_csv_cell)
                .map(|cell| csv_escape(&cell))
                .collect::<Vec<_>>()
                .join(","),
        );
        body.push('\n');
    }
    write_sql_stdout(body, result)
}

pub(crate) fn print_sql_raw(result: &RawSqlResult) -> Result<usize> {
    if result.columns.len() != 1 {
        return Err(anyhow!(
            "--format raw requires exactly one selected column; got {}",
            result.columns.len()
        ));
    }
    let mut body = String::new();
    for row in &result.rows {
        body.push_str(&sql_raw_cell(&row[0]));
        body.push('\n');
    }
    write_sql_stdout(body, result)
}

fn write_sql_stdout(body: String, result: &RawSqlResult) -> Result<usize> {
    std::io::stdout().lock().write_all(body.as_bytes())?;
    print_sql_truncation_notice(result);
    Ok(body.len())
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
        "payload_type": "sql_result",
        "read_only": true,
        "share_safe": false,
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

pub(crate) fn csv_escape(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

#[cfg(test)]
mod ui_tests {
    use std::{io::Write as _, time::Duration};

    use ctx_history_relational::{RawSqlColumn, RawSqlLimits, RawSqlTruncation, RawSqlValue};
    use unicode_width::UnicodeWidthStr as _;

    use super::*;
    use crate::ui::{ColorMode, StreamKind, TestContext};

    fn context(width: usize, color: ColorMode) -> RenderContext {
        RenderContext::for_test(TestContext::tty(StreamKind::Stdout, width).color(color))
    }

    fn result(rows: Vec<Vec<RawSqlValue>>) -> RawSqlResult {
        RawSqlResult {
            columns: vec![
                RawSqlColumn {
                    name: "provider".to_owned(),
                },
                RawSqlColumn {
                    name: "summary".to_owned(),
                },
            ],
            returned_rows: rows.len(),
            rows,
            truncated: RawSqlTruncation {
                rows: false,
                values: false,
            },
            elapsed: Duration::from_millis(2),
            limits: RawSqlLimits {
                max_rows: 100,
                max_columns: 32,
                max_value_bytes: 16_384,
                max_sql_bytes: 65_536,
                timeout_ms: 10_000,
            },
        }
    }

    fn text(value: &str) -> RawSqlValue {
        RawSqlValue::Text {
            value: value.to_owned(),
            bytes: value.len(),
            truncated: false,
        }
    }

    fn assert_fits(document: &Document, context: &RenderContext) {
        let width = context.content_width().unwrap_or(1);
        for line in document.render_plain().lines() {
            assert!(line.width() <= width, "{line:?} exceeded {width} columns");
        }
    }

    fn strip_ansi(rendered: &str) -> String {
        let mut stream = anstream::StripStream::new(Vec::new());
        stream.write_all(rendered.as_bytes()).unwrap();
        String::from_utf8(stream.into_inner()).unwrap()
    }

    #[test]
    fn sql_table_is_outcome_first_responsive_and_control_safe() {
        let result = result(vec![vec![
            text("codex"),
            text("a long user-controlled summary with \u{1b}[31m terminal controls"),
        ]]);
        for width in [32, 48, 80, 120] {
            let context = context(width, ColorMode::Never);
            let document = render_sql_table(&context, &result);
            let rendered = document.render_plain();
            assert!(rendered.starts_with("✓ 1 row returned\n"));
            assert!(rendered.contains("Results\n"));
            assert!(rendered.contains("\\x1b[31m"));
            assert!(!rendered.as_bytes().contains(&0x1b));
            assert_fits(&document, &context);
        }
    }

    #[test]
    fn sql_empty_result_is_explicit() {
        let context = context(48, ColorMode::Never);
        let rendered = render_sql_table(&context, &result(Vec::new())).render_plain();
        assert!(
            rendered.starts_with("No rows returned\nThe read-only query completed successfully.\n")
        );
        assert!(rendered.contains("\nSelected columns\n"));
        assert!(rendered.contains("Position"));
        assert!(rendered.contains("Column"));
        assert!(rendered.find("provider").unwrap() < rendered.find("summary").unwrap());
    }

    #[test]
    fn sql_truncation_is_structured_and_actionable() {
        let mut result = result(vec![vec![text("codex"), text("summary")]]);
        result.truncated.rows = true;
        result.truncated.values = true;
        let context = context(48, ColorMode::Never);
        let document = render_sql_truncation(&context, &result).unwrap();
        let rendered = document.render_plain();
        assert!(rendered.starts_with("! SQL results were truncated\n"));
        assert!(rendered.contains("Row limit"));
        assert!(rendered.contains("Value-byte limit"));
        assert_fits(&document, &context);
    }

    #[test]
    fn sql_plain_output_matches_ansi_stripped_output() {
        let result = result(vec![vec![text("codex"), text("summary")]]);
        let context = context(80, ColorMode::Always);
        let document = render_sql_table(&context, &result);
        assert_eq!(
            strip_ansi(&document.render(&context)),
            document.render_plain()
        );
    }

    #[test]
    fn csv_and_raw_cells_keep_their_machine_contracts() {
        let value = text("line one\nline two,\"quoted\"");
        assert_eq!(
            csv_escape(&sql_csv_cell(&value)),
            "\"line one\\nline two,\"\"quoted\"\"\""
        );
        assert_eq!(sql_raw_cell(&value), "line one\nline two,\"quoted\"");
    }
}
