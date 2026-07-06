#[allow(unused_imports)]
use super::*;

#[derive(Debug, Args)]
pub(crate) struct SqlArgs {
    #[arg(help = "Read-only SQL statement to run; pass '-' to read SQL from stdin")]
    pub(crate) sql: Option<String>,
    #[arg(long, conflicts_with = "sql", help = "Read SQL from a file")]
    pub(crate) file: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t = SqlFormat::Table)]
    pub(crate) format: SqlFormat,
    #[arg(long, help = "Alias for --format json")]
    pub(crate) json: bool,
    #[arg(long, default_value_t = RAW_SQL_DEFAULT_MAX_ROWS)]
    pub(crate) max_rows: usize,
    #[arg(long, default_value_t = RAW_SQL_DEFAULT_MAX_COLUMNS)]
    pub(crate) max_columns: usize,
    #[arg(long, default_value_t = RAW_SQL_DEFAULT_MAX_VALUE_BYTES)]
    pub(crate) max_value_bytes: usize,
    #[arg(long, default_value_t = RAW_SQL_DEFAULT_MAX_SQL_BYTES)]
    pub(crate) max_sql_bytes: usize,
    #[arg(long, default_value = "10s", value_parser = parse_sql_timeout)]
    pub(crate) timeout: StdDuration,
    #[arg(long, help = "Omit the header row for CSV output")]
    pub(crate) no_header: bool,
}

impl SqlArgs {
    pub(crate) fn output_format(&self) -> SqlFormat {
        if self.json {
            SqlFormat::Json
        } else {
            self.format
        }
    }

    pub(crate) fn json_output(&self) -> bool {
        self.output_format() == SqlFormat::Json
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum SqlFormat {
    Table,
    Json,
    Csv,
    Raw,
}

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
