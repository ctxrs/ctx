use std::{collections::BTreeSet, path::Path, time::SystemTime};

use chrono::NaiveDate;
use rusqlite::{Connection, TransactionBehavior};
use serde::Serialize;
use serde_json::{json, Value};

use super::store::{open_read_only, usage_path, usage_store_exists, verify_report_dates};
use super::{
    estimate_usage, CoveredTokenEstimate, EstimateFacts, UsageEstimates, DEFINITION_VERSION,
    RETENTION_DAYS,
};

const HUMAN_OUTPUT_WIDTH: usize = 80;

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
    pub(crate) estimates: Option<UsageEstimates>,
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
    pub(crate) mcp_response_byte_samples: u64,
    pub(crate) cli_output_bytes: u64,
    pub(crate) cli_output_byte_samples: u64,
    pub(crate) measured_latency_ms: u64,
    pub(crate) measured_latency_samples: u64,
    pub(crate) semantic_context_bytes: u64,
    pub(crate) semantic_context_byte_samples: u64,
    pub(crate) semantic_search_result_bytes: u64,
    pub(crate) semantic_search_result_byte_samples: u64,
    pub(crate) context: ContextProxySummary,
    pub(crate) result_actions: ResultActionSummary,
    pub(crate) pro_blame: ProBlameSummary,
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct ContextProxySummary {
    pub(crate) context_searches: u64,
    pub(crate) context_found: u64,
    pub(crate) context_opened: u64,
    pub(crate) context_cited: u64,
    pub(crate) validated_discoveries: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct ResultActionSummary {
    pub(crate) searches: u64,
    pub(crate) result_bearing_searches: u64,
    pub(crate) sessions_opened: u64,
    pub(crate) events_opened: u64,
    pub(crate) locate_requests: u64,
    pub(crate) records_located: u64,
    pub(crate) sources_requests: u64,
    pub(crate) sql_requests: u64,
    pub(crate) blame_requests: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct ProBlameSummary {
    pub(crate) requests: u64,
    pub(crate) citation_count: u64,
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

#[derive(Debug, Clone, Default, Serialize)]
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
            schema_version: 2,
            enabled: false,
            state: "error",
            definition_version: DEFINITION_VERSION,
            retention_days: RETENTION_DAYS,
            summary: None,
            details: None,
            estimates: None,
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
            schema_version: 2,
            enabled,
            state: "disabled",
            definition_version: DEFINITION_VERSION,
            retention_days: RETENTION_DAYS,
            summary: None,
            details: None,
            estimates: None,
            error: None,
        };
    }
    let exists = match usage_store_exists(data_root) {
        Ok(exists) => exists,
        Err(error) => return error_report(enabled, error.public_message()),
    };
    if !exists {
        return UsageReport {
            schema_version: 2,
            enabled,
            state: "empty",
            definition_version: DEFINITION_VERSION,
            retention_days: RETENTION_DAYS,
            summary: Some(UsageSummary::default()),
            details: detailed.then(UsageDetails::default),
            estimates: Some(estimate_usage(EstimateFacts::default())),
            error: None,
        };
    }
    match open_read_only(&path).and_then(|mut store| {
        let report = query_report(store.connection_mut(), detailed)?;
        store.verify_unchanged()?;
        Ok(report)
    }) {
        Ok((summary, details)) => {
            let estimates = estimate_usage(estimate_facts(&summary));
            UsageReport {
                schema_version: 2,
                enabled,
                state: if summary.calls == 0 { "empty" } else { "ready" },
                definition_version: DEFINITION_VERSION,
                retention_days: RETENTION_DAYS,
                summary: Some(summary),
                details,
                estimates: Some(estimates),
                error: None,
            }
        }
        Err(error) => error_report(enabled, error.public_message()),
    }
}

