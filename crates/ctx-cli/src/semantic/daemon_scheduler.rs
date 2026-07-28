#[cfg(test)]
pub(super) fn run_daemon_once(
    args: &DaemonRunArgs,
    data_root: &Path,
    runtime: &mut DaemonRuntime,
    deadline: Option<Instant>,
    semantic_enabled: bool,
) -> Result<DaemonIteration> {
    run_daemon_once_with_activity(
        args,
        data_root,
        runtime,
        deadline,
        semantic_enabled,
        None,
        None,
    )
}

pub(super) fn run_daemon_once_with_activity(
    args: &DaemonRunArgs,
    data_root: &Path,
    runtime: &mut DaemonRuntime,
    deadline: Option<Instant>,
    semantic_enabled: bool,
    query_activity: Option<&DaemonQueryActivity>,
    source_refresh: Option<&SourceBackedRefreshCoordinator>,
) -> Result<DaemonIteration> {
    let source_refresh_requested =
        source_refresh.is_some_and(SourceBackedRefreshCoordinator::has_pending_request);
    let query_generation = query_activity.map(|activity| activity.snapshot().1);
    if !source_refresh_requested
        && daemon_foreground_query_preempts(query_activity, query_generation)
    {
        write_daemon_preempted_jobs(data_root, semantic_enabled, runtime)?;
        return Ok(DaemonIteration::new(
            false,
            false,
            DaemonCycleStateV1::unknown(),
        ));
    }
    if !source_refresh_requested
        && semantic_enabled
        && semantic_bootstrap_should_run_first(data_root, runtime)?
    {
        let mut history_refresh_job =
            daemon_history_refresh_skipped_job("semantic_bootstrap_in_progress");
        preserve_daemon_history_runtime_state(&mut history_refresh_job, runtime);
        write_daemon_job_status(
            &daemon_history_refresh_job_path(data_root),
            &history_refresh_job,
        )?;
        let semantic_job = run_daemon_semantic_job_with_retry(
            args,
            data_root,
            runtime,
            deadline,
            semantic_enabled,
        );
        let semantic_did_work = daemon_semantic_job_did_work(&semantic_job);
        runtime.semantic_bootstrap_passes_since_refresh = runtime
            .semantic_bootstrap_passes_since_refresh
            .saturating_add(1);
        write_daemon_job_status_unless_deadline_skip(
            &daemon_semantic_job_path(data_root),
            &semantic_job,
        )?;
        let (did_work, failed) = (semantic_did_work, daemon_job_failed(&semantic_job));
        let semantic_report = semantic_worker_report_for_daemon(data_root);
        let state = daemon_cycle_state(
            runtime,
            &history_refresh_job,
            &semantic_job,
            &semantic_report,
        );
        return Ok(DaemonIteration::new(did_work, failed, state));
    }
    if source_refresh_requested {
        let Some(run) = source_refresh.and_then(|coordinator| coordinator.run_next(data_root))
        else {
            return Ok(DaemonIteration::new(
                false,
                false,
                DaemonCycleStateV1::unknown(),
            ));
        };
        debug_assert_eq!(
            run.failed,
            run.job.get("status").and_then(Value::as_str) == Some("failed")
        );
        write_daemon_job_status(&daemon_source_backed_refresh_job_path(data_root), &run.job)?;
        return Ok(DaemonIteration::new(
            run.did_work,
            run.failed,
            DaemonCycleStateV1::unknown(),
        ));
    }

    let mut provider_refresh_events = Vec::new();
    let history_refresh_job = if daemon_history_retry_blocks_scheduler(runtime) {
        daemon_history_refresh_retry_backoff_job(&runtime.history_retry)
    } else if daemon_deadline_has_min_budget(deadline, DAEMON_MIN_REMAINING_FOR_JOB_SECS) {
        match run_daemon_history_refresh_job(
            data_root,
            &mut runtime.history_source_cursor,
            &runtime.config,
        ) {
            Ok(result) => {
                provider_refresh_events = result.provider_refresh_events;
                result.job
            }
            Err(error) => daemon_history_refresh_failed_job(format!("{error:#}")),
        }
    } else {
        daemon_history_refresh_skipped_job("daemon_deadline")
    };
    let mut history_refresh_job = record_daemon_history_job_retry(runtime, history_refresh_job);
    let history_refresh_did_work =
        finish_daemon_history_refresh_job(runtime, &mut history_refresh_job);
    runtime.semantic_bootstrap_passes_since_refresh = 0;
    write_daemon_job_status_unless_deadline_skip(
        &daemon_history_refresh_job_path(data_root),
        &history_refresh_job,
    )?;

    let mut semantic_job = if daemon_foreground_query_preempts(query_activity, query_generation) {
        daemon_semantic_skipped_job(data_root, semantic_enabled, "foreground_query")
    } else if daemon_deadline_has_min_budget(deadline, DAEMON_MIN_REMAINING_FOR_JOB_SECS) {
        run_daemon_semantic_job_with_retry(args, data_root, runtime, deadline, semantic_enabled)
    } else {
        daemon_semantic_deadline_skipped_job(data_root)
    };
    preserve_daemon_retry_state(&mut semantic_job, &runtime.semantic_retry);
    let semantic_did_work = daemon_semantic_job_did_work(&semantic_job);
    write_daemon_job_status_unless_deadline_skip(
        &daemon_semantic_job_path(data_root),
        &semantic_job,
    )?;

    let pro_materialization_did_work =
        if !daemon_foreground_query_preempts(query_activity, query_generation)
            && daemon_deadline_has_min_budget(deadline, DAEMON_MIN_REMAINING_FOR_JOB_SECS)
            && daemon_history_freshness(
                runtime,
                &history_refresh_job,
                daemon_job_in_retry_backoff(&history_refresh_job),
            ) == DaemonHistoryFreshnessV1::Current
        {
            crate::pro::run_pending_materialization(data_root).unwrap_or(false)
        } else {
            false
        };

    let (did_work, failed) = (
        history_refresh_did_work || semantic_did_work || pro_materialization_did_work,
        daemon_history_job_failed(&history_refresh_job) || daemon_job_failed(&semantic_job),
    );

    let semantic_report = semantic_worker_report_for_daemon(data_root);
    let state = daemon_cycle_state(
        runtime,
        &history_refresh_job,
        &semantic_job,
        &semantic_report,
    );
    Ok(DaemonIteration::new(did_work, failed, state)
        .with_provider_refresh_events(provider_refresh_events))
}

