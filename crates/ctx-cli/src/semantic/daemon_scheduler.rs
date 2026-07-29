pub(super) fn run_daemon_once_with_activity(
    _args: &DaemonRunArgs,
    data_root: &Path,
    runtime: &mut DaemonRuntime,
    deadline: Option<Instant>,
    _semantic_enabled: bool,
    query_activity: Option<&DaemonQueryActivity>,
    source_refresh: Option<&SourceBackedRefreshCoordinator>,
) -> Result<DaemonIteration> {
    let source_refresh_requested =
        source_refresh.is_some_and(SourceBackedRefreshCoordinator::has_pending_request);
    if runtime.config.daemon.mode.runs_only_source_refresh() {
        return Ok(
            run_pending_source_backed_refresh(data_root, source_refresh)?.unwrap_or_else(|| {
                DaemonIteration::new(false, false, DaemonCycleStateV1::unknown())
            }),
        );
    }
    let query_generation = query_activity.map(|activity| activity.snapshot().1);
    if !source_refresh_requested
        && daemon_foreground_query_preempts(query_activity, query_generation)
    {
        return Ok(DaemonIteration::new(
            false,
            false,
            DaemonCycleStateV1::unknown(),
        ));
    }
    if source_refresh_requested {
        return run_full_source_backed_refresh(data_root, runtime, source_refresh, false);
    }
    if !runtime.history_retry.ready() {
        let job = source_backed_refresh_retry_backoff_job(data_root, &runtime.history_retry);
        write_daemon_job_status(&daemon_source_backed_refresh_job_path(data_root), &job)?;
        let state = daemon_source_backed_cycle_state(&job);
        return Ok(DaemonIteration::new(false, false, state));
    }
    if !daemon_deadline_has_min_budget(deadline, DAEMON_MIN_REMAINING_FOR_JOB_SECS) {
        return Ok(DaemonIteration::new(
            false,
            false,
            DaemonCycleStateV1::unknown(),
        ));
    }
    run_full_source_backed_refresh(data_root, runtime, source_refresh, true)
}

fn run_pending_source_backed_refresh(
    data_root: &Path,
    source_refresh: Option<&SourceBackedRefreshCoordinator>,
) -> Result<Option<DaemonIteration>> {
    let Some(run) = source_refresh.and_then(|coordinator| coordinator.run_next(data_root)) else {
        return Ok(None);
    };
    debug_assert_eq!(
        run.failed,
        run.job.get("status").and_then(Value::as_str) == Some("failed")
    );
    write_daemon_job_status(&daemon_source_backed_refresh_job_path(data_root), &run.job)?;
    Ok(Some(DaemonIteration::new(
        run.did_work,
        run.failed,
        DaemonCycleStateV1::unknown(),
    )))
}

fn run_full_source_backed_refresh(
    data_root: &Path,
    runtime: &mut DaemonRuntime,
    source_refresh: Option<&SourceBackedRefreshCoordinator>,
    enqueue_periodic: bool,
) -> Result<DaemonIteration> {
    let run = match source_refresh {
        Some(coordinator) => {
            if enqueue_periodic {
                if let Err(error) = coordinator.enqueue_periodic(data_root) {
                    return finish_full_source_backed_refresh(
                        data_root,
                        runtime,
                        source_backed_refresh_failed_job(
                            data_root,
                            format!("schedule periodic source-backed refresh: {error:#}"),
                        ),
                        false,
                    );
                }
            }
            coordinator.run_next(data_root)
        }
        None => None,
    };
    let Some(run) = run else {
        return finish_full_source_backed_refresh(
            data_root,
            runtime,
            source_backed_refresh_failed_job(
                data_root,
                "daemon source-backed refresh coordinator is unavailable".to_owned(),
            ),
            false,
        );
    };
    let mut job = run.job;
    let mut did_work = run.did_work;
    if !run.failed && daemon_mode_runs_source_backed_relational_catch_up(runtime.config.daemon.mode)
    {
        if let Some(core_generation_id) = job
            .get("published_generation")
            .and_then(Value::as_str)
            .map(str::to_owned)
        {
            match run_relational_after_core_publication(data_root, &core_generation_id) {
                Ok(relational_run) => {
                    did_work |= relational_run.did_work;
                    job["relational_projection"] = relational_run.status;
                }
                Err(error) => {
                    job["relational_projection"] = compact_json(json!({
                        "schema_version": 1,
                        "owner": "daemon",
                        "kind": "source_backed_relational_catch_up",
                        "status": "error",
                        "pending": true,
                        "retryable": true,
                        "core_generation_id": core_generation_id,
                        "active_core_generation_id": null,
                        "receipt_core_generation_id": null,
                        "projection_status": null,
                        "error_code": "source_relational_status_unavailable",
                        "last_error": format!("{error:#}"),
                    }));
                }
            }
        }
    }
    if !run.failed && daemon_mode_runs_source_backed_pro_catch_up(runtime.config.daemon.mode) {
        if let Some(core_generation_id) = job
            .get("published_generation")
            .and_then(Value::as_str)
            .map(str::to_owned)
        {
            let authority = source_refresh
                .and_then(SourceBackedRefreshCoordinator::retained_published_generation);
            match run_after_core_publication(data_root, &core_generation_id, authority.as_deref()) {
                Ok(pro_run) => {
                    did_work |= pro_run.did_work;
                    job["pro_projection"] = pro_run.status;
                }
                Err(error) => {
                    job["pro_projection"] = compact_json(json!({
                        "schema_version": 1,
                        "owner": "daemon",
                        "kind": "source_backed_pro_catch_up",
                        "status": "error",
                        "pending": true,
                        "retryable": true,
                        "core_generation_id": core_generation_id,
                        "receipt_core_generation_id": null,
                        "error_code": "source_pro_status_unavailable",
                        "last_error": format!("{error:#}"),
                    }));
                }
            }
        }
    }
    finish_full_source_backed_refresh(data_root, runtime, job, did_work)
}