fn error_report(enabled: bool, message: &'static str) -> UsageReport {
    UsageReport {
        schema_version: 2,
        enabled,
        state: "error",
        definition_version: DEFINITION_VERSION,
        retention_days: RETENTION_DAYS,
        summary: None,
        details: None,
        estimates: None,
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
            COALESCE(SUM(response_bytes), 0),
            COALESCE(SUM(response_byte_samples), 0),
            COALESCE(SUM(output_bytes), 0),
            COALESCE(SUM(output_byte_samples), 0),
            COALESCE(SUM(latency_ms), 0),
            COALESCE(SUM(latency_samples), 0),
            COALESCE(SUM(context_bytes), 0),
            COALESCE(SUM(context_byte_samples), 0),
            COALESCE(SUM(search_result_bytes), 0),
            COALESCE(SUM(search_result_byte_samples), 0),
            COALESCE(SUM(context_searches), 0),
            COALESCE(SUM(context_found), 0),
            COALESCE(SUM(context_opened), 0),
            COALESCE(SUM(context_cited), 0),
            COALESCE(SUM(validated_discoveries), 0)
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
                row.get::<_, i64>(12)?,
                row.get::<_, i64>(13)?,
                row.get::<_, i64>(14)?,
                row.get::<_, i64>(15)?,
                row.get::<_, i64>(16)?,
                row.get::<_, i64>(17)?,
                row.get::<_, i64>(18)?,
                row.get::<_, i64>(19)?,
                row.get::<_, i64>(20)?,
                row.get::<_, i64>(21)?,
                row.get::<_, i64>(22)?,
                row.get::<_, i64>(23)?,
                row.get::<_, i64>(24)?,
                row.get::<_, i64>(25)?,
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
        mcp_response_byte_samples: checked_count(raw.12)?,
        cli_output_bytes: checked_count(raw.13)?,
        cli_output_byte_samples: checked_count(raw.14)?,
        measured_latency_ms: checked_count(raw.15)?,
        measured_latency_samples: checked_count(raw.16)?,
        semantic_context_bytes: checked_count(raw.17)?,
        semantic_context_byte_samples: checked_count(raw.18)?,
        semantic_search_result_bytes: checked_count(raw.19)?,
        semantic_search_result_byte_samples: checked_count(raw.20)?,
        context: ContextProxySummary {
            context_searches: checked_count(raw.21)?,
            context_found: checked_count(raw.22)?,
            context_opened: checked_count(raw.23)?,
            context_cited: checked_count(raw.24)?,
            validated_discoveries: checked_count(raw.25)?,
        },
        result_actions: ResultActionSummary::default(),
        pro_blame: ProBlameSummary::default(),
    };
    let mut version_statement = transaction.prepare(
        "SELECT DISTINCT ctx_version FROM daily_usage \
         WHERE definition_version = ?1 ORDER BY ctx_version",
    )?;
    summary.ctx_versions = version_statement
        .query_map([DEFINITION_VERSION], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    summary.pro_blame = query_pro_blame(&transaction)?;
    summary.result_actions = query_result_actions(&transaction)?;
    reconcile_summary(&summary)?;
    let all_details = query_details(&transaction)?;
    reconcile_report(&summary, &all_details)?;
    drop(version_statement);
    transaction.commit()?;
    Ok((summary, detailed.then_some(all_details)))
}

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

struct StoredRowV2 {
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
    result_action: String,
    calls: i64,
    result_count: i64,
    citation_count: i64,
    latency_ms: i64,
    latency_samples: i64,
    response_bytes: i64,
    response_byte_samples: i64,
    output_bytes: i64,
    output_byte_samples: i64,
    context_bytes: i64,
    context_byte_samples: i64,
    search_result_bytes: i64,
    search_result_byte_samples: i64,
    context_searches: i64,
    context_found: i64,
    context_opened: i64,
    context_cited: i64,
    validated_discoveries: i64,
}

pub(super) fn validate_rows_for_schema(
    conn: &Connection,
    schema_version: i64,
) -> Result<(), super::UsageStoreError> {
    match schema_version {
        1 => validate_rows_v1(conn),
        2 => validate_rows(conn),
        version => Err(super::UsageStoreError::SchemaVersion(version)),
    }
}

fn validate_rows_v1(conn: &Connection) -> Result<(), super::UsageStoreError> {
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
        if !stored_row_v1_is_valid(&row?) {
            return Err(super::UsageStoreError::Integrity);
        }
    }
    drop(statement);
    validate_maintenance(conn)
}

