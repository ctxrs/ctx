use std::{
    path::Path,
    time::{Duration, SystemTime},
};

use rusqlite::{params, OptionalExtension, TransactionBehavior};

use super::super::{CompletedOperation, DEFINITION_VERSION};
use super::{
    open_writable, preflight_existing_family, protect_sqlite_files, reject_future_daily_dates,
    retention_cutoff, usage_path, utc_day, verify_schema, CorrelationCommit, StoreGeneration,
    UsageStoreError, WritableStore,
};

#[allow(clippy::too_many_arguments)]
pub(super) fn record_correlated_at_with_hook<T>(
    data_root: &Path,
    expected_generation: Option<StoreGeneration>,
    matching_operation: CompletedOperation,
    stale_operation: CompletedOperation,
    now: SystemTime,
    busy_timeout: Duration,
    ctx_version: &str,
    after_generation_check: impl FnOnce() -> T,
) -> Result<CorrelationCommit, UsageStoreError> {
    let path = usage_path(data_root);
    let WritableStore {
        mut conn,
        family_guard,
    } = open_writable(&path, true, busy_timeout)?.ok_or(UsageStoreError::SchemaIdentity)?;
    let day = utc_day(now);
    let cutoff = retention_cutoff(now);
    let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    verify_schema(&transaction)?;
    super::super::report::validate_rows(&transaction)?;
    reject_future_daily_dates(&transaction, &day)?;
    let maintenance = transaction
        .query_row(
            "SELECT last_retention_day, store_generation \
             FROM maintenance WHERE singleton = 1",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?;
    let generation = StoreGeneration(
        maintenance
            .as_ref()
            .map_or(0, |(_, store_generation)| *store_generation),
    );
    let expected_generation_matched =
        expected_generation.is_none_or(|expected| expected == generation);
    let generation_check_guard = after_generation_check();
    // Both candidates contain aggregates only. Select one while the same
    // IMMEDIATE transaction excludes a reset until this upsert commits.
    let operation = if expected_generation_matched {
        matching_operation
    } else {
        stale_operation
    };
    if maintenance.is_none() {
        transaction.execute(
            "INSERT INTO maintenance \
             (singleton, last_retention_day, store_generation) VALUES (1, ?1, 0)",
            [day.as_str()],
        )?;
        transaction.execute("DELETE FROM daily_usage WHERE day_utc < ?1", [cutoff])?;
    } else if maintenance
        .as_ref()
        .is_some_and(|(last_retention_day, _)| last_retention_day != &day)
    {
        transaction.execute(
            "UPDATE maintenance SET last_retention_day = ?1 WHERE singleton = 1",
            [day.as_str()],
        )?;
        transaction.execute("DELETE FROM daily_usage WHERE day_utc < ?1", [cutoff])?;
    }
    transaction.execute(
        r#"
        INSERT INTO daily_usage (
            day_utc, definition_version, ctx_version, surface, operation, outcome,
            value_class, duration_bucket, target_type, pro_outcome, result_action,
            calls, result_count, citation_count, latency_ms, latency_samples,
            response_bytes, response_byte_samples, output_bytes, output_byte_samples,
            context_bytes, context_byte_samples, search_result_bytes,
            search_result_byte_samples, context_searches, context_found,
            context_opened, context_cited, validated_discoveries
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
            1, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20,
            ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28
        )
        ON CONFLICT (
            day_utc, definition_version, ctx_version, surface, operation, outcome,
            value_class, duration_bucket, target_type, pro_outcome, result_action
        ) DO UPDATE SET
            calls = calls + 1,
            result_count = result_count + excluded.result_count,
            citation_count = citation_count + excluded.citation_count,
            latency_ms = latency_ms + excluded.latency_ms,
            latency_samples = latency_samples + excluded.latency_samples,
            response_bytes = response_bytes + excluded.response_bytes,
            response_byte_samples = response_byte_samples + excluded.response_byte_samples,
            output_bytes = output_bytes + excluded.output_bytes,
            output_byte_samples = output_byte_samples + excluded.output_byte_samples,
            context_bytes = context_bytes + excluded.context_bytes,
            context_byte_samples = context_byte_samples + excluded.context_byte_samples,
            search_result_bytes = search_result_bytes + excluded.search_result_bytes,
            search_result_byte_samples =
                search_result_byte_samples + excluded.search_result_byte_samples,
            context_searches = context_searches + excluded.context_searches,
            context_found = context_found + excluded.context_found,
            context_opened = context_opened + excluded.context_opened,
            context_cited = context_cited + excluded.context_cited,
            validated_discoveries =
                validated_discoveries + excluded.validated_discoveries
        "#,
        params![
            day,
            DEFINITION_VERSION,
            ctx_version,
            operation.surface.as_str(),
            operation.operation,
            operation.outcome.as_str(),
            operation.value_class.as_str(),
            operation.duration.as_str(),
            operation.target_type.as_str(),
            operation.pro_outcome.as_str(),
            operation.result_action.map_or(
                "not_applicable",
                super::super::ResultObservationAction::as_str
            ),
            operation.result_count,
            operation.citation_count,
            operation.latency_ms,
            operation.latency_samples,
            operation.response_bytes,
            operation.response_byte_samples,
            operation.output_bytes,
            operation.output_byte_samples,
            operation.context_bytes,
            operation.context_byte_samples,
            operation.search_result_bytes,
            operation.search_result_byte_samples,
            operation.context.context_searches,
            operation.context.context_found,
            operation.context.context_opened,
            operation.context.context_cited,
            operation.context.validated_discoveries,
        ],
    )?;
    family_guard.recheck(&path)?;
    let commit_guard = preflight_existing_family(&path, true)?;
    verify_schema(&transaction)?;
    super::super::report::validate_rows(&transaction)?;
    transaction.commit()?;
    drop(commit_guard);
    drop(generation_check_guard);
    let _ = protect_sqlite_files(&path);
    Ok(CorrelationCommit {
        generation,
        expected_generation_matched,
    })
}
