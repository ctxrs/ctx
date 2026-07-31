#[derive(Default)]
pub(super) struct DaemonSidecarDrain {
    pub(super) generation: Option<String>,
    pub(super) pro_attempted_generation: Option<String>,
    pub(super) relational_attempted_generation: Option<String>,
    pub(super) semantic_attempted_generation: Option<String>,
}

fn immediate_follow_up(mut iteration: DaemonIteration) -> DaemonIteration {
    iteration.continue_immediately = true;
    iteration
}

pub(super) fn run_daemon_scheduler_cycle_with_activity(
    args: &DaemonRunArgs,
    data_root: &Path,
    runtime: &mut DaemonRuntime,
    deadline: Option<Instant>,
    semantic_enabled: bool,
    query_activity: Option<&DaemonQueryActivity>,
    source_refresh: Option<&CoreRefreshEngine>,
) -> Result<DaemonIteration> {
    let source_refresh_requested =
        source_refresh.is_some_and(CoreRefreshEngine::has_pending_request);
    if runtime.config.daemon.mode.runs_only_source_refresh() {
        return Ok(
            run_pending_core_refresh(data_root, runtime, source_refresh)?.unwrap_or_else(|| {
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
        return run_core_refresh(data_root, runtime, source_refresh, false);
    }
    if let Some(iteration) = run_pending_core_relational_catch_up(data_root, runtime)? {
        return Ok(iteration);
    }
    if let Some(iteration) = run_pending_core_pro_catch_up(data_root, runtime, source_refresh)? {
        return Ok(iteration);
    }
    if let Some(iteration) =
        run_pending_core_semantic_catch_up(args, data_root, runtime, deadline, semantic_enabled)?
    {
        return Ok(iteration);
    }
    if runtime.sidecar_drain.generation.take().is_some() {
        return Ok(DaemonIteration::new(
            false,
            false,
            DaemonCycleStateV1::unknown(),
        ));
    }
    if !runtime.history_retry.ready() {
        let job = core_refresh_retry_backoff_job(data_root, &runtime.history_retry);
        write_daemon_job_status(&daemon_core_refresh_job_path(data_root), &job)?;
        let state = daemon_core_cycle_state(&job);
        return Ok(DaemonIteration::new(false, false, state));
    }
    if !daemon_deadline_has_min_budget(deadline, DAEMON_MIN_REMAINING_FOR_JOB_SECS) {
        return Ok(DaemonIteration::new(
            false,
            false,
            DaemonCycleStateV1::unknown(),
        ));
    }
    run_core_refresh(data_root, runtime, source_refresh, true)
}

fn run_pending_core_relational_catch_up(
    data_root: &Path,
    runtime: &mut DaemonRuntime,
) -> Result<Option<DaemonIteration>> {
    if !daemon_mode_runs_core_relational_catch_up(runtime.config.daemon.mode) {
        return Ok(None);
    }
    let Some(generation) = pin_published_generation(data_root)? else {
        return Ok(None);
    };
    let generation_id = generation.generation_id();
    if runtime.sidecar_drain.generation.as_deref() == Some(generation_id)
        && runtime
            .sidecar_drain
            .relational_attempted_generation
            .as_deref()
            == Some(generation_id)
    {
        return Ok(None);
    }
    if !relational_generation_needs_catch_up(data_root, generation_id) {
        runtime.relational_retry.reset();
        return Ok(None);
    }
    prepare_relational_retry_for_generation(runtime, data_root, generation_id);
    if !runtime.relational_retry.ready() {
        return Ok(None);
    }
    let run = run_relational_catch_up_with_retry(data_root, runtime, generation_id)?;
    runtime.sidecar_drain.relational_attempted_generation = Some(generation_id.to_owned());
    runtime.sidecar_drain.generation = Some(generation_id.to_owned());
    Ok(Some(immediate_follow_up(DaemonIteration::new(
        run.did_work,
        false,
        DaemonCycleStateV1::unknown(),
    ))))
}

fn run_pending_core_semantic_catch_up(
    args: &DaemonRunArgs,
    data_root: &Path,
    runtime: &mut DaemonRuntime,
    deadline: Option<Instant>,
    semantic_enabled: bool,
) -> Result<Option<DaemonIteration>> {
    if !semantic_enabled
        || !daemon_mode_runs_core_semantic_projection(runtime.config.daemon.mode)
        || runtime.semantic_blocked_job.is_some()
        || !daemon_deadline_has_min_budget(deadline, DAEMON_MIN_REMAINING_FOR_JOB_SECS)
    {
        return Ok(None);
    }
    let Some(generation) = pin_published_generation(data_root)? else {
        return Ok(None);
    };
    let generation_id = generation.generation_id();
    if runtime.sidecar_drain.generation.as_deref() == Some(generation_id)
        && runtime
            .sidecar_drain
            .semantic_attempted_generation
            .as_deref()
            == Some(generation_id)
    {
        return Ok(None);
    }
    if !semantic_generation_needs_catch_up(data_root, generation_id) {
        runtime.semantic_retry.reset();
        return Ok(None);
    }
    prepare_semantic_retry_for_generation(runtime, data_root, generation_id);
    if !runtime.semantic_retry.ready() {
        return Ok(None);
    }
    let job = run_daemon_semantic_job_with_retry(
        args,
        data_root,
        runtime,
        deadline,
        true,
        Some(generation_id),
    );
    let did_work = daemon_semantic_job_did_work(&job);
    write_daemon_job_status_unless_deadline_skip(&daemon_semantic_job_path(data_root), &job)?;
    runtime.sidecar_drain.semantic_attempted_generation = Some(generation_id.to_owned());
    runtime.sidecar_drain.generation = Some(generation_id.to_owned());
    Ok(Some(immediate_follow_up(DaemonIteration::new(
        did_work,
        false,
        DaemonCycleStateV1::unknown(),
    ))))
}

fn run_pending_core_pro_catch_up(
    data_root: &Path,
    runtime: &mut DaemonRuntime,
    source_refresh: Option<&CoreRefreshEngine>,
) -> Result<Option<DaemonIteration>> {
    if !daemon_mode_runs_core_pro_catch_up(runtime.config.daemon.mode) {
        return Ok(None);
    }
    let Some(authority) = source_refresh.and_then(CoreRefreshEngine::pinned_core_publication)
    else {
        return Ok(None);
    };
    let generation = authority.generation_id();
    if runtime.sidecar_drain.generation.as_deref() == Some(generation)
        && runtime.sidecar_drain.pro_attempted_generation.as_deref() == Some(generation)
    {
        return Ok(None);
    }
    prepare_pro_retry_for_generation(runtime, data_root, generation);
    if !runtime.pro_retry.ready() {
        return Ok(None);
    }
    let run =
        run_pro_catch_up_with_retry(data_root, runtime, generation, Some(authority.as_ref()))?;
    runtime.sidecar_drain.pro_attempted_generation = Some(generation.to_owned());
    runtime.sidecar_drain.generation = Some(generation.to_owned());
    Ok(Some(immediate_follow_up(DaemonIteration::new(
        run.did_work,
        false,
        DaemonCycleStateV1::unknown(),
    ))))
}

fn run_pending_core_refresh(
    data_root: &Path,
    runtime: &mut DaemonRuntime,
    source_refresh: Option<&CoreRefreshEngine>,
) -> Result<Option<DaemonIteration>> {
    let Some(run) = source_refresh.and_then(|coordinator| coordinator.run_next(data_root)) else {
        return Ok(None);
    };
    debug_assert_eq!(
        run.failed,
        run.job.get("status").and_then(Value::as_str) == Some("failed")
    );
    let job = record_daemon_job_retry(&mut runtime.history_retry, run.job);
    write_daemon_job_status(&daemon_core_refresh_job_path(data_root), &job)?;
    Ok(Some(DaemonIteration::new(
        run.did_work,
        run.failed,
        DaemonCycleStateV1::unknown(),
    )))
}

fn run_core_refresh(
    data_root: &Path,
    runtime: &mut DaemonRuntime,
    source_refresh: Option<&CoreRefreshEngine>,
    enqueue_periodic: bool,
) -> Result<DaemonIteration> {
    let run = match source_refresh {
        Some(coordinator) => {
            if enqueue_periodic {
                if let Err(error) = coordinator.enqueue_periodic(data_root) {
                    return finish_core_refresh(
                        data_root,
                        runtime,
                        core_refresh_failed_job(
                            data_root,
                            format!("schedule periodic Core refresh: {error:#}"),
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
        return finish_core_refresh(
            data_root,
            runtime,
            core_refresh_failed_job(
                data_root,
                "daemon Core refresh engine is unavailable".to_owned(),
            ),
            false,
        );
    };
    finish_core_refresh(data_root, runtime, run.job, run.did_work)
}

fn run_relational_catch_up_with_retry(
    data_root: &Path,
    runtime: &mut DaemonRuntime,
    core_generation_id: &str,
) -> Result<SourceBackedRelationalCatchUpRun> {
    prepare_relational_retry_for_generation(runtime, data_root, core_generation_id);
    if !runtime.relational_retry.ready() {
        let mut status = read_relational_status(data_root)
            .unwrap_or_else(|| relational_failure_status(core_generation_id, None));
        status["reason"] = Value::String("retry_backoff".to_owned());
        preserve_daemon_retry_state(&mut status, &runtime.relational_retry);
        return Ok(SourceBackedRelationalCatchUpRun {
            status,
            did_work: false,
        });
    }

    #[cfg(test)]
    let run = daemon_test_job("relational_projection")
        .map(|status| SourceBackedRelationalCatchUpRun {
            did_work: status
                .get("did_work")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            status,
        })
        .map(Ok)
        .unwrap_or_else(|| run_relational_after_core_publication(data_root, core_generation_id));
    #[cfg(not(test))]
    let run = run_relational_after_core_publication(data_root, core_generation_id);

    let run = run.unwrap_or_else(|error| SourceBackedRelationalCatchUpRun {
        status: relational_failure_status(core_generation_id, Some(format!("{error:#}"))),
        did_work: false,
    });
    let status = record_daemon_job_retry(&mut runtime.relational_retry, run.status);
    persist_relational_status(data_root, &status)?;
    Ok(SourceBackedRelationalCatchUpRun {
        status,
        did_work: run.did_work,
    })
}

fn relational_failure_status(core_generation_id: &str, error: Option<String>) -> Value {
    compact_json(json!({
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
        "build_generation": null,
        "attempts": 0,
        "last_attempt_at_ms": utc_now().timestamp_millis(),
        "error_code": "source_relational_status_unavailable",
        "last_error": error.unwrap_or_else(|| "relational catch-up is awaiting retry".to_owned()),
    }))
}

fn prepare_relational_retry_for_generation(
    runtime: &mut DaemonRuntime,
    data_root: &Path,
    core_generation_id: &str,
) {
    if relational_status_generation(data_root).as_deref() != Some(core_generation_id) {
        runtime.relational_retry.reset();
    }
}

fn run_pro_catch_up_with_retry(
    data_root: &Path,
    runtime: &mut DaemonRuntime,
    core_generation_id: &str,
    authority: Option<&PinnedCorePublication>,
) -> Result<SourceBackedProCatchUpRun> {
    prepare_pro_retry_for_generation(runtime, data_root, core_generation_id);
    if !runtime.pro_retry.ready() {
        let mut status = read_pro_status(data_root).unwrap_or_else(|| {
            compact_json(json!({
                "schema_version": 1,
                "owner": "daemon",
                "kind": "source_backed_pro_catch_up",
                "status": "pending",
                "pending": true,
                "retryable": true,
                "core_generation_id": core_generation_id,
            }))
        });
        status["reason"] = Value::String("retry_backoff".to_owned());
        preserve_daemon_retry_state(&mut status, &runtime.pro_retry);
        return Ok(SourceBackedProCatchUpRun {
            status,
            did_work: false,
        });
    }
    let run = run_after_core_publication(data_root, core_generation_id, authority)?;
    let status = record_daemon_job_retry(&mut runtime.pro_retry, run.status);
    persist_pro_status(data_root, &status)?;
    Ok(SourceBackedProCatchUpRun {
        status,
        did_work: run.did_work,
    })
}

fn prepare_pro_retry_for_generation(
    runtime: &mut DaemonRuntime,
    data_root: &Path,
    core_generation_id: &str,
) {
    if pro_status_generation(data_root).as_deref() != Some(core_generation_id) {
        runtime.pro_retry.reset();
    }
}

fn daemon_mode_runs_core_pro_catch_up(mode: crate::config::DaemonMode) -> bool {
    !mode.runs_only_source_refresh()
}

fn daemon_mode_runs_core_relational_catch_up(mode: crate::config::DaemonMode) -> bool {
    !mode.runs_only_source_refresh()
}

fn daemon_mode_runs_core_semantic_projection(mode: crate::config::DaemonMode) -> bool {
    !mode.runs_only_source_refresh()
}

fn finish_core_refresh(
    data_root: &Path,
    runtime: &mut DaemonRuntime,
    job: Value,
    did_work: bool,
) -> Result<DaemonIteration> {
    let job = record_daemon_job_retry(&mut runtime.history_retry, job);
    write_daemon_job_status(&daemon_core_refresh_job_path(data_root), &job)?;
    let failed = daemon_job_failed(&job);
    let state = daemon_core_cycle_state(&job);
    let published_generation = (!failed
        && job.get("status").and_then(Value::as_str) == Some("completed"))
    .then(|| {
        job.get("published_generation")
            .and_then(Value::as_str)
            .map(str::to_owned)
    })
    .flatten();
    if let Some(generation) = published_generation {
        runtime.sidecar_drain.generation = Some(generation);
        Ok(immediate_follow_up(DaemonIteration::new(
            did_work, false, state,
        )))
    } else {
        Ok(DaemonIteration::new(did_work, failed, state))
    }
}

fn daemon_core_cycle_state(job: &Value) -> DaemonCycleStateV1 {
    let history_backoff = daemon_job_in_retry_backoff(job);
    DaemonCycleStateV1::new(
        daemon_core_freshness(job, history_backoff),
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

fn daemon_core_freshness(job: &Value, backoff: bool) -> DaemonHistoryFreshnessV1 {
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
    let status = read_daemon_job_status(&daemon_core_refresh_job_path(data_root));
    runtime.history_retry.restore(status.as_ref());
}

pub(super) fn restore_daemon_consumer_retries(runtime: &mut DaemonRuntime, data_root: &Path) {
    let pro = read_pro_status(data_root);
    runtime.pro_retry.restore(pro.as_ref());
    let relational = read_relational_status(data_root);
    runtime.relational_retry.restore(relational.as_ref());
    let semantic = read_daemon_job_status(&daemon_semantic_job_path(data_root));
    runtime.semantic_retry.restore(semantic.as_ref());
}

pub(super) fn daemon_retry_due(runtime: &DaemonRuntime) -> bool {
    (runtime.history_retry.consecutive_failures > 0 && runtime.history_retry.ready())
        || (runtime.pro_retry.consecutive_failures > 0 && runtime.pro_retry.ready())
        || (runtime.relational_retry.consecutive_failures > 0 && runtime.relational_retry.ready())
        || (runtime.semantic_retry.consecutive_failures > 0 && runtime.semantic_retry.ready())
}

fn core_refresh_failed_job(data_root: &Path, message: String) -> Value {
    core_scheduler_job(data_root, "failed", None, Some(message))
}

fn core_refresh_retry_backoff_job(data_root: &Path, backoff: &DaemonRetryBackoff) -> Value {
    let mut job = core_scheduler_job(
        data_root,
        "skipped",
        Some("retry_backoff"),
        read_daemon_job_status(&daemon_core_refresh_job_path(data_root)).and_then(|job| {
            job.get("last_error")
                .and_then(Value::as_str)
                .map(str::to_owned)
        }),
    );
    preserve_daemon_retry_state(&mut job, backoff);
    job
}

fn core_scheduler_job(
    data_root: &Path,
    status: &str,
    reason: Option<&str>,
    last_error: Option<String>,
) -> Value {
    let previous = read_daemon_job_status(&daemon_core_refresh_job_path(data_root));
    compact_json(json!({
        "mode": "background",
        "owner": "daemon",
        "kind": "core_refresh",
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

fn run_daemon_semantic_job_with_retry(
    args: &DaemonRunArgs,
    data_root: &Path,
    runtime: &mut DaemonRuntime,
    deadline: Option<Instant>,
    semantic_enabled: bool,
    core_generation_id: Option<&str>,
) -> Value {
    if let Some(job) = runtime.semantic_blocked_job.as_ref() {
        return job.clone();
    }
    if !runtime.semantic_retry.ready() {
        let job = daemon_semantic_retry_backoff_job(data_root, &runtime.semantic_retry);
        return bind_semantic_generation(job, core_generation_id);
    }
    let job = run_daemon_semantic_job(args, data_root, runtime, deadline, semantic_enabled)
        .unwrap_or_else(|error| daemon_semantic_failed_job(data_root, error));
    let job = bind_semantic_generation(job, core_generation_id);
    let job = record_daemon_job_retry(&mut runtime.semantic_retry, job);
    if semantic_failure_class_from_job(&job).is_some_and(SemanticFailureClass::blocks_until_restart)
    {
        runtime.semantic_blocked_job = Some(job.clone());
    }
    job
}

fn bind_semantic_generation(mut job: Value, core_generation_id: Option<&str>) -> Value {
    if let Some(core_generation_id) = core_generation_id {
        job["core_generation_id"] = Value::String(core_generation_id.to_owned());
    }
    job
}

fn semantic_generation_needs_catch_up(data_root: &Path, core_generation_id: &str) -> bool {
    let Some(job) = read_daemon_job_status(&daemon_semantic_job_path(data_root)) else {
        return true;
    };
    job.get("core_generation_id").and_then(Value::as_str) != Some(core_generation_id)
        || job.get("status").and_then(Value::as_str) != Some("ready")
}

fn prepare_semantic_retry_for_generation(
    runtime: &mut DaemonRuntime,
    data_root: &Path,
    core_generation_id: &str,
) {
    let status_generation =
        read_daemon_job_status(&daemon_semantic_job_path(data_root)).and_then(|job| {
            job.get("core_generation_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    if status_generation.as_deref() != Some(core_generation_id) {
        runtime.semantic_retry.reset();
        runtime.semantic_blocked_job = None;
    }
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

pub(super) fn daemon_run_start_mode(args: &DaemonRunArgs) -> DaemonStartModeArg {
    args.start_mode.unwrap_or(DaemonStartModeArg::Manual)
}

pub(super) fn daemon_job_failed(value: &Value) -> bool {
    value.get("status").and_then(Value::as_str) == Some("failed")
}

fn daemon_semantic_job_did_work(value: &Value) -> bool {
    value
        .get("indexed_chunks")
        .and_then(Value::as_u64)
        .is_some_and(|chunks| chunks > 0)
        || value
            .get("source_records_scanned")
            .and_then(Value::as_u64)
            .is_some_and(|records| records > 0)
}

fn write_daemon_job_status_unless_deadline_skip(path: &Path, value: &Value) -> Result<()> {
    if value.get("status").and_then(Value::as_str) == Some("skipped")
        && value.get("reason").and_then(Value::as_str) == Some("daemon_deadline")
        && path.exists()
    {
        return Ok(());
    }
    write_daemon_job_status(path, value)
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
    daemon_retry::{semantic_failure_class_from_job, DaemonRetryBackoff, SemanticFailureClass},
    daemon_worker::{
        daemon_semantic_failed_job, daemon_semantic_retry_backoff_job, run_daemon_semantic_job,
    },
    paths_status::{
        daemon_core_refresh_job_path, daemon_semantic_job_path, read_daemon_job_status,
        write_daemon_job_status,
    },
    query_service::DaemonQueryActivity,
    runtime_limits::DAEMON_MIN_REMAINING_FOR_JOB_SECS,
    source_backed_pro_catch_up::{
        persist_status_json as persist_pro_status, read_status_json as read_pro_status,
        run_after_core_publication, status_generation as pro_status_generation,
        SourceBackedProCatchUpRun,
    },
    source_backed_refresh_coordinator::{
        pin_published_generation, CoreRefreshEngine, PinnedCorePublication,
    },
    source_backed_relational_catch_up::{
        generation_needs_catch_up as relational_generation_needs_catch_up,
        persist_status_json as persist_relational_status,
        read_status_json as read_relational_status,
        run_after_core_publication as run_relational_after_core_publication,
        status_generation as relational_status_generation, SourceBackedRelationalCatchUpRun,
    },
};

#[cfg(test)]
use super::daemon::daemon_test_job;

#[cfg(test)]
#[path = "daemon_scheduler_tests.rs"]
mod tests;