pub(super) fn validate_rows(conn: &Connection) -> Result<(), super::UsageStoreError> {
    let mut statement = conn.prepare(
        r#"
        SELECT day_utc, definition_version, ctx_version, surface, operation,
               outcome, value_class, duration_bucket, target_type, pro_outcome,
               result_action, calls, result_count, citation_count, latency_ms,
               latency_samples, response_bytes, response_byte_samples,
               output_bytes, output_byte_samples, context_bytes,
               context_byte_samples, search_result_bytes,
               search_result_byte_samples, context_searches, context_found,
               context_opened, context_cited, validated_discoveries
        FROM daily_usage
        "#,
    )?;
    let rows = statement.query_map([], |row| {
        Ok(StoredRowV2 {
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
            result_action: row.get(10)?,
            calls: row.get(11)?,
            result_count: row.get(12)?,
            citation_count: row.get(13)?,
            latency_ms: row.get(14)?,
            latency_samples: row.get(15)?,
            response_bytes: row.get(16)?,
            response_byte_samples: row.get(17)?,
            output_bytes: row.get(18)?,
            output_byte_samples: row.get(19)?,
            context_bytes: row.get(20)?,
            context_byte_samples: row.get(21)?,
            search_result_bytes: row.get(22)?,
            search_result_byte_samples: row.get(23)?,
            context_searches: row.get(24)?,
            context_found: row.get(25)?,
            context_opened: row.get(26)?,
            context_cited: row.get(27)?,
            validated_discoveries: row.get(28)?,
        })
    })?;
    for row in rows {
        if !stored_row_v2_is_valid(&row?) {
            return Err(super::UsageStoreError::Integrity);
        }
    }
    drop(statement);
    validate_maintenance(conn)
}

