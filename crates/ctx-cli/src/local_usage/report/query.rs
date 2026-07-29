use std::time::SystemTime;

use rusqlite::{Connection, TransactionBehavior};

use super::super::{
    store::verify_report_dates, EstimateFacts, UsageStoreError, DEFINITION_VERSION,
};
use super::{
    validation::{checked_count, checked_sum, reconcile_report, reconcile_summary, validate_rows},
    ContextProxySummary, DurationSummary, OperationSummary, ProBlameSummary, ProBlameTargetSummary,
    ResultActionSummary, UsageDetails, UsageSummary, CONTEXT_CITED_COVERAGE_UNSUPPORTED,
};

pub(super) fn query_report(
    conn: &mut Connection,
    detailed: bool,
) -> Result<(UsageSummary, Option<UsageDetails>), UsageStoreError> {
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
            context_cited_coverage: CONTEXT_CITED_COVERAGE_UNSUPPORTED,
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

fn query_pro_blame(conn: &Connection) -> Result<ProBlameSummary, UsageStoreError> {
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

fn query_result_actions(conn: &Connection) -> Result<ResultActionSummary, UsageStoreError> {
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

pub(super) fn estimate_facts(summary: &UsageSummary) -> Result<EstimateFacts, UsageStoreError> {
    let actions = &summary.result_actions;
    let semantic_context_eligible_samples = checked_sum([
        actions.searches,
        actions.sessions_opened,
        actions.events_opened,
        actions.locate_requests,
        actions.sources_requests,
        actions.sql_requests,
        actions.blame_requests,
    ])?;
    Ok(EstimateFacts {
        result_bearing_searches: actions.result_bearing_searches,
        semantic_context_eligible_samples,
        semantic_context_bytes: summary.semantic_context_bytes,
        semantic_context_byte_samples: summary.semantic_context_byte_samples,
        semantic_search_result_bytes: summary.semantic_search_result_bytes,
        semantic_search_result_byte_samples: summary.semantic_search_result_byte_samples,
        discovered_record_opens: summary.context.context_opened,
        produced_blame_requests: summary.pro_blame.produced_attribution_requests,
        possible_blame_requests: summary.pro_blame.possible_or_reference_only_requests,
    })
}

fn query_details(conn: &Connection) -> Result<UsageDetails, UsageStoreError> {
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
