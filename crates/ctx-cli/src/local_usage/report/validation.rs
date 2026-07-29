use chrono::NaiveDate;
use rusqlite::Connection;

use super::super::{store, UsageStoreError};
use super::{UsageDefinition, UsageSummary};

struct StoredRowV1 {
    day: String,
    definition_version: i64,
    ctx_version: String,
    surface: String,
    operation: String,
    outcome: String,
    value_class: String,
    duration_bucket: String,
    target_type: String,
    pro_outcome: String,
    calls: i64,
    result_count: i64,
    citation_count: i64,
    response_bytes: i64,
}

struct StoredRow {
    day: String,
    definition_version: i64,
    ctx_version: String,
    surface: String,
    operation: String,
    outcome: String,
    value_class: String,
    duration_bucket: String,
    target_type: String,
    pro_outcome: String,
    context_coverage: String,
    calls: i64,
    result_count: i64,
    citation_count: i64,
    delivered_output_bytes: i64,
    delivered_context_bytes: i64,
    matched_normalized_session_bytes: i64,
}

pub(in crate::local_usage) fn validate_rows_for_schema(
    conn: &Connection,
    schema_version: i64,
) -> Result<(), UsageStoreError> {
    match schema_version {
        1 => validate_rows_v1(conn),
        2 => validate_rows(conn),
        version => Err(UsageStoreError::SchemaVersion(version)),
    }
}

fn validate_rows_v1(conn: &Connection) -> Result<(), UsageStoreError> {
    let normalize_legacy_blame = store::v1_uses_legacy_blame_schema(conn)?;
    let mut statement = conn.prepare(
        r#"
        SELECT day_utc, definition_version, ctx_version, surface, operation,
               outcome, value_class, duration_bucket, target_type, pro_outcome,
               calls, result_count, citation_count, response_bytes
        FROM daily_usage
        "#,
    )?;
    let rows = statement.query_map([], |row| {
        Ok(StoredRowV1 {
            day: row.get(0)?,
            definition_version: row.get(1)?,
            ctx_version: row.get(2)?,
            surface: row.get(3)?,
            operation: row.get(4)?,
            outcome: row.get(5)?,
            value_class: row.get(6)?,
            duration_bucket: row.get(7)?,
            target_type: row.get(8)?,
            pro_outcome: row.get(9)?,
            calls: row.get(10)?,
            result_count: row.get(11)?,
            citation_count: row.get(12)?,
            response_bytes: row.get(13)?,
        })
    })?;
    for row in rows {
        let row = row?;
        if !common_row_is_valid(
            &row.day,
            row.definition_version,
            &row.ctx_version,
            &row.surface,
            &row.operation,
            &row.outcome,
            &row.value_class,
            &row.duration_bucket,
            &row.target_type,
            &row.pro_outcome,
            row.calls,
            row.result_count,
            row.citation_count,
            normalize_legacy_blame,
        ) || row.definition_version != 1
            || row.response_bytes < 0
            || (row.surface == "cli" && row.response_bytes != 0)
            || (row.surface == "mcp" && row.response_bytes <= 0)
        {
            return Err(UsageStoreError::Integrity);
        }
    }
    validate_maintenance(conn)
}

pub(in crate::local_usage) fn validate_rows(conn: &Connection) -> Result<(), UsageStoreError> {
    let mut statement = conn.prepare(
        r#"
        SELECT day_utc, definition_version, ctx_version, surface, operation,
               outcome, value_class, duration_bucket, target_type, pro_outcome,
               context_coverage, calls, result_count, citation_count,
               delivered_output_bytes, delivered_context_bytes,
               matched_normalized_session_bytes
        FROM daily_usage
        "#,
    )?;
    let rows = statement.query_map([], |row| {
        Ok(StoredRow {
            day: row.get(0)?,
            definition_version: row.get(1)?,
            ctx_version: row.get(2)?,
            surface: row.get(3)?,
            operation: row.get(4)?,
            outcome: row.get(5)?,
            value_class: row.get(6)?,
            duration_bucket: row.get(7)?,
            target_type: row.get(8)?,
            pro_outcome: row.get(9)?,
            context_coverage: row.get(10)?,
            calls: row.get(11)?,
            result_count: row.get(12)?,
            citation_count: row.get(13)?,
            delivered_output_bytes: row.get(14)?,
            delivered_context_bytes: row.get(15)?,
            matched_normalized_session_bytes: row.get(16)?,
        })
    })?;
    for row in rows {
        let row = row?;
        let complete = row.context_coverage == "complete"
            && row.definition_version == 2
            && row.operation == "search"
            && row.outcome == "success"
            && row.value_class == "result_bearing"
            && row.delivered_context_bytes > 0
            && row.matched_normalized_session_bytes > 0
            && row.matched_normalized_session_bytes >= row.delivered_context_bytes;
        let unavailable = row.context_coverage == "unavailable"
            && row.definition_version == 2
            && row.operation == "search"
            && row.outcome == "success"
            && row.value_class == "result_bearing"
            && row.delivered_context_bytes == 0
            && row.matched_normalized_session_bytes == 0;
        let not_applicable = row.context_coverage == "not_applicable"
            && row.delivered_context_bytes == 0
            && row.matched_normalized_session_bytes == 0;
        if !common_row_is_valid(
            &row.day,
            row.definition_version,
            &row.ctx_version,
            &row.surface,
            &row.operation,
            &row.outcome,
            &row.value_class,
            &row.duration_bucket,
            &row.target_type,
            &row.pro_outcome,
            row.calls,
            row.result_count,
            row.citation_count,
            row.definition_version == 1,
        ) || !matches!(row.definition_version, 1 | 2)
            || row.delivered_output_bytes < 0
            || row.delivered_context_bytes < 0
            || row.matched_normalized_session_bytes < 0
            || !(complete || unavailable || not_applicable)
            || (row.definition_version == 1
                && ((row.surface == "cli" && row.delivered_output_bytes != 0)
                    || (row.surface == "mcp" && row.delivered_output_bytes <= 0)))
        {
            return Err(UsageStoreError::Integrity);
        }
    }
    validate_maintenance(conn)
}