fn validate_maintenance(conn: &Connection) -> Result<(), super::UsageStoreError> {
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

fn stored_row_v1_is_valid(row: &StoredRowV1) -> bool {
    let nonnegative = row.calls > 0
        && row.result_count >= 0
        && row.citation_count >= 0
        && row.response_bytes >= 0;
    let failure_valid = if row.outcome == "failure" {
        row.value_class == "not_applicable" && row.result_count == 0 && row.citation_count == 0
    } else {
        row.outcome == "success"
    };
    let value_valid = value_class_is_valid(
        &row.value_class,
        row.calls,
        row.result_count,
        row.citation_count,
    );
    let blame = row.operation == "blame";
    let dimensions_valid = dimensions_are_valid(
        &row.operation,
        &row.outcome,
        &row.target_type,
        &row.pro_outcome,
    ) && (blame || row.citation_count == 0);
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
    common_dimensions_are_valid(
        &row.day,
        row.definition_version,
        1,
        &row.ctx_version,
        &row.surface,
        &row.operation,
        &row.duration_bucket,
    ) && nonnegative
        && failure_valid
        && value_valid
        && dimensions_valid
        && classification_valid
        && bytes_valid
}

fn stored_row_v2_is_valid(row: &StoredRowV2) -> bool {
    let counters = [
        row.result_count,
        row.citation_count,
        row.latency_ms,
        row.latency_samples,
        row.response_bytes,
        row.response_byte_samples,
        row.output_bytes,
        row.output_byte_samples,
        row.context_bytes,
        row.context_byte_samples,
        row.search_result_bytes,
        row.search_result_byte_samples,
        row.context_searches,
        row.context_found,
        row.context_opened,
        row.context_cited,
        row.validated_discoveries,
    ];
    let nonnegative = row.calls > 0 && counters.into_iter().all(|value| value >= 0);
    let failure_valid = if row.outcome == "failure" {
        row.value_class == "not_applicable"
            && row.result_action == "not_applicable"
            && row.result_count == 0
            && row.citation_count == 0
            && row.context_bytes == 0
            && row.context_byte_samples == 0
            && row.search_result_bytes == 0
            && row.search_result_byte_samples == 0
            && row.context_searches == 0
            && row.context_found == 0
            && row.context_opened == 0
            && row.context_cited == 0
            && row.validated_discoveries == 0
    } else {
        row.outcome == "success"
    };
    let value_valid = value_class_is_valid(
        &row.value_class,
        row.calls,
        row.result_count,
        row.citation_count,
    );
    let sample_counts_valid = [
        row.latency_samples,
        row.response_byte_samples,
        row.output_byte_samples,
        row.context_byte_samples,
        row.search_result_byte_samples,
    ]
    .into_iter()
    .all(|samples| samples <= row.calls);
    let latency_valid =
        (row.latency_samples == 0 && row.latency_ms == 0) || row.latency_samples > 0;
    let delivery_bytes_valid = match row.surface.as_str() {
        "cli" => {
            row.response_bytes == 0
                && row.response_byte_samples == 0
                && ((row.output_byte_samples == 0 && row.output_bytes == 0)
                    || row.output_byte_samples > 0)
        }
        "mcp" => {
            row.response_bytes > 0
                && row.response_byte_samples == row.calls
                && row.output_bytes == 0
                && row.output_byte_samples == 0
        }
        _ => false,
    };
    let context_bytes_valid =
        (row.context_byte_samples == 0 && row.context_bytes == 0) || row.context_byte_samples > 0;
    let search_bytes_valid = if row.operation == "search"
        && row.outcome == "success"
        && row.value_class == "result_bearing"
    {
        ((row.search_result_byte_samples == 0 && row.search_result_bytes == 0)
            || row.search_result_byte_samples > 0)
            && row.search_result_byte_samples <= row.context_byte_samples
            && row.search_result_bytes <= row.context_bytes
    } else {
        row.search_result_bytes == 0 && row.search_result_byte_samples == 0
    };
    let proxies_are_zero = row.context_searches == 0
        && row.context_found == 0
        && row.context_opened == 0
        && row.context_cited == 0
        && row.validated_discoveries == 0;
    let context_proxies_valid = match (row.outcome.as_str(), row.result_action.as_str()) {
        ("success", "search") => {
            row.context_searches <= row.calls
                && row.context_found <= row.result_count
                && (row.context_found == 0 || row.context_searches > 0)
                && row.context_opened == 0
                && row.context_cited == 0
                && row.validated_discoveries == 0
        }
        ("success", "open_session" | "open_event") => {
            row.context_searches == 0
                && row.context_found == 0
                && row.context_opened <= row.calls
                && row.context_cited == 0
                && row.validated_discoveries <= row.calls
                && row
                    .context_opened
                    .checked_add(row.context_cited)
                    .is_some_and(|validated_sources| row.validated_discoveries <= validated_sources)
        }
        _ => proxies_are_zero,
    };
    let action_valid = result_action_is_valid(
        &row.result_action,
        &row.surface,
        &row.operation,
        &row.outcome,
        &row.value_class,
    );
    common_dimensions_are_valid(
        &row.day,
        row.definition_version,
        DEFINITION_VERSION,
        &row.ctx_version,
        &row.surface,
        &row.operation,
        &row.duration_bucket,
    ) && nonnegative
        && failure_valid
        && value_valid
        && sample_counts_valid
        && latency_valid
        && delivery_bytes_valid
        && context_bytes_valid
        && search_bytes_valid
        && context_proxies_valid
        && dimensions_are_valid(
            &row.operation,
            &row.outcome,
            &row.target_type,
            &row.pro_outcome,
        )
        && action_valid
}

fn common_dimensions_are_valid(
    day: &str,
    definition_version: i64,
    expected_definition_version: i64,
    ctx_version: &str,
    surface: &str,
    operation: &str,
    duration_bucket: &str,
) -> bool {
    let valid_day = day.len() == 10 && NaiveDate::parse_from_str(day, "%Y-%m-%d").is_ok();
    let valid_version = !ctx_version.is_empty()
        && ctx_version.len() <= 64
        && ctx_version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'+' | b'-'));
    let valid_operation = match surface {
        "cli" => matches!(
            operation,
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
    };
    let valid_duration = matches!(
        duration_bucket,
        "under_10_ms"
            | "10_to_49_ms"
            | "50_to_249_ms"
            | "250_to_999_ms"
            | "1_to_4_s"
            | "5_to_29_s"
            | "30_s_or_more"
    );
    valid_day
        && definition_version == expected_definition_version
        && valid_version
        && valid_operation
        && valid_duration
}

fn value_class_is_valid(
    value_class: &str,
    calls: i64,
    result_count: i64,
    citation_count: i64,
) -> bool {
    match value_class {
        "result_bearing" => result_count >= calls,
        "empty" | "not_applicable" => result_count == 0 && citation_count == 0,
        _ => false,
    }
}

fn dimensions_are_valid(
    operation: &str,
    outcome: &str,
    target_type: &str,
    pro_outcome: &str,
) -> bool {
    if operation == "blame" {
        let target_valid = matches!(target_type, "file" | "commit" | "pull_request")
            || (outcome == "failure" && target_type == "not_applicable");
        let pro_valid = if outcome == "failure" {
            pro_outcome == "error"
        } else {
            outcome == "success" && matches!(pro_outcome, "produced" | "possible" | "none")
        };
        target_valid && pro_valid
    } else {
        target_type == "not_applicable" && pro_outcome == "not_applicable"
    }
}

