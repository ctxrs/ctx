use std::{
    path::Path,
    time::{Duration, SystemTime},
};

use rusqlite::{params, OptionalExtension, TransactionBehavior};

use super::super::CompletedOperation;
use super::{
    open_writable, preflight_existing_family, protect_sqlite_files, reject_future_daily_dates,
    retention_cutoff, utc_day, verify_schema, UsageStoreError, WritableStore,
};

pub(super) fn record_at(
    database_path: &Path,
    operation: CompletedOperation,
    now: SystemTime,
    busy_timeout: Duration,
    ctx_version: &str,
) -> Result<(), UsageStoreError> {
    let path = database_path.to_path_buf();
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
    let last_retention_day = transaction
        .query_row(
            "SELECT last_retention_day FROM maintenance WHERE singleton = 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if last_retention_day.as_deref() != Some(day.as_str()) {
        transaction.execute(
            r#"
            INSERT INTO maintenance (singleton, last_retention_day)
            VALUES (1, ?1)
            ON CONFLICT (singleton) DO UPDATE SET
                last_retention_day = excluded.last_retention_day
            "#,
            [day.as_str()],
        )?;
        transaction.execute("DELETE FROM daily_usage WHERE day_utc < ?1", [cutoff])?;
    }
    transaction.execute(
        r#"
        INSERT INTO daily_usage (
            day_utc, definition_version, ctx_version, surface, operation, outcome,
            value_class, duration_bucket, context_coverage,
            calls, result_count, delivered_output_bytes,
            delivered_context_bytes, matched_normalized_session_bytes
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
            1, ?10, ?11, ?12, ?13
        )
        ON CONFLICT (
            day_utc, definition_version, ctx_version, surface, operation, outcome,
            value_class, duration_bucket, context_coverage
        ) DO UPDATE SET
            calls = calls + 1,
            result_count = result_count + excluded.result_count,
            delivered_output_bytes =
                delivered_output_bytes + excluded.delivered_output_bytes,
            delivered_context_bytes =
                delivered_context_bytes + excluded.delivered_context_bytes,
            matched_normalized_session_bytes =
                matched_normalized_session_bytes
                + excluded.matched_normalized_session_bytes
        "#,
        params![
            day,
            operation.definition_version,
            ctx_version,
            operation.surface.as_str(),
            operation.operation.as_str(),
            operation.outcome.as_str(),
            operation.value_class.as_str(),
            operation.duration.as_str(),
            operation.context_coverage.as_str(),
            operation.result_count,
            operation.delivered_output_bytes,
            operation.delivered_context_bytes,
            operation.matched_normalized_session_bytes,
        ],
    )?;
    family_guard.recheck(&path)?;
    let commit_guard = preflight_existing_family(&path, true)?;
    verify_schema(&transaction)?;
    super::super::report::validate_rows(&transaction)?;
    transaction.commit()?;
    drop(commit_guard);
    let _ = protect_sqlite_files(&path);
    Ok(())
}