pub(super) fn daemon_cycle_state(
    runtime: &DaemonRuntime,
    history_job: &Value,
    semantic_job: &Value,
    semantic_report: &SemanticWorkerReport,
) -> DaemonCycleStateV1 {
    let history_backoff = daemon_job_in_retry_backoff(history_job);
    let semantic_backoff = daemon_job_in_retry_backoff(semantic_job);
    let retry_backoff = match (history_backoff, semantic_backoff) {
        (false, false) => DaemonBackoffV1::None,
        (true, false) => DaemonBackoffV1::History,
        (false, true) => DaemonBackoffV1::Semantic,
        (true, true) => DaemonBackoffV1::Both,
    };
    DaemonCycleStateV1::new(
        daemon_history_freshness(runtime, history_job, history_backoff),
        daemon_semantic_backlog(semantic_report),
        daemon_semantic_coverage(semantic_report),
        retry_backoff,
    )
}

fn daemon_job_in_retry_backoff(job: &Value) -> bool {
    job.get("reason").and_then(Value::as_str) == Some("retry_backoff")
        || (job.get("retryable").and_then(Value::as_bool) == Some(true)
            && job
                .get("consecutive_failures")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                > 0)
}

fn daemon_history_freshness(
    runtime: &DaemonRuntime,
    job: &Value,
    backoff: bool,
) -> DaemonHistoryFreshnessV1 {
    if daemon_history_job_failed(job) {
        return DaemonHistoryFreshnessV1::Failed;
    }
    if backoff {
        return DaemonHistoryFreshnessV1::Backoff;
    }
    if job.get("capture_work_remaining").and_then(Value::as_bool) == Some(true)
        || runtime.history_followup_passes_remaining > 0
        || runtime.history_retry_drain_passes_remaining > 0
    {
        return DaemonHistoryFreshnessV1::Pending;
    }
    if job.get("status").and_then(Value::as_str) == Some("completed")
        || job.get("reason").and_then(Value::as_str) == Some("no_sources")
    {
        return DaemonHistoryFreshnessV1::Current;
    }
    DaemonHistoryFreshnessV1::Unknown
}

fn daemon_semantic_backlog(report: &SemanticWorkerReport) -> DaemonBacklogV1 {
    if !report.searchable_items_known {
        return DaemonBacklogV1::Unknown;
    }
    DaemonBacklogV1::Bucket(count_bucket(report.queued_items_estimate as u64))
}