fn daemon_mode_runs_source_backed_pro_catch_up(mode: crate::config::DaemonMode) -> bool {
    !mode.runs_only_source_refresh()
}

fn daemon_mode_runs_source_backed_relational_catch_up(mode: crate::config::DaemonMode) -> bool {
    !mode.runs_only_source_refresh()
}

fn finish_full_source_backed_refresh(
    data_root: &Path,
    runtime: &mut DaemonRuntime,
    job: Value,
    did_work: bool,
) -> Result<DaemonIteration> {
    let job = record_daemon_job_retry(&mut runtime.history_retry, job);
    write_daemon_job_status(&daemon_source_backed_refresh_job_path(data_root), &job)?;
    let failed = daemon_job_failed(&job);
    let state = daemon_source_backed_cycle_state(&job);
    Ok(DaemonIteration::new(did_work, failed, state))
}

fn daemon_source_backed_cycle_state(job: &Value) -> DaemonCycleStateV1 {
    let history_backoff = daemon_job_in_retry_backoff(job);
    DaemonCycleStateV1::new(
        daemon_source_backed_freshness(job, history_backoff),
        DaemonBacklogV1::Unknown,
        DaemonCoverageV1::Unknown,
        if history_backoff {
            DaemonBackoffV1::History
        } else {
            DaemonBackoffV1::None
        },
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

fn daemon_source_backed_freshness(job: &Value, backoff: bool) -> DaemonHistoryFreshnessV1 {
    if daemon_job_failed(job) {
        return DaemonHistoryFreshnessV1::Failed;
    }
    if backoff {
        return DaemonHistoryFreshnessV1::Backoff;
    }
    if job.get("status").and_then(Value::as_str) == Some("completed") {
        return DaemonHistoryFreshnessV1::Current;
    }
    DaemonHistoryFreshnessV1::Unknown
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

pub(super) fn restore_daemon_source_refresh_retry(runtime: &mut DaemonRuntime, data_root: &Path) {
    let status = read_daemon_job_status(&daemon_source_backed_refresh_job_path(data_root));
    runtime.history_retry.restore(status.as_ref());
}

pub(super) fn source_refresh_retry_due(runtime: &DaemonRuntime) -> bool {
    runtime.history_retry.consecutive_failures > 0 && runtime.history_retry.ready()
}

fn source_backed_refresh_failed_job(data_root: &Path, message: String) -> Value {
    source_backed_scheduler_job(data_root, "failed", None, Some(message))
}

fn source_backed_refresh_retry_backoff_job(
    data_root: &Path,
    backoff: &DaemonRetryBackoff,
) -> Value {
    let mut job = source_backed_scheduler_job(
        data_root,
        "skipped",
        Some("retry_backoff"),
        read_daemon_job_status(&daemon_source_backed_refresh_job_path(data_root)).and_then(|job| {
            job.get("last_error")
                .and_then(Value::as_str)
                .map(str::to_owned)
        }),
    );
    preserve_daemon_retry_state(&mut job, backoff);
    job
}

fn source_backed_scheduler_job(
    data_root: &Path,
    status: &str,
    reason: Option<&str>,
    last_error: Option<String>,
) -> Value {
    let previous = read_daemon_job_status(&daemon_source_backed_refresh_job_path(data_root));
    compact_json(json!({
        "mode": "background",
        "owner": "daemon",
        "kind": "source_backed",
        "status": status,
        "reason": reason,
        "last_run_at_ms": utc_now().timestamp_millis(),
        "previous_generation": previous
            .as_ref()
            .and_then(|job| job.get("previous_generation"))
            .cloned(),
        "published_generation": previous
            .as_ref()
            .and_then(|job| job.get("published_generation"))
            .cloned(),
        "daemon_mode": "full",
        "trigger": "periodic",
        "trigger_provenance": "daemon_scheduler",
        "last_error": last_error,
    }))
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

pub(super) fn semantic_report_should_queue_recent_work(report: &SemanticWorkerReport) -> bool {
    report.searchable_items > 0
        && report.embedded_items >= report.searchable_items
        && report.dirty_items == 0
}

pub(super) fn refresh_semantic_document_count_cache(store: &Store) -> Result<()> {
    store.refresh_event_embedding_document_count_cache()?;
    Ok(())
}

pub(super) fn daemon_run_start_mode(args: &DaemonRunArgs) -> DaemonStartModeArg {
    args.start_mode.unwrap_or(DaemonStartModeArg::Manual)
}

pub(super) fn daemon_job_failed(value: &Value) -> bool {
    value.get("status").and_then(Value::as_str) == Some("failed")
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

use anyhow::Result;
use ctx_history_core::utc_now;
use ctx_history_store::Store;
use serde_json::{json, Value};

use crate::{
    analytics::{
        DaemonBacklogV1, DaemonBackoffV1, DaemonCoverageV1, DaemonCycleStateV1,
        DaemonHistoryFreshnessV1,
    },
    compact_json, DaemonRunArgs, DaemonStartModeArg,
};

use super::{
    daemon::{DaemonIteration, DaemonRuntime},
    daemon_retry::{semantic_failure_class_from_job, DaemonRetryBackoff},
    paths_status::{
        daemon_source_backed_refresh_job_path, read_daemon_job_status, write_daemon_job_status,
    },
    query_service::DaemonQueryActivity,
    reports::SemanticWorkerReport,
    runtime_limits::DAEMON_MIN_REMAINING_FOR_JOB_SECS,
    source_backed_pro_catch_up::run_after_core_publication,
    source_backed_refresh_coordinator::SourceBackedRefreshCoordinator,
    source_backed_relational_catch_up::run_after_core_publication as run_relational_after_core_publication,
};

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::{
        config::{AppConfig, DaemonMode},
        output::JsonOutputFormat,
        DaemonRunArgs,
    };

    use super::{
        daemon_job_should_backoff, daemon_mode_runs_source_backed_pro_catch_up,
        daemon_mode_runs_source_backed_relational_catch_up, record_daemon_job_retry,
        run_daemon_once_with_activity, DaemonRetryBackoff, DaemonRuntime,
    };

    #[test]
    fn source_refresh_only_mode_excludes_source_backed_pro_catch_up() {
        assert!(daemon_mode_runs_source_backed_pro_catch_up(
            DaemonMode::Full
        ));
        assert!(!daemon_mode_runs_source_backed_pro_catch_up(
            DaemonMode::SourceRefreshOnly
        ));
    }

    #[test]
    fn source_refresh_only_mode_excludes_source_backed_relational_catch_up() {
        assert!(daemon_mode_runs_source_backed_relational_catch_up(
            DaemonMode::Full
        ));
        assert!(!daemon_mode_runs_source_backed_relational_catch_up(
            DaemonMode::SourceRefreshOnly
        ));
    }

    #[test]
    fn source_refresh_only_tick_creates_no_consumer_catch_up_status() {
        let temp = tempfile::tempdir().unwrap();
        let mut config = AppConfig::default();
        config.daemon.mode = DaemonMode::SourceRefreshOnly;
        let mut runtime = DaemonRuntime {
            config,
            ..DaemonRuntime::default()
        };
        let args = DaemonRunArgs {
            foreground: false,
            once: true,
            idle_exit_seconds: None,
            loop_interval_seconds: None,
            max_chunks: None,
            max_seconds: None,
            force: false,
            start_mode: None,
            trigger_command: None,
            format: JsonOutputFormat::Json,
        };

        let iteration = run_daemon_once_with_activity(
            &args,
            temp.path(),
            &mut runtime,
            None,
            false,
            None,
            None,
        )
        .unwrap();

        assert!(!iteration.did_work);
        assert!(!iteration.failed);
        assert!(!temp
            .path()
            .join("daemon/jobs/source-backed-pro-catch-up.json")
            .exists());
        assert!(!temp
            .path()
            .join("daemon/jobs/source-backed-relational-catch-up.json")
            .exists());
    }

    #[test]
    fn pro_projection_error_never_puts_core_refresh_into_backoff() {
        let core_job = json!({
            "status": "completed",
            "published_generation": "a".repeat(64),
            "pro_projection": {
                "status": "error",
                "pending": true,
                "retryable": true,
                "error_code": "pro_not_installed",
            },
        });
        let mut backoff = DaemonRetryBackoff::default();

        assert!(!daemon_job_should_backoff(&core_job));
        let recorded = record_daemon_job_retry(&mut backoff, core_job);

        assert_eq!(recorded["status"], "completed");
        assert_eq!(recorded["pro_projection"]["status"], "error");
        assert_eq!(backoff.consecutive_failures, 0);
    }

    #[test]
    fn relational_projection_error_never_puts_core_refresh_into_backoff() {
        let core_job = json!({
            "status": "completed",
            "published_generation": "a".repeat(64),
            "relational_projection": {
                "status": "error",
                "pending": true,
                "retryable": true,
                "error_code": "source_relational_projection_unavailable",
            },
        });
        let mut backoff = DaemonRetryBackoff::default();

        assert!(!daemon_job_should_backoff(&core_job));
        let recorded = record_daemon_job_retry(&mut backoff, core_job);

        assert_eq!(recorded["status"], "completed");
        assert_eq!(recorded["relational_projection"]["status"], "error");
        assert_eq!(backoff.consecutive_failures, 0);
    }
}