fn result_action_is_valid(
    result_action: &str,
    surface: &str,
    operation: &str,
    outcome: &str,
    value_class: &str,
) -> bool {
    if outcome == "failure" {
        return result_action == "not_applicable" && value_class == "not_applicable";
    }
    let action_matches = match result_action {
        "search" => operation == "search",
        "open_session" => {
            (surface == "cli" && operation == "show")
                || (surface == "mcp" && operation == "show_session")
        }
        "open_event" => {
            (surface == "cli" && operation == "show")
                || (surface == "mcp" && operation == "show_event")
        }
        "locate" => surface == "cli" && operation == "locate",
        "sources" => operation == "sources",
        "sql" => operation == "sql",
        "blame" => operation == "blame",
        "not_applicable" => value_class == "not_applicable",
        _ => false,
    };
    let classification_matches = if result_action == "not_applicable" {
        value_class == "not_applicable"
    } else {
        matches!(value_class, "result_bearing" | "empty")
    };
    let mcp_classified = surface != "mcp"
        || matches!(operation, "status" | "pro_status")
        || result_action != "not_applicable";
    action_matches && classification_matches && mcp_classified
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
    let actions = &summary.result_actions;
    let context = &summary.context;
    let semantic_context_eligible_samples = checked_sum([
        actions.searches,
        actions.sessions_opened,
        actions.events_opened,
        actions.locate_requests,
        actions.sources_requests,
        actions.sql_requests,
        actions.blame_requests,
    ])?;
    let validation_sources = checked_sum([context.context_opened, context.context_cited])?;
    if checked_sum([summary.successful_calls, summary.failed_calls])? != summary.calls
        || checked_sum([
            summary.result_bearing_calls,
            summary.empty_calls,
            summary.not_applicable_calls,
        ])? != summary.calls
        || summary.semantic_context_byte_samples > semantic_context_eligible_samples
        || summary.semantic_search_result_byte_samples > actions.result_bearing_searches
        || summary.semantic_search_result_bytes > summary.semantic_context_bytes
        || summary.pro_blame.citation_count > summary.citation_count
        || context.context_searches > actions.searches
        || context.context_found > summary.result_count
        || (context.context_found > 0 && context.context_searches == 0)
        || context.context_opened > context.context_found
        || context.context_cited > context.context_found
        || context.validated_discoveries > context.context_found
        || context.validated_discoveries > validation_sources
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
            COALESCE(SUM(CASE
                WHEN result_action = 'blame' THEN citation_count ELSE 0 END
            ), 0),
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
                citation_count: row.get(1)?,
                produced_attribution_requests: row.get(2)?,
                possible_or_reference_only_requests: row.get(3)?,
                no_confident_attribution_requests: row.get(4)?,
                error_requests: row.get(5)?,
                by_target: Vec::new(),
                unclassified_target_errors: row.get(6)?,
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

fn query_result_actions(conn: &Connection) -> Result<ResultActionSummary, super::UsageStoreError> {
    let raw = conn.query_row(
        r#"
        SELECT
            COALESCE(SUM(CASE WHEN result_action = 'search' THEN calls ELSE 0 END), 0),
            COALESCE(SUM(CASE
                WHEN result_action = 'search' AND value_class = 'result_bearing'
                THEN calls ELSE 0 END
            ), 0),
            COALESCE(SUM(CASE WHEN result_action = 'open_session' THEN calls ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN result_action = 'open_event' THEN calls ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN result_action = 'locate' THEN calls ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN result_action = 'locate' THEN result_count ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN result_action = 'sources' THEN calls ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN result_action = 'sql' THEN calls ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN result_action = 'blame' THEN calls ELSE 0 END), 0)
        FROM daily_usage
        WHERE definition_version = ?1
        "#,
        [DEFINITION_VERSION],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
            ))
        },
    )?;
    Ok(ResultActionSummary {
        searches: checked_count(raw.0)?,
        result_bearing_searches: checked_count(raw.1)?,
        sessions_opened: checked_count(raw.2)?,
        events_opened: checked_count(raw.3)?,
        locate_requests: checked_count(raw.4)?,
        records_located: checked_count(raw.5)?,
        sources_requests: checked_count(raw.6)?,
        sql_requests: checked_count(raw.7)?,
        blame_requests: checked_count(raw.8)?,
    })
}

