use std::{collections::BTreeSet, path::Path, time::SystemTime};

use chrono::NaiveDate;
use rusqlite::{Connection, TransactionBehavior};
use serde::Serialize;
use serde_json::{json, Value};

use super::store::{open_read_only, usage_path, usage_store_exists, verify_report_dates};
use super::{DEFINITION_VERSION, RETENTION_DAYS};

#[derive(Debug, Clone, Serialize)]
pub(crate) struct UsageReport {
    pub(crate) schema_version: i64,
    pub(crate) enabled: bool,
    pub(crate) state: &'static str,
    pub(crate) definition_version: i64,
    pub(crate) retention_days: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) summary: Option<UsageSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) details: Option<UsageDetails>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<UsageReportError>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct UsageSummary {
    pub(crate) first_day_utc: Option<String>,
    pub(crate) last_day_utc: Option<String>,
    pub(crate) active_days: u64,
    pub(crate) ctx_versions: Vec<String>,
    pub(crate) calls: u64,
    pub(crate) successful_calls: u64,
    pub(crate) failed_calls: u64,
    pub(crate) result_bearing_calls: u64,
    pub(crate) empty_calls: u64,
    pub(crate) not_applicable_calls: u64,
    pub(crate) result_count: u64,
    pub(crate) citation_count: u64,
    pub(crate) mcp_response_bytes: u64,
    pub(crate) pro_blame: ProBlameSummary,
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct ProBlameSummary {
    pub(crate) requests: u64,
    pub(crate) produced_attribution_requests: u64,
    pub(crate) possible_or_reference_only_requests: u64,
    pub(crate) no_confident_attribution_requests: u64,
    pub(crate) error_requests: u64,
    pub(crate) by_target: Vec<ProBlameTargetSummary>,
    #[serde(skip)]
    unclassified_target_errors: u64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProBlameTargetSummary {
    pub(crate) target_type: String,
    pub(crate) requests: u64,
    pub(crate) produced: u64,
    pub(crate) possible_or_reference_only: u64,
    pub(crate) none: u64,
    pub(crate) error: u64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct UsageDetails {
    pub(crate) by_operation: Vec<OperationSummary>,
    pub(crate) duration_buckets: Vec<DurationSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct OperationSummary {
    pub(crate) ctx_version: String,
    pub(crate) surface: String,
    pub(crate) operation: String,
    pub(crate) calls: u64,
    pub(crate) successful_calls: u64,
    pub(crate) failed_calls: u64,
    pub(crate) result_bearing_calls: u64,
    pub(crate) empty_calls: u64,
    pub(crate) not_applicable_calls: u64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct DurationSummary {
    pub(crate) duration_bucket: String,
    pub(crate) calls: u64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct UsageReportError {
    pub(crate) code: &'static str,
    pub(crate) message: &'static str,
}

impl UsageReport {
    pub(crate) fn config_error() -> Self {
        Self {
            schema_version: 1,
            enabled: false,
            state: "error",
            definition_version: DEFINITION_VERSION,
            retention_days: RETENTION_DAYS,
            summary: None,
            details: None,
            error: Some(UsageReportError {
                code: "local_usage_config_unavailable",
                message: "local usage configuration could not be read",
            }),
        }
    }
}

pub(crate) fn read_report(data_root: &Path, enabled: bool, detailed: bool) -> UsageReport {
    let path = usage_path(data_root);
    if !enabled {
        return UsageReport {
            schema_version: 1,
            enabled,
            state: "disabled",
            definition_version: DEFINITION_VERSION,
            retention_days: RETENTION_DAYS,
            summary: None,
            details: None,
            error: None,
        };
    }
    let exists = match usage_store_exists(data_root) {
        Ok(exists) => exists,
        Err(error) => return error_report(enabled, error.public_message()),
    };
    if !exists {
        return UsageReport {
            schema_version: 1,
            enabled,
            state: "empty",
            definition_version: DEFINITION_VERSION,
            retention_days: RETENTION_DAYS,
            summary: Some(UsageSummary::default()),
            details: detailed.then(UsageDetails::default),
            error: None,
        };
    }
    match open_read_only(&path).and_then(|mut store| {
        let report = query_report(store.connection_mut(), detailed)?;
        store.verify_unchanged()?;
        Ok(report)
    }) {
        Ok((summary, details)) => UsageReport {
            schema_version: 1,
            enabled,
            state: if summary.calls == 0 { "empty" } else { "ready" },
            definition_version: DEFINITION_VERSION,
            retention_days: RETENTION_DAYS,
            summary: Some(summary),
            details,
            error: None,
        },
        Err(error) => error_report(enabled, error.public_message()),
    }
}

fn error_report(enabled: bool, message: &'static str) -> UsageReport {
    UsageReport {
        schema_version: 1,
        enabled,
        state: "error",
        definition_version: DEFINITION_VERSION,
        retention_days: RETENTION_DAYS,
        summary: None,
        details: None,
        error: Some(UsageReportError {
            code: "usage_store_unavailable",
            message,
        }),
    }
}

fn query_report(
    conn: &mut Connection,
    detailed: bool,
) -> Result<(UsageSummary, Option<UsageDetails>), super::UsageStoreError> {
    let transaction = conn.transaction_with_behavior(TransactionBehavior::Deferred)?;
    verify_report_dates(&transaction, SystemTime::now())?;
    validate_rows(&transaction)?;
    let raw = transaction.query_row(
        r#"
        SELECT
            MIN(day_utc),
            MAX(day_utc),
            COUNT(DISTINCT day_utc),
            COALESCE(SUM(calls), 0),
            COALESCE(SUM(CASE WHEN outcome = 'success' THEN calls ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN outcome = 'failure' THEN calls ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN value_class = 'result_bearing' THEN calls ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN value_class = 'empty' THEN calls ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN value_class = 'not_applicable' THEN calls ELSE 0 END), 0),
            COALESCE(SUM(result_count), 0),
            COALESCE(SUM(citation_count), 0),
            COALESCE(SUM(response_bytes), 0)
        FROM daily_usage
        WHERE definition_version = ?1
        "#,
        [DEFINITION_VERSION],
        |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, i64>(9)?,
                row.get::<_, i64>(10)?,
                row.get::<_, i64>(11)?,
            ))
        },
    )?;
    let mut summary = UsageSummary {
        first_day_utc: raw.0,
        last_day_utc: raw.1,
        active_days: checked_count(raw.2)?,
        ctx_versions: Vec::new(),
        calls: checked_count(raw.3)?,
        successful_calls: checked_count(raw.4)?,
        failed_calls: checked_count(raw.5)?,
        result_bearing_calls: checked_count(raw.6)?,
        empty_calls: checked_count(raw.7)?,
        not_applicable_calls: checked_count(raw.8)?,
        result_count: checked_count(raw.9)?,
        citation_count: checked_count(raw.10)?,
        mcp_response_bytes: checked_count(raw.11)?,
        pro_blame: ProBlameSummary::default(),
    };
    reconcile_summary(&summary)?;
    let mut version_statement = transaction.prepare(
        "SELECT DISTINCT ctx_version FROM daily_usage \
         WHERE definition_version = ?1 ORDER BY ctx_version",
    )?;
    summary.ctx_versions = version_statement
        .query_map([DEFINITION_VERSION], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    summary.pro_blame = query_pro_blame(&transaction)?;
    let all_details = query_details(&transaction)?;
    reconcile_report(&summary, &all_details)?;
    drop(version_statement);
    transaction.commit()?;
    Ok((summary, detailed.then_some(all_details)))
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
    calls: i64,
    result_count: i64,
    citation_count: i64,
    response_bytes: i64,
}

pub(super) fn validate_rows(conn: &Connection) -> Result<(), super::UsageStoreError> {
    let mut statement = conn.prepare(
        r#"
        SELECT day_utc, definition_version, ctx_version, surface, operation,
               outcome, value_class, duration_bucket, target_type, pro_outcome,
               calls, result_count, citation_count, response_bytes
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
            calls: row.get(10)?,
            result_count: row.get(11)?,
            citation_count: row.get(12)?,
            response_bytes: row.get(13)?,
        })
    })?;
    for row in rows {
        if !stored_row_is_valid(&row?) {
            return Err(super::UsageStoreError::Integrity);
        }
    }
    drop(statement);

    let mut maintenance_statement =
        conn.prepare("SELECT singleton, last_retention_day FROM maintenance")?;
    let maintenance_rows = maintenance_statement.query_map([], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut maintenance_count = 0_u8;
    for row in maintenance_rows {
        let (singleton, day) = row?;
        maintenance_count = maintenance_count
            .checked_add(1)
            .ok_or(super::UsageStoreError::Integrity)?;
        if singleton != 1 || day.len() != 10 || NaiveDate::parse_from_str(&day, "%Y-%m-%d").is_err()
        {
            return Err(super::UsageStoreError::Integrity);
        }
    }
    if maintenance_count > 1 {
        return Err(super::UsageStoreError::Integrity);
    }
    Ok(())
}

fn stored_row_is_valid(row: &StoredRow) -> bool {
    let valid_day = row.day.len() == 10 && NaiveDate::parse_from_str(&row.day, "%Y-%m-%d").is_ok();
    let valid_version = !row.ctx_version.is_empty()
        && row.ctx_version.len() <= 64
        && row
            .ctx_version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'+' | b'-'));
    let valid_operation = match row.surface.as_str() {
        "cli" => matches!(
            row.operation.as_str(),
            "setup"
                | "index"
                | "sources"
                | "import"
                | "show"
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
        ),
        "mcp" => matches!(
            row.operation.as_str(),
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
    };
    let valid_duration = matches!(
        row.duration_bucket.as_str(),
        "under_10_ms"
            | "10_to_49_ms"
            | "50_to_249_ms"
            | "250_to_999_ms"
            | "1_to_4_s"
            | "5_to_29_s"
            | "30_s_or_more"
    );
    let nonnegative = row.calls > 0
        && row.result_count >= 0
        && row.citation_count >= 0
        && row.response_bytes >= 0;
    let failure_valid = if row.outcome == "failure" {
        row.value_class == "not_applicable" && row.result_count == 0 && row.citation_count == 0
    } else {
        row.outcome == "success"
    };
    let value_valid = match row.value_class.as_str() {
        "result_bearing" => row.result_count >= row.calls,
        "empty" | "not_applicable" => row.result_count == 0 && row.citation_count == 0,
        _ => false,
    };
    let blame = row.operation == "blame";
    let dimensions_valid = if blame {
        let target_valid = matches!(row.target_type.as_str(), "file" | "commit" | "pull_request")
            || (row.outcome == "failure" && row.target_type == "not_applicable");
        let pro_valid = if row.outcome == "failure" {
            row.pro_outcome == "error"
        } else {
            matches!(row.pro_outcome.as_str(), "produced" | "possible" | "none")
        };
        target_valid && pro_valid
    } else {
        row.target_type == "not_applicable"
            && row.pro_outcome == "not_applicable"
            && row.citation_count == 0
    };
    let classification_valid = match row.surface.as_str() {
        "cli" if row.outcome == "failure" => row.value_class == "not_applicable",
        "cli" if blame => matches!(row.value_class.as_str(), "result_bearing" | "empty"),
        "cli" => row.value_class == "not_applicable",
        "mcp" if row.outcome == "failure" => row.value_class == "not_applicable",
        "mcp"
            if matches!(
                row.operation.as_str(),
                "sources" | "search" | "sql" | "show_session" | "show_event" | "blame"
            ) =>
        {
            matches!(row.value_class.as_str(), "result_bearing" | "empty")
        }
        "mcp" => row.value_class == "not_applicable",
        _ => false,
    };
    let bytes_valid = match row.surface.as_str() {
        "cli" => row.response_bytes == 0,
        "mcp" => row.response_bytes > 0,
        _ => false,
    };
    valid_day
        && row.definition_version == DEFINITION_VERSION
        && valid_version
        && valid_operation
        && valid_duration
        && nonnegative
        && failure_valid
        && value_valid
        && dimensions_valid
        && classification_valid
        && bytes_valid
}

fn checked_count(value: i64) -> Result<u64, super::UsageStoreError> {
    u64::try_from(value).map_err(|_| super::UsageStoreError::Integrity)
}

fn checked_sum(values: impl IntoIterator<Item = u64>) -> Result<u64, super::UsageStoreError> {
    values.into_iter().try_fold(0_u64, |total, value| {
        total
            .checked_add(value)
            .ok_or(super::UsageStoreError::Integrity)
    })
}

fn reconcile_summary(summary: &UsageSummary) -> Result<(), super::UsageStoreError> {
    if checked_sum([summary.successful_calls, summary.failed_calls])? != summary.calls
        || checked_sum([
            summary.result_bearing_calls,
            summary.empty_calls,
            summary.not_applicable_calls,
        ])? != summary.calls
    {
        return Err(super::UsageStoreError::Integrity);
    }
    Ok(())
}

fn reconcile_report(
    summary: &UsageSummary,
    details: &UsageDetails,
) -> Result<(), super::UsageStoreError> {
    let operation_calls = checked_sum(details.by_operation.iter().map(|row| row.calls))?;
    let duration_calls = checked_sum(details.duration_buckets.iter().map(|row| row.calls))?;
    if operation_calls != summary.calls || duration_calls != summary.calls {
        return Err(super::UsageStoreError::Integrity);
    }
    for row in &details.by_operation {
        if checked_sum([row.successful_calls, row.failed_calls])? != row.calls
            || checked_sum([
                row.result_bearing_calls,
                row.empty_calls,
                row.not_applicable_calls,
            ])? != row.calls
        {
            return Err(super::UsageStoreError::Integrity);
        }
    }
    let detail_versions = details
        .by_operation
        .iter()
        .map(|row| row.ctx_version.clone())
        .collect::<BTreeSet<_>>();
    if summary
        .ctx_versions
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        != detail_versions
    {
        return Err(super::UsageStoreError::Integrity);
    }
    let blame = &summary.pro_blame;
    if checked_sum([
        blame.produced_attribution_requests,
        blame.possible_or_reference_only_requests,
        blame.no_confident_attribution_requests,
        blame.error_requests,
    ])? != blame.requests
    {
        return Err(super::UsageStoreError::Integrity);
    }
    let targeted = checked_sum(blame.by_target.iter().map(|target| target.requests))?;
    if checked_sum([targeted, blame.unclassified_target_errors])? != blame.requests {
        return Err(super::UsageStoreError::Integrity);
    }
    for target in &blame.by_target {
        if checked_sum([
            target.produced,
            target.possible_or_reference_only,
            target.none,
            target.error,
        ])? != target.requests
        {
            return Err(super::UsageStoreError::Integrity);
        }
    }
    Ok(())
}

fn query_pro_blame(conn: &Connection) -> Result<ProBlameSummary, super::UsageStoreError> {
    let mut result = conn.query_row(
        r#"
        SELECT
            COALESCE(SUM(calls), 0),
            COALESCE(SUM(CASE WHEN pro_outcome = 'produced' THEN calls ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN pro_outcome = 'possible' THEN calls ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN pro_outcome = 'none' THEN calls ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN pro_outcome = 'error' THEN calls ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN target_type = 'not_applicable' THEN calls ELSE 0 END), 0)
        FROM daily_usage
        WHERE definition_version = ?1 AND operation = 'blame'
        "#,
        [DEFINITION_VERSION],
        |row| {
            Ok(ProBlameSummary {
                requests: row.get(0)?,
                produced_attribution_requests: row.get(1)?,
                possible_or_reference_only_requests: row.get(2)?,
                no_confident_attribution_requests: row.get(3)?,
                error_requests: row.get(4)?,
                by_target: Vec::new(),
                unclassified_target_errors: row.get(5)?,
            })
        },
    )?;
    let mut statement = conn.prepare(
        r#"
        SELECT
            target_type,
            SUM(calls),
            SUM(CASE WHEN pro_outcome = 'produced' THEN calls ELSE 0 END),
            SUM(CASE WHEN pro_outcome = 'possible' THEN calls ELSE 0 END),
            SUM(CASE WHEN pro_outcome = 'none' THEN calls ELSE 0 END),
            SUM(CASE WHEN pro_outcome = 'error' THEN calls ELSE 0 END)
        FROM daily_usage
        WHERE definition_version = ?1
          AND operation = 'blame'
          AND target_type IN ('file', 'commit', 'pull_request')
        GROUP BY target_type
        ORDER BY CASE target_type
            WHEN 'file' THEN 1 WHEN 'commit' THEN 2 WHEN 'pull_request' THEN 3 ELSE 4 END
        "#,
    )?;
    let rows = statement.query_map([DEFINITION_VERSION], |row| {
        Ok(ProBlameTargetSummary {
            target_type: row.get(0)?,
            requests: row.get(1)?,
            produced: row.get(2)?,
            possible_or_reference_only: row.get(3)?,
            none: row.get(4)?,
            error: row.get(5)?,
        })
    })?;
    for row in rows {
        result.by_target.push(row?);
    }
    Ok(result)
}

fn query_details(conn: &Connection) -> Result<UsageDetails, super::UsageStoreError> {
    let mut by_operation = Vec::new();
    let mut statement = conn.prepare(
        r#"
        SELECT
            ctx_version,
            surface,
            operation,
            SUM(calls),
            SUM(CASE WHEN outcome = 'success' THEN calls ELSE 0 END),
            SUM(CASE WHEN outcome = 'failure' THEN calls ELSE 0 END),
            SUM(CASE WHEN value_class = 'result_bearing' THEN calls ELSE 0 END),
            SUM(CASE WHEN value_class = 'empty' THEN calls ELSE 0 END),
            SUM(CASE WHEN value_class = 'not_applicable' THEN calls ELSE 0 END)
        FROM daily_usage
        WHERE definition_version = ?1
        GROUP BY ctx_version, surface, operation
        ORDER BY ctx_version, surface, operation
        "#,
    )?;
    let rows = statement.query_map([DEFINITION_VERSION], |row| {
        Ok(OperationSummary {
            ctx_version: row.get(0)?,
            surface: row.get(1)?,
            operation: row.get(2)?,
            calls: row.get(3)?,
            successful_calls: row.get(4)?,
            failed_calls: row.get(5)?,
            result_bearing_calls: row.get(6)?,
            empty_calls: row.get(7)?,
            not_applicable_calls: row.get(8)?,
        })
    })?;
    for row in rows {
        by_operation.push(row?);
    }

    let mut duration_buckets = Vec::new();
    let mut statement = conn.prepare(
        r#"
        SELECT duration_bucket, SUM(calls)
        FROM daily_usage
        WHERE definition_version = ?1
        GROUP BY duration_bucket
        ORDER BY CASE duration_bucket
            WHEN 'under_10_ms' THEN 1 WHEN '10_to_49_ms' THEN 2
            WHEN '50_to_249_ms' THEN 3 WHEN '250_to_999_ms' THEN 4
            WHEN '1_to_4_s' THEN 5 WHEN '5_to_29_s' THEN 6 ELSE 7 END
        "#,
    )?;
    let rows = statement.query_map([DEFINITION_VERSION], |row| {
        Ok(DurationSummary {
            duration_bucket: row.get(0)?,
            calls: row.get(1)?,
        })
    })?;
    for row in rows {
        duration_buckets.push(row?);
    }
    Ok(UsageDetails {
        by_operation,
        duration_buckets,
    })
}

impl Default for UsageDetails {
    fn default() -> Self {
        Self {
            by_operation: Vec::new(),
            duration_buckets: Vec::new(),
        }
    }
}

pub(crate) fn render_human_summary(report: &UsageReport, detailed: bool) {
    println!("local_usage: {}", report.state);
    let Some(summary) = &report.summary else {
        if let Some(error) = &report.error {
            println!("local_usage_error: {} ({})", error.code, error.message);
        }
        return;
    };
    println!("usage_calls: {}", summary.calls);
    println!("usage_active_utc_days: {}", summary.active_days);
    println!(
        "usage_classified_result_sets: {} nonempty, {} empty",
        summary.result_bearing_calls, summary.empty_calls
    );
    println!(
        "usage_no_result_set_classification: {} calls",
        summary.not_applicable_calls
    );
    let blame = &summary.pro_blame;
    if blame.requests > 0 {
        println!(
            "Pro returned produced attribution in {} of {} blame requests.",
            blame.produced_attribution_requests, blame.requests
        );
        println!(
            "pro_blame_outcomes: produced-attribution {}, possible-only {}, none {}, error {}",
            blame.produced_attribution_requests,
            blame.possible_or_reference_only_requests,
            blame.no_confident_attribution_requests,
            blame.error_requests
        );
        for target in &blame.by_target {
            println!(
                "  {}: produced-attribution {}, possible-only/reference-only {}, none {}, error {}",
                target.target_type,
                target.produced,
                target.possible_or_reference_only,
                target.none,
                target.error
            );
        }
    }
    if detailed {
        if let Some(details) = &report.details {
            for operation in &details.by_operation {
                println!(
                    "usage_operation: {}/{}/{} calls={} success={} failure={} result={} empty={} not-applicable={}",
                    operation.ctx_version,
                    operation.surface,
                    operation.operation,
                    operation.calls,
                    operation.successful_calls,
                    operation.failed_calls,
                    operation.result_bearing_calls,
                    operation.empty_calls,
                    operation.not_applicable_calls
                );
            }
            for duration in &details.duration_buckets {
                println!(
                    "usage_duration: {} calls={}",
                    duration.duration_bucket, duration.calls
                );
            }
        }
    }
}

pub(crate) fn pro_conversion_action(access_state: Option<&str>) -> Option<Value> {
    match access_state {
        Some("trial") => Some(json!({
            "kind": "pro_monthly_conversion",
            "price": "$15/month",
            "command": "ctx pro manage",
            "reason": "trial_active",
        })),
        Some("locked") => Some(json!({
            "kind": "pro_restore_access",
            "command": "ctx pro manage",
            "reason": "access_locked",
            "graph_preserved": true,
        })),
        Some("active" | "canceling_paid" | "offline_grace") | None | Some(_) => None,
    }
}
