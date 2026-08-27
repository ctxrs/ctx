use chrono::NaiveDate;
use rusqlite::Connection;

use super::super::{store, UsageStoreError};
use super::{UsageDefinition, UsageSummary};

struct StoredRow {
    day: String,
    definition_version: i64,
    ctx_version: String,
    surface: String,
    operation: String,
    outcome: String,
    value_class: String,
    duration_bucket: String,
    context_coverage: String,
    calls: i64,
    result_count: i64,
    delivered_output_bytes: i64,
    delivered_context_bytes: i64,
    matched_normalized_session_bytes: i64,
}

pub(in crate::local_usage) fn validate_rows_for_schema(
    conn: &Connection,
    schema_version: i64,
) -> Result<(), UsageStoreError> {
    match schema_version {
        store::LEGACY_SCHEMA_VERSION => validate_old_rows(conn, true),
        store::PREVIOUS_SCHEMA_VERSION | store::RELEASED_SCHEMA_VERSION => {
            validate_old_rows(conn, false)
        }
        store::PRIOR_SCHEMA_VERSION => validate_current_rows(conn, false),
        store::SCHEMA_VERSION => validate_rows(conn),
        version => Err(UsageStoreError::SchemaVersion(version)),
    }
}

fn validate_old_rows(conn: &Connection, first_version: bool) -> Result<(), UsageStoreError> {
    let sql = if first_version {
        r#"
        SELECT day_utc, 1, ctx_version, surface, operation, outcome,
            value_class, duration_bucket, 'not_applicable', calls, result_count,
            CASE WHEN surface = 'mcp' THEN response_bytes ELSE 0 END, 0, 0
        FROM daily_usage
        "#
    } else {
        r#"
        SELECT day_utc, definition_version, ctx_version, surface, operation,
            outcome, value_class, duration_bucket, context_coverage, calls,
            result_count, delivered_output_bytes, delivered_context_bytes,
            matched_normalized_session_bytes
        FROM daily_usage
        "#
    };
    let mut statement = conn.prepare(sql)?;
    let rows = statement.query_map([], stored_row)?;
    for row in rows {
        let row = row?;
        // Older public schemas included additional operation families. They are
        // intentionally omitted during migration; neutral rows remain strict.
        if valid_operation(
            row.definition_version,
            &row.surface,
            &row.operation,
            true,
            false,
        ) && !row_is_valid(&row, true, false)
        {
            return Err(UsageStoreError::Integrity);
        }
    }
    validate_maintenance(conn)
}

pub(in crate::local_usage) fn validate_rows(conn: &Connection) -> Result<(), UsageStoreError> {
    validate_current_rows(conn, true)
}

fn validate_current_rows(
    conn: &Connection,
    accepts_definition_three: bool,
) -> Result<(), UsageStoreError> {
    let mut statement = conn.prepare(
        r#"
        SELECT day_utc, definition_version, ctx_version, surface, operation,
            outcome, value_class, duration_bucket, context_coverage, calls,
            result_count, delivered_output_bytes, delivered_context_bytes,
            matched_normalized_session_bytes
        FROM daily_usage
        "#,
    )?;
    let rows = statement.query_map([], stored_row)?;
    for row in rows {
        if !row_is_valid(&row?, false, accepts_definition_three) {
            return Err(UsageStoreError::Integrity);
        }
    }
    validate_maintenance(conn)
}

fn stored_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredRow> {
    Ok(StoredRow {
        day: row.get(0)?,
        definition_version: row.get(1)?,
        ctx_version: row.get(2)?,
        surface: row.get(3)?,
        operation: row.get(4)?,
        outcome: row.get(5)?,
        value_class: row.get(6)?,
        duration_bucket: row.get(7)?,
        context_coverage: row.get(8)?,
        calls: row.get(9)?,
        result_count: row.get(10)?,
        delivered_output_bytes: row.get(11)?,
        delivered_context_bytes: row.get(12)?,
        matched_normalized_session_bytes: row.get(13)?,
    })
}