fn estimate_facts(summary: &UsageSummary) -> EstimateFacts {
    let actions = &summary.result_actions;
    let semantic_context_eligible_samples = [
        actions.searches,
        actions.sessions_opened,
        actions.events_opened,
        actions.locate_requests,
        actions.sources_requests,
        actions.sql_requests,
        actions.blame_requests,
    ]
    .into_iter()
    .fold(0_u64, u64::saturating_add);
    EstimateFacts {
        result_bearing_searches: actions.result_bearing_searches,
        semantic_context_eligible_samples,
        semantic_context_bytes: summary.semantic_context_bytes,
        semantic_context_byte_samples: summary.semantic_context_byte_samples,
        semantic_search_result_bytes: summary.semantic_search_result_bytes,
        semantic_search_result_byte_samples: summary.semantic_search_result_byte_samples,
        discovered_record_opens: summary.context.context_opened,
        produced_blame_requests: summary.pro_blame.produced_attribution_requests,
        possible_blame_requests: summary.pro_blame.possible_or_reference_only_requests,
    }
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
        "usage_measured_latency_ms: {} samples={}",
        summary.measured_latency_ms, summary.measured_latency_samples
    );
    println!(
        "usage_cli_output_bytes: {} samples={}",
        summary.cli_output_bytes, summary.cli_output_byte_samples
    );
    println!(
        "usage_mcp_transport_bytes: {} samples={}",
        summary.mcp_response_bytes, summary.mcp_response_byte_samples
    );
    println!(
        "usage_semantic_context_bytes: {} samples={}",
        summary.semantic_context_bytes, summary.semantic_context_byte_samples
    );
    println!(
        "usage_semantic_search_result_bytes: {} samples={}",
        summary.semantic_search_result_bytes, summary.semantic_search_result_byte_samples
    );
    println!(
        "usage_context_proxies: searches={} found={} opened={} cited={} validated={}",
        summary.context.context_searches,
        summary.context.context_found,
        summary.context.context_opened,
        summary.context.context_cited,
        summary.context.validated_discoveries
    );
    if let Some(estimates) = &report.estimates {
        println!("usage_estimate_model: {}", estimates.model.version);
        print_token_estimate(
            "usage_approximate_context_tokens",
            estimates.approximate_context_tokens,
        );
        print_token_estimate(
            "usage_approximate_avoided_context_tokens",
            estimates.approximate_avoided_context_tokens,
        );
        println!(
            "usage_estimated_time_saved_seconds: {}",
            estimates.estimated_time_saved_seconds
        );
    }
    println!(
        "usage_mcp_pro_result_classification: {} nonempty, {} empty",
        summary.result_bearing_calls, summary.empty_calls
    );
    println!(
        "usage_mcp_pro_result_classification_not_applicable: {} calls",
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
                    "usage_operation: {}/{}",
                    operation.surface, operation.operation
                );
                print_wrapped_fields([
                    format!("ctx_version={}", operation.ctx_version),
                    format!("calls={}", operation.calls),
                    format!("success={}", operation.successful_calls),
                    format!("failure={}", operation.failed_calls),
                    format!("result={}", operation.result_bearing_calls),
                    format!("empty={}", operation.empty_calls),
                    format!("not-applicable={}", operation.not_applicable_calls),
                ]);
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

fn print_token_estimate(label: &str, estimate: CoveredTokenEstimate) {
    match estimate.approximate_tokens {
        Some(tokens) => println!(
            "{label}: {tokens} coverage={} samples={}/{}",
            estimate.coverage.as_str(),
            estimate.measured_samples,
            estimate.eligible_samples
        ),
        None => println!(
            "{label}: unavailable coverage={} samples={}/{}",
            estimate.coverage.as_str(),
            estimate.measured_samples,
            estimate.eligible_samples
        ),
    }
}

fn print_wrapped_fields(fields: impl IntoIterator<Item = String>) {
    let mut line = String::from("  ");
    for field in fields {
        let separator_width = usize::from(line.len() > 2);
        if line.len() + separator_width + field.len() > HUMAN_OUTPUT_WIDTH {
            println!("{line}");
            line.truncate(2);
        } else if separator_width > 0 {
            line.push(' ');
        }
        line.push_str(&field);
    }
    if line.len() > 2 {
        println!("{line}");
    }
}

pub(crate) fn pro_conversion_action(access_state: Option<&str>) -> Option<Value> {
    match access_state {
        Some("trial") => Some(json!({
            "kind": "pro_monthly_conversion",
            "price": "$20/month",
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
