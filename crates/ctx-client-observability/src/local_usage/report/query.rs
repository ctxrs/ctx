use rusqlite::{Connection, Transaction, TransactionBehavior};

use super::super::{EstimateFacts, UsageStoreError, DEFINITION_VERSION};
use super::validation::{checked_count, reconcile_definition};
use super::{DurationSummary, OperationSummary, UsageDefinition, UsageSummary};

pub(super) fn query_report(
    conn: &mut Connection,
    detailed: bool,
) -> Result<(Vec<UsageDefinition>, EstimateFacts), UsageStoreError> {
    let transaction = conn.transaction_with_behavior(TransactionBehavior::Deferred)?;
    super::super::store::verify_report_dates(&transaction, std::time::SystemTime::now())?;
    let versions = {
        let mut statement = transaction.prepare(
            "SELECT DISTINCT definition_version FROM daily_usage ORDER BY definition_version",
        )?;
        let values = statement
            .query_map([], |row| row.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        values
    };
    let mut definitions = Vec::with_capacity(versions.len());
    let mut estimate_facts = EstimateFacts::default();
    for definition_version in versions {
        let definition = query_definition(&transaction, definition_version, detailed)?;
        // Definition 3 adds Blame without changing the Search context facts
        // used by this estimate. Preserve value already measured under v2 and
        // combine it with compatible v3 Search facts.
        if matches!(definition_version, 2 | DEFINITION_VERSION) {
            estimate_facts = EstimateFacts {
                complete_calls: estimate_facts
                    .complete_calls
                    .checked_add(definition.summary.complete_context_eligible_calls)
                    .ok_or(UsageStoreError::Integrity)?,
                unavailable_calls: estimate_facts
                    .unavailable_calls
                    .checked_add(definition.summary.unavailable_context_eligible_calls)
                    .ok_or(UsageStoreError::Integrity)?,
                delivered_context_bytes: estimate_facts
                    .delivered_context_bytes
                    .checked_add(definition.summary.delivered_context_bytes)
                    .ok_or(UsageStoreError::Integrity)?,
                matched_normalized_session_bytes: estimate_facts
                    .matched_normalized_session_bytes
                    .checked_add(definition.summary.matched_normalized_session_bytes)
                    .ok_or(UsageStoreError::Integrity)?,
            };
        }
        definitions.push(definition);
    }
    transaction.commit()?;
    Ok((definitions, estimate_facts))
}

fn query_definition(
    conn: &Transaction<'_>,
    definition_version: i64,
    detailed: bool,
) -> Result<UsageDefinition, UsageStoreError> {
    let (first_day_utc, last_day_utc, active_days) = conn.query_row(
        r#"
        SELECT MIN(day_utc), MAX(day_utc), COUNT(DISTINCT day_utc)
        FROM daily_usage
        WHERE definition_version = ?1
        "#,
        [definition_version],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        },
    )?;
    let ctx_versions = {
        let mut statement = conn.prepare(
            r#"
            SELECT DISTINCT ctx_version
            FROM daily_usage
            WHERE definition_version = ?1
            ORDER BY ctx_version
            "#,
        )?;
        let values = statement
            .query_map([definition_version], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        values
    };
    let raw = conn.query_row(
        r#"
        SELECT
            COALESCE(SUM(calls), 0),
            COALESCE(SUM(CASE WHEN outcome = 'success' THEN calls ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN outcome = 'failure' THEN calls ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN value_class = 'result_bearing' THEN calls ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN value_class = 'empty' THEN calls ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN value_class = 'not_applicable' THEN calls ELSE 0 END), 0),
            COALESCE(SUM(result_count), 0),
            COALESCE(SUM(delivered_output_bytes), 0),
            COALESCE(SUM(delivered_context_bytes), 0),
            COALESCE(SUM(matched_normalized_session_bytes), 0),
            COALESCE(SUM(CASE WHEN context_coverage = 'complete' THEN calls ELSE 0 END), 0),
            COALESCE(SUM(CASE WHEN context_coverage = 'unavailable' THEN calls ELSE 0 END), 0)
        FROM daily_usage
        WHERE definition_version = ?1
        "#,
        [definition_version],
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
                row.get::<_, i64>(9)?,
                row.get::<_, i64>(10)?,
                row.get::<_, i64>(11)?,
            ))
        },
    )?;
    let summary = UsageSummary {
        calls: checked_count(raw.0)?,
        successful_calls: checked_count(raw.1)?,
        failed_calls: checked_count(raw.2)?,
        result_bearing_calls: checked_count(raw.3)?,
        empty_calls: checked_count(raw.4)?,
        not_applicable_calls: checked_count(raw.5)?,
        result_count: checked_count(raw.6)?,
        delivered_output_bytes: checked_count(raw.7)?,
        delivered_context_bytes: checked_count(raw.8)?,
        matched_normalized_session_bytes: checked_count(raw.9)?,
        complete_context_eligible_calls: checked_count(raw.10)?,
        unavailable_context_eligible_calls: checked_count(raw.11)?,
    };
    let (by_operation, duration_buckets) = if detailed {
        (
            query_operations(conn, definition_version)?,
            query_durations(conn, definition_version)?,
        )
    } else {
        (Vec::new(), Vec::new())
    };
    let definition = UsageDefinition {
        definition_version,
        ctx_versions,
        first_day_utc,
        last_day_utc,
        active_days: checked_count(active_days)?,
        summary,
        by_operation,
        duration_buckets,
    };
    reconcile_definition(&definition, detailed)?;
    Ok(definition)
}

fn query_operations(
    conn: &Transaction<'_>,
    definition_version: i64,
) -> Result<Vec<OperationSummary>, UsageStoreError> {
    let mut statement = conn.prepare(
        r#"
        SELECT
            ctx_version, surface, operation,
            SUM(calls),
            SUM(CASE WHEN outcome = 'success' THEN calls ELSE 0 END),
            SUM(CASE WHEN outcome = 'failure' THEN calls ELSE 0 END),
            SUM(CASE WHEN value_class = 'result_bearing' THEN calls ELSE 0 END),
            SUM(CASE WHEN value_class = 'empty' THEN calls ELSE 0 END),
            SUM(CASE WHEN value_class = 'not_applicable' THEN calls ELSE 0 END),
            SUM(result_count), SUM(delivered_output_bytes),
            SUM(delivered_context_bytes), SUM(matched_normalized_session_bytes),
            SUM(CASE WHEN context_coverage = 'complete' THEN calls ELSE 0 END),
            SUM(CASE WHEN context_coverage = 'unavailable' THEN calls ELSE 0 END)
        FROM daily_usage
        WHERE definition_version = ?1
        GROUP BY ctx_version, surface, operation
        ORDER BY ctx_version, surface, operation
        "#,
    )?;
    let rows = statement.query_map([definition_version], |row| {
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
            result_count: row.get(9)?,
            delivered_output_bytes: row.get(10)?,
            delivered_context_bytes: row.get(11)?,
            matched_normalized_session_bytes: row.get(12)?,
            complete_context_eligible_calls: row.get(13)?,
            unavailable_context_eligible_calls: row.get(14)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(UsageStoreError::from)
}

fn query_durations(
    conn: &Transaction<'_>,
    definition_version: i64,
) -> Result<Vec<DurationSummary>, UsageStoreError> {
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
    let rows = statement.query_map([definition_version], |row| {
        Ok(DurationSummary {
            duration_bucket: row.get(0)?,
            calls: row.get(1)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(UsageStoreError::from)
}