fn row_is_valid(
    row: &StoredRow,
    accepts_retired_sql: bool,
    accepts_definition_three: bool,
) -> bool {
    let failure_valid = if row.outcome == "failure" {
        row.value_class == "not_applicable" && row.result_count == 0
    } else {
        row.outcome == "success"
    };
    let value_valid = (row.value_class == "result_bearing" && row.result_count >= row.calls)
        || (matches!(row.value_class.as_str(), "empty" | "not_applicable")
            && row.result_count == 0);
    let classification = if row.outcome == "failure" {
        row.value_class == "not_applicable"
    } else if row.surface == "cli" {
        if matches!(row.definition_version, 2 | 3) && row.operation == "search" {
            matches!(row.value_class.as_str(), "result_bearing" | "empty")
        } else {
            row.value_class == "not_applicable"
        }
    } else if matches!(
        row.operation.as_str(),
        "sources" | "search" | "show_session" | "show_event"
    ) || (accepts_retired_sql && row.operation == "sql")
    {
        matches!(row.value_class.as_str(), "result_bearing" | "empty")
    } else {
        row.value_class == "not_applicable"
    };
    let complete = row.context_coverage == "complete"
        && matches!(row.definition_version, 2 | 3)
        && row.operation == "search"
        && row.outcome == "success"
        && row.value_class == "result_bearing"
        && row.delivered_context_bytes > 0
        && row.matched_normalized_session_bytes >= row.delivered_context_bytes;
    let unavailable = row.context_coverage == "unavailable"
        && matches!(row.definition_version, 2 | 3)
        && row.operation == "search"
        && row.outcome == "success"
        && row.value_class == "result_bearing"
        && row.delivered_context_bytes == 0
        && row.matched_normalized_session_bytes == 0;
    let not_applicable = row.context_coverage == "not_applicable"
        && row.delivered_context_bytes == 0
        && row.matched_normalized_session_bytes == 0;
    let output_valid = match row.definition_version {
        1 => {
            (row.surface == "cli" && row.delivered_output_bytes == 0)
                || (row.surface == "mcp" && row.delivered_output_bytes > 0)
        }
        2 => {
            row.delivered_output_bytes > 0
                || (row.surface == "cli"
                    && row.outcome == "failure"
                    && row.delivered_output_bytes == 0)
        }
        3 if accepts_definition_three => {
            if row.operation == "blame" {
                (row.surface == "cli" && row.delivered_output_bytes == 0)
                    || (row.surface == "mcp" && row.delivered_output_bytes > 0)
            } else {
                row.delivered_output_bytes > 0
                    || (row.surface == "cli"
                        && row.outcome == "failure"
                        && row.delivered_output_bytes == 0)
            }
        }
        _ => false,
    };

    NaiveDate::parse_from_str(&row.day, "%Y-%m-%d").is_ok()
        && (matches!(row.definition_version, 1 | 2)
            || (accepts_definition_three && row.definition_version == 3))
        && valid_ctx_version(&row.ctx_version)
        && valid_operation(
            row.definition_version,
            &row.surface,
            &row.operation,
            accepts_retired_sql,
            accepts_definition_three,
        )
        && matches!(
            row.duration_bucket.as_str(),
            "under_10_ms"
                | "10_to_49_ms"
                | "50_to_249_ms"
                | "250_to_999_ms"
                | "1_to_4_s"
                | "5_to_29_s"
                | "30_s_or_more"
        )
        && row.calls > 0
        && row.result_count >= 0
        && row.delivered_output_bytes >= 0
        && row.delivered_context_bytes >= 0
        && row.matched_normalized_session_bytes >= 0
        && failure_valid
        && value_valid
        && classification
        && output_valid
        && (complete || unavailable || not_applicable)
}

fn valid_ctx_version(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'+' | b'-'))
}