#[allow(clippy::too_many_arguments)]
fn common_row_is_valid(
    day: &str,
    definition_version: i64,
    ctx_version: &str,
    surface: &str,
    operation: &str,
    outcome: &str,
    value_class: &str,
    duration_bucket: &str,
    target_type: &str,
    pro_outcome: &str,
    calls: i64,
    result_count: i64,
    citation_count: i64,
    allow_legacy_blame_value: bool,
) -> bool {
    let failure_valid = if outcome == "failure" {
        value_class == "not_applicable" && result_count == 0 && citation_count == 0
    } else {
        outcome == "success"
    };
    let value_valid = (value_class == "result_bearing" && result_count >= calls)
        || (matches!(value_class, "empty" | "not_applicable")
            && result_count == 0
            && citation_count == 0);
    let blame = operation == "blame";
    let blame_dimensions = if blame {
        ((outcome == "failure" && pro_outcome == "error")
            || (outcome == "success" && matches!(pro_outcome, "produced" | "possible" | "none")))
            && (matches!(target_type, "file" | "commit" | "pull_request")
                || (outcome == "failure" && target_type == "not_applicable"))
            && (allow_legacy_blame_value
                || outcome == "failure"
                || ((pro_outcome != "produced" && pro_outcome != "possible"
                    || value_class == "result_bearing")
                    && (value_class != "empty" || pro_outcome == "none")))
    } else {
        target_type == "not_applicable" && pro_outcome == "not_applicable" && citation_count == 0
    };
    let classification = if outcome == "failure" {
        value_class == "not_applicable"
    } else if surface == "cli" {
        if blame || definition_version == 2 && operation == "search" {
            matches!(value_class, "result_bearing" | "empty")
        } else {
            value_class == "not_applicable"
        }
    } else if matches!(
        operation,
        "sources" | "search" | "sql" | "show_session" | "show_event" | "blame"
    ) {
        matches!(value_class, "result_bearing" | "empty")
    } else {
        value_class == "not_applicable"
    };
    NaiveDate::parse_from_str(day, "%Y-%m-%d").is_ok()
        && matches!(definition_version, 1 | 2)
        && valid_ctx_version(ctx_version)
        && valid_operation(definition_version, surface, operation)
        && matches!(
            duration_bucket,
            "under_10_ms"
                | "10_to_49_ms"
                | "50_to_249_ms"
                | "250_to_999_ms"
                | "1_to_4_s"
                | "5_to_29_s"
                | "30_s_or_more"
        )
        && calls > 0
        && result_count >= 0
        && citation_count >= 0
        && (citation_count == 0 || blame && outcome == "success")
        && failure_valid
        && value_valid
        && blame_dimensions
        && classification
}

fn valid_ctx_version(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'+' | b'-'))
}

fn valid_operation(definition_version: i64, surface: &str, operation: &str) -> bool {
    match surface {
        "cli" => {
            let common = matches!(
                operation,
                "setup"
                    | "index"
                    | "sources"
                    | "import"
                    | "locate"
                    | "search"
                    | "pro_setup"
                    | "pro_manage"
                    | "pro_uninstall"
                    | "blame"
                    | "sql"
                    | "docs"
                    | "integrations"
                    | "daemon_status"
                    | "daemon_enable"
                    | "daemon_disable"
                    | "upgrade"
                    | "doctor"
            );
            common
                || (definition_version == 1 && operation == "show")
                || (definition_version == 2 && matches!(operation, "show_session" | "show_event"))
        }
        "mcp" => matches!(
            operation,
            "status"
                | "sources"
                | "search"
                | "sql"
                | "show_session"
                | "show_event"
                | "pro_status"
                | "blame"
        ),
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
    let blame = &summary.pro_blame;
    if checked_sum([
        blame.produced_attribution_requests,
        blame.possible_only_requests,
        blame.none_requests,
        blame.error_requests,
    ])? != blame.requests
    {
        return Err(UsageStoreError::Integrity);
    }
    let concrete_requests = checked_sum(blame.by_target.iter().map(|target| target.requests))?;
    if checked_sum([concrete_requests, blame.not_applicable_target_errors])? != blame.requests {
        return Err(UsageStoreError::Integrity);
    }
    for target in &blame.by_target {
        if checked_sum([target.produced, target.possible, target.none, target.error])?
            != target.requests
        {
            return Err(UsageStoreError::Integrity);
        }
    }
    Ok(())
}

pub(super) fn reconcile_definition(definition: &UsageDefinition) -> Result<(), UsageStoreError> {
    reconcile_summary(&definition.summary)?;
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
