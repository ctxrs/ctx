use std::collections::BTreeSet;

use chrono::NaiveDate;
use rusqlite::Connection;

use super::super::{store, UsageStoreError, DEFINITION_VERSION};
use super::{UsageDetails, UsageSummary};

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
        if !stored_row_v1_is_valid(&row?, normalize_legacy_blame) {
            return Err(UsageStoreError::Integrity);
        }
    }
    drop(statement);
    validate_maintenance(conn)
}

pub(in crate::local_usage) fn validate_rows(conn: &Connection) -> Result<(), UsageStoreError> {
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
            return Err(UsageStoreError::Integrity);
        }
    }
    drop(statement);
    validate_maintenance(conn)
}

fn validate_maintenance(conn: &Connection) -> Result<(), UsageStoreError> {
    let has_store_generation = conn
        .prepare("PRAGMA table_info(maintenance)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .any(|column| column == "store_generation");
    let query = if has_store_generation {
        "SELECT singleton, last_retention_day, store_generation FROM maintenance"
    } else {
        "SELECT singleton, last_retention_day, NULL FROM maintenance"
    };
    let mut maintenance_statement = conn.prepare(query)?;
    let maintenance_rows = maintenance_statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<i64>>(2)?,
        ))
    })?;
    let mut maintenance_count = 0_u8;
    for row in maintenance_rows {
        let (singleton, day, store_generation) = row?;
        maintenance_count = maintenance_count
            .checked_add(1)
            .ok_or(UsageStoreError::Integrity)?;
        if singleton != 1
            || day.len() != 10
            || NaiveDate::parse_from_str(&day, "%Y-%m-%d").is_err()
            || store_generation.is_some_and(|generation| generation < 0)
        {
            return Err(UsageStoreError::Integrity);
        }
    }
    if maintenance_count > 1 {
        return Err(UsageStoreError::Integrity);
    }
    Ok(())
}

fn stored_row_v1_is_valid(row: &StoredRowV1, normalize_legacy_blame: bool) -> bool {
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
    ) && blame_value_class_is_valid(
        &row.operation,
        &row.outcome,
        &row.value_class,
        &row.pro_outcome,
        normalize_legacy_blame,
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
        && blame_value_class_is_valid(
            &row.operation,
            &row.outcome,
            &row.value_class,
            &row.pro_outcome,
            false,
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

fn blame_value_class_is_valid(
    operation: &str,
    outcome: &str,
    value_class: &str,
    pro_outcome: &str,
    normalize_legacy_blame: bool,
) -> bool {
    normalize_legacy_blame
        || operation != "blame"
        || outcome == "failure"
        || ((!matches!(pro_outcome, "produced" | "possible") || value_class == "result_bearing")
            && (value_class != "empty" || pro_outcome == "none"))
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

pub(super) fn checked_count(value: i64) -> Result<u64, UsageStoreError> {
    u64::try_from(value).map_err(|_| UsageStoreError::Integrity)
}

pub(super) fn checked_sum(values: impl IntoIterator<Item = u64>) -> Result<u64, UsageStoreError> {
    values.into_iter().try_fold(0_u64, |total, value| {
        total.checked_add(value).ok_or(UsageStoreError::Integrity)
    })
}

pub(super) fn reconcile_summary(summary: &UsageSummary) -> Result<(), UsageStoreError> {
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
        || context.context_cited != 0
        || context.validated_discoveries > validation_sources
    {
        return Err(UsageStoreError::Integrity);
    }
    Ok(())
}

pub(super) fn reconcile_report(
    summary: &UsageSummary,
    details: &UsageDetails,
) -> Result<(), UsageStoreError> {
    let operation_calls = checked_sum(details.by_operation.iter().map(|row| row.calls))?;
    let duration_calls = checked_sum(details.duration_buckets.iter().map(|row| row.calls))?;
    if operation_calls != summary.calls || duration_calls != summary.calls {
        return Err(UsageStoreError::Integrity);
    }
    for row in &details.by_operation {
        if checked_sum([row.successful_calls, row.failed_calls])? != row.calls
            || checked_sum([
                row.result_bearing_calls,
                row.empty_calls,
                row.not_applicable_calls,
            ])? != row.calls
        {
            return Err(UsageStoreError::Integrity);
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
        return Err(UsageStoreError::Integrity);
    }
    let blame = &summary.pro_blame;
    if checked_sum([
        blame.produced_attribution_requests,
        blame.possible_or_reference_only_requests,
        blame.no_confident_attribution_requests,
        blame.error_requests,
    ])? != blame.requests
    {
        return Err(UsageStoreError::Integrity);
    }
    let targeted = checked_sum(blame.by_target.iter().map(|target| target.requests))?;
    if checked_sum([targeted, blame.unclassified_target_errors])? != blame.requests {
        return Err(UsageStoreError::Integrity);
    }
    for target in &blame.by_target {
        if checked_sum([
            target.produced,
            target.possible_or_reference_only,
            target.none,
            target.error,
        ])? != target.requests
        {
            return Err(UsageStoreError::Integrity);
        }
    }
    Ok(())
}