fn valid_operation(
    definition_version: i64,
    surface: &str,
    operation: &str,
    accepts_retired_sql: bool,
    accepts_definition_three: bool,
) -> bool {
    if accepts_definition_three && definition_version == 3 {
        return match surface {
            "cli" => matches!(
                operation,
                "setup"
                    | "index"
                    | "sources"
                    | "import"
                    | "show_session"
                    | "show_event"
                    | "locate"
                    | "search"
                    | "blame"
                    | "docs"
                    | "integrations"
                    | "daemon_status"
                    | "daemon_enable"
                    | "daemon_disable"
                    | "upgrade"
                    | "doctor"
            ),
            "mcp" => matches!(
                operation,
                "status" | "sources" | "search" | "show_session" | "show_event" | "blame"
            ),
            _ => false,
        };
    }
    match surface {
        "cli" => {
            matches!(
                operation,
                "setup"
                    | "index"
                    | "sources"
                    | "import"
                    | "locate"
                    | "search"
                    | "docs"
                    | "integrations"
                    | "daemon_status"
                    | "daemon_enable"
                    | "daemon_disable"
                    | "upgrade"
                    | "doctor"
            ) || (accepts_retired_sql && operation == "sql")
                || (definition_version == 1 && operation == "show")
                || (definition_version == 2 && matches!(operation, "show_session" | "show_event"))
        }
        "mcp" => {
            matches!(
                operation,
                "status" | "sources" | "search" | "show_session" | "show_event"
            ) || (accepts_retired_sql && operation == "sql")
        }
        _ => false,
    }
}

fn validate_maintenance(conn: &Connection) -> Result<(), UsageStoreError> {
    let mut statement = conn.prepare("SELECT singleton, last_retention_day FROM maintenance")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut count = 0_u8;
    for row in rows {
        let (singleton, day) = row?;
        count = count.checked_add(1).ok_or(UsageStoreError::Integrity)?;
        if singleton != 1 || NaiveDate::parse_from_str(&day, "%Y-%m-%d").is_err() {
            return Err(UsageStoreError::Integrity);
        }
    }
    if count > 1 {
        Err(UsageStoreError::Integrity)
    } else {
        Ok(())
    }
}

pub(super) fn checked_count(value: i64) -> Result<u64, UsageStoreError> {
    u64::try_from(value).map_err(|_| UsageStoreError::Integrity)
}

pub(super) fn checked_sum(values: impl IntoIterator<Item = u64>) -> Result<u64, UsageStoreError> {
    values.into_iter().try_fold(0_u64, |total, value| {
        total.checked_add(value).ok_or(UsageStoreError::Integrity)
    })
}

fn reconcile_summary(summary: &UsageSummary) -> Result<(), UsageStoreError> {
    if checked_sum([summary.successful_calls, summary.failed_calls])? != summary.calls
        || checked_sum([
            summary.result_bearing_calls,
            summary.empty_calls,
            summary.not_applicable_calls,
        ])? != summary.calls
    {
        return Err(UsageStoreError::Integrity);
    }
    Ok(())
}

pub(super) fn reconcile_definition(
    definition: &UsageDefinition,
    detailed: bool,
) -> Result<(), UsageStoreError> {
    reconcile_summary(&definition.summary)?;
    if !detailed {
        return Ok(());
    }
    for operation in &definition.by_operation {
        if checked_sum([operation.successful_calls, operation.failed_calls])? != operation.calls
            || checked_sum([
                operation.result_bearing_calls,
                operation.empty_calls,
                operation.not_applicable_calls,
            ])? != operation.calls
        {
            return Err(UsageStoreError::Integrity);
        }
    }
    if checked_sum(
        definition
            .by_operation
            .iter()
            .map(|operation| operation.calls),
    )? != definition.summary.calls
        || checked_sum(
            definition
                .duration_buckets
                .iter()
                .map(|duration| duration.calls),
        )? != definition.summary.calls
    {
        return Err(UsageStoreError::Integrity);
    }
    Ok(())
}