fn daemon_semantic_coverage(report: &SemanticWorkerReport) -> DaemonCoverageV1 {
    if !report.searchable_items_known {
        return DaemonCoverageV1::Unknown;
    }
    if report.searchable_items == 0 {
        DaemonCoverageV1::Empty
    } else if report.dirty_items > 0 {
        DaemonCoverageV1::Dirty
    } else if report.embedded_items >= report.searchable_items {
        DaemonCoverageV1::Complete
    } else {
        DaemonCoverageV1::Incomplete
    }
}

pub(super) fn daemon_foreground_query_preempts(
    activity: Option<&DaemonQueryActivity>,
    observed_generation: Option<u64>,
) -> bool {
    let Some(activity) = activity else {
        return false;
    };
    let (active_requests, generation) = activity.snapshot();
    active_requests > 0 || observed_generation.is_some_and(|observed| generation != observed)
}

pub(super) fn write_daemon_preempted_jobs(
    data_root: &Path,
    semantic_enabled: bool,
    runtime: &DaemonRuntime,
) -> Result<()> {
    let mut history_job = daemon_history_refresh_skipped_job("foreground_query");
    preserve_daemon_history_runtime_state(&mut history_job, runtime);
    write_daemon_job_status(&daemon_history_refresh_job_path(data_root), &history_job)?;
    let mut semantic_job =
        daemon_semantic_skipped_job(data_root, semantic_enabled, "foreground_query");
    preserve_daemon_retry_state(&mut semantic_job, &runtime.semantic_retry);
    write_daemon_job_status(&daemon_semantic_job_path(data_root), &semantic_job)?;
    Ok(())
}

pub(super) fn preserve_daemon_retry_state(job: &mut Value, backoff: &DaemonRetryBackoff) {
    if backoff.ready() {
        return;
    }
    job["retryable"] = Value::Bool(true);
    job["retry_after_ms"] = json!(backoff.retry_after_ms().unwrap_or(0));
    job["consecutive_failures"] = json!(backoff.consecutive_failures);
    job["retry_not_before_at_ms"] = json!(backoff.retry_not_before_at_ms);
}

pub(super) fn run_daemon_semantic_job_with_retry(
    args: &DaemonRunArgs,
    data_root: &Path,
    runtime: &mut DaemonRuntime,
    deadline: Option<Instant>,
    semantic_enabled: bool,
) -> Value {
    if let Some(job) = runtime.semantic_blocked_job.as_ref() {
        return job.clone();
    }
    if !runtime.semantic_retry.ready() {
        return daemon_semantic_retry_backoff_job(data_root, &runtime.semantic_retry);
    }
    let job = run_daemon_semantic_job(args, data_root, runtime, deadline, semantic_enabled)
        .unwrap_or_else(|error| daemon_semantic_failed_job(data_root, error));
    let job = record_daemon_job_retry(&mut runtime.semantic_retry, job);
    if semantic_failure_class_from_job(&job).is_some_and(SemanticFailureClass::blocks_until_restart)
    {
        runtime.semantic_blocked_job = Some(job.clone());
    }
    job
}

pub(super) fn record_daemon_job_retry(backoff: &mut DaemonRetryBackoff, mut job: Value) -> Value {
    if daemon_job_should_backoff(&job) {
        let delay = backoff.record_failure();
        job["retryable"] = Value::Bool(true);
        job["retry_after_ms"] = json!(delay.as_millis() as u64);
        job["consecutive_failures"] = json!(backoff.consecutive_failures);
        job["retry_not_before_at_ms"] = json!(backoff.retry_not_before_at_ms);
    } else if job.get("reason").and_then(Value::as_str) != Some("retry_backoff") {
        backoff.reset();
    }
    job
}

pub(super) fn daemon_job_should_backoff(job: &Value) -> bool {
    if let Some(class) = semantic_failure_class_from_job(job) {
        return class.retries_with_backoff();
    }
    daemon_job_failed(job)
        || (job.get("reason").and_then(Value::as_str) != Some("retry_backoff")
            && (job.get("retryable").and_then(Value::as_bool) == Some(true)
                || (job.get("status").and_then(Value::as_str) == Some("skipped")
                    && job.get("last_error").and_then(Value::as_str).is_some())))
}

pub(super) fn semantic_bootstrap_should_run_first(
    data_root: &Path,
    runtime: &mut DaemonRuntime,
) -> Result<bool> {
    let db_path = database_path(data_root.to_path_buf());
    if !db_path.exists() {
        return Ok(false);
    }
    if runtime.semantic_bootstrap_passes_since_refresh
        >= DAEMON_SEMANTIC_BOOTSTRAP_PASSES_BEFORE_REFRESH
    {
        return Ok(false);
    }
    let store = Store::open(&db_path).context("open ctx store for daemon semantic bootstrap")?;
    refresh_semantic_document_count_cache(&store)?;
    let report = semantic_worker_report(data_root, Some(&store))?;
    Ok(report.searchable_items > 0 && report.queued_items_estimate > 0)
}

pub(super) fn semantic_report_should_queue_recent_work(report: &SemanticWorkerReport) -> bool {
    report.searchable_items > 0
        && report.embedded_items >= report.searchable_items
        && report.dirty_items == 0
}

pub(super) fn refresh_semantic_document_count_cache(store: &Store) -> Result<()> {
    store.refresh_event_embedding_document_count_cache()?;
    Ok(())
}

pub(super) fn daemon_semantic_job_did_work(value: &Value) -> bool {
    value
        .get("indexed_chunks")
        .and_then(Value::as_u64)
        .is_some_and(|chunks| chunks > 0)
}

pub(super) fn daemon_run_start_mode(args: &DaemonRunArgs) -> DaemonStartModeArg {
    args.start_mode.unwrap_or(DaemonStartModeArg::Manual)
}

pub(super) fn daemon_job_failed(value: &Value) -> bool {
    value.get("status").and_then(Value::as_str) == Some("failed")
}

pub(super) fn daemon_history_job_failed(value: &Value) -> bool {
    daemon_job_failed(value)
        || value
            .get("totals")
            .and_then(|totals| totals.get("failed_sources"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
            > 0
}

pub(super) fn write_daemon_job_status_unless_deadline_skip(
    path: &Path,
    value: &Value,
) -> Result<()> {
    if daemon_job_skipped_for_deadline(value) && path.exists() {
        return Ok(());
    }
    write_daemon_job_status(path, value)
}

pub(super) fn daemon_job_skipped_for_deadline(value: &Value) -> bool {
    value.get("status").and_then(Value::as_str) == Some("skipped")
        && value.get("reason").and_then(Value::as_str) == Some("daemon_deadline")
}

pub(super) fn daemon_deadline_remaining(deadline: Option<Instant>) -> Option<StdDuration> {
    deadline.and_then(|deadline| deadline.checked_duration_since(Instant::now()))
}

pub(super) fn daemon_deadline_has_min_budget(deadline: Option<Instant>, min_secs: u64) -> bool {
    let Some(remaining) = daemon_deadline_remaining(deadline) else {
        return deadline.is_none();
    };
    remaining >= StdDuration::from_secs(min_secs)
}
use std::{
    path::Path,
    time::{Duration as StdDuration, Instant},
};

use anyhow::{Context, Result};
use ctx_history_core::database_path;
use ctx_history_store::Store;
use serde_json::{json, Value};

use crate::{
    analytics::{
        count_bucket, DaemonBacklogV1, DaemonBackoffV1, DaemonCoverageV1, DaemonCycleStateV1,
        DaemonHistoryFreshnessV1,
    },
    DaemonRunArgs, DaemonStartModeArg,
};

use super::{
    daemon::{DaemonIteration, DaemonRuntime},
    daemon_history::{
        daemon_history_refresh_failed_job, daemon_history_refresh_retry_backoff_job,
        daemon_history_refresh_skipped_job, daemon_history_retry_blocks_scheduler,
        finish_daemon_history_refresh_job, preserve_daemon_history_runtime_state,
        record_daemon_history_job_retry, run_daemon_history_refresh_job,
    },
    daemon_retry::{semantic_failure_class_from_job, DaemonRetryBackoff, SemanticFailureClass},
    daemon_worker::{
        daemon_semantic_deadline_skipped_job, daemon_semantic_failed_job,
        daemon_semantic_retry_backoff_job, daemon_semantic_skipped_job, run_daemon_semantic_job,
        semantic_worker_report_for_daemon,
    },
    paths_status::{
        daemon_history_refresh_job_path, daemon_semantic_job_path,
        daemon_source_backed_refresh_job_path, semantic_worker_report, write_daemon_job_status,
    },
    query_service::DaemonQueryActivity,
    reports::SemanticWorkerReport,
    runtime_limits::{
        DAEMON_MIN_REMAINING_FOR_JOB_SECS, DAEMON_SEMANTIC_BOOTSTRAP_PASSES_BEFORE_REFRESH,
    },
    source_backed_refresh_coordinator::SourceBackedRefreshCoordinator,
};
