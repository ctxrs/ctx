mod background_refresh_cadence;

#[cfg(test)]
pub(super) use background_refresh_cadence::DAEMON_BACKGROUND_REFRESH_MIN_REST;
#[cfg(test)]
use background_refresh_cadence::{background_refresh_rest, DAEMON_BACKGROUND_REFRESH_MAX_REST};
pub(super) use background_refresh_cadence::{
    preserve_daemon_background_refresh_recovery_provenance,
    restore_daemon_background_refresh_cadence, DaemonBackgroundRefreshCadence,
};

#[derive(Default)]
pub(super) struct DaemonSidecarDrain {
    pub(super) generation: Option<String>,
    pub(super) pro_attempted_generation: Option<String>,
    pub(super) pro_attempted_recheck: Option<String>,
    pub(super) semantic_attempted_generation: Option<String>,
}

pub(super) const DAEMON_CONSUMER_RETRY_QUERY_GRACE: StdDuration = StdDuration::from_secs(2);

#[derive(Debug, Default)]
pub(super) struct DaemonConsumerRetryDeferral {
    pub(super) retry_at: Option<Instant>,
}

impl DaemonConsumerRetryDeferral {
    fn defer_for_foreground_query(&mut self, now: Instant) -> bool {
        let retry_at = self
            .retry_at
            .get_or_insert(now + DAEMON_CONSUMER_RETRY_QUERY_GRACE);
        if now < *retry_at {
            return true;
        }
        self.reset();
        false
    }

    pub(super) fn remaining(&self, now: Instant) -> Option<StdDuration> {
        self.retry_at
            .and_then(|retry_at| retry_at.checked_duration_since(now))
    }

    fn reset(&mut self) {
        self.retry_at = None;
    }
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
        runtime.consumer_retry_deferral.reset();
        if let Some(activity) = query_activity {
            activity.cancel_idle_wakeup();
        }
        if source_refresh_requested && !runtime.history_retry.ready() {
            return Ok(deferred_pending_core_refresh(data_root, runtime));
        }
        if let Some(iteration) = run_pending_core_refresh(data_root, runtime, source_refresh)? {
            return Ok(iteration);
        }
        return run_dirty_core_refresh(data_root, runtime, source_refresh);
    }
    let query_generation = query_activity.map(|activity| activity.snapshot().1);
    if source_refresh_requested {
        runtime.consumer_retry_deferral.reset();
        if let Some(activity) = query_activity {
            activity.cancel_idle_wakeup();
        }
        if !runtime.history_retry.ready() {
            return Ok(deferred_pending_core_refresh(data_root, runtime));
        }
        return run_core_refresh(data_root, runtime, source_refresh);
    }
    if daemon_foreground_query_preempts(query_activity, query_generation) {
        if !daemon_consumer_retry_due(runtime) {
            runtime.consumer_retry_deferral.reset();
            if let Some(activity) = query_activity {
                activity.cancel_idle_wakeup();
            }
            return Ok(DaemonIteration::new(
                false,
                false,
                DaemonCycleStateV1::unknown(),
            ));
        }
        if runtime
            .consumer_retry_deferral
            .defer_for_foreground_query(Instant::now())
        {
            if let Some(activity) = query_activity {
                activity.wake_daemon_when_idle();
            }
            return Ok(DaemonIteration::new(
                false,
                false,
                DaemonCycleStateV1::unknown(),
            ));
        }
        if let Some(activity) = query_activity {
            activity.cancel_idle_wakeup();
        }
    } else {
        runtime.consumer_retry_deferral.reset();
        if let Some(activity) = query_activity {
            activity.cancel_idle_wakeup();
        }
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
    // Global history backoff belongs only to an admitted control-plane
    // finalization. With no pending engine request there is nothing global to
    // retry, and manufacturing periodic capture work would violate the route
    // ledger's retry/block dispositions.
    if runtime.history_retry.consecutive_failures > 0 {
        runtime.history_retry.reset();
    }
    if !daemon_deadline_has_min_budget(deadline, DAEMON_MIN_REMAINING_FOR_JOB_SECS) {
        return Ok(DaemonIteration::new(
            false,
            false,
            DaemonCycleStateV1::unknown(),
        ));
    }
    run_dirty_core_refresh(data_root, runtime, source_refresh)
}

fn deferred_pending_core_refresh(data_root: &Path, runtime: &DaemonRuntime) -> DaemonIteration {
    let job = core_refresh_retry_backoff_job(data_root, &runtime.history_retry);
    DaemonIteration::new(false, false, daemon_core_cycle_state(&job))
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
    let page_continuation_pending = semantic_page_continuation_pending(data_root, generation_id);
    let retry_due =
        runtime.semantic_retry.consecutive_failures > 0 && runtime.semantic_retry.ready();
    if runtime.sidecar_drain.generation.as_deref() == Some(generation_id)
        && runtime
            .sidecar_drain
            .semantic_attempted_generation
            .as_deref()
            == Some(generation_id)
        && !page_continuation_pending
        && !retry_due
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
    let retry_due = runtime.pro_retry.consecutive_failures > 0 && runtime.pro_retry.ready();
    let retained_authority = source_refresh.and_then(CoreRefreshEngine::pinned_core_publication);
    // Reading an absent intent is the ordinary fast path. Only a pending
    // intent resolves the installed helper identity (and may take its lock).
    let helper_recheck = helper_recheck_schedule(data_root)?;
    let helper_recheck_request = helper_recheck
        .as_ref()
        .map(|schedule| schedule.attempt_key.clone());
    let helper_recheck_due = helper_recheck_request.is_some()
        && helper_recheck_request != runtime.sidecar_drain.pro_attempted_recheck;
    if helper_recheck
        .as_ref()
        .is_some_and(|schedule| !schedule.target_ready)
    {
        // Remember the old/missing installed identity. Once the transaction
        // publishes the target helper, the attempt key changes and this same
        // daemon automatically reconsiders the unchanged Core generation.
        runtime.sidecar_drain.pro_attempted_recheck = helper_recheck_request;
        return Ok(None);
    }
    let installation_requires_recheck = helper_recheck_due
        || (retained_authority.is_none()
            && runtime.sidecar_drain.pro_attempted_generation.is_some()
            && pro_installation_requires_recheck(data_root));
    let durable_check_required = retained_authority.is_none()
        && (runtime.sidecar_drain.pro_attempted_generation.is_none()
            || retry_due
            || installation_requires_recheck);
    let durable_authority = if durable_check_required {
        pin_published_generation(data_root)?
    } else {
        None
    };
    let authority = retained_authority
        .as_deref()
        .map(SourceBackedProCoreAuthority::Retained)
        .or_else(|| {
            durable_authority
                .as_ref()
                .map(SourceBackedProCoreAuthority::Durable)
        });
    let Some(authority) = authority else {
        return Ok(None);
    };
    let generation = authority.generation_id();
    if runtime.sidecar_drain.pro_attempted_generation.as_deref() == Some(generation)
        && !retry_due
        && !installation_requires_recheck
    {
        return Ok(None);
    }
    prepare_pro_retry_for_generation(runtime, data_root, generation);
    if !runtime.pro_retry.ready() {
        runtime.sidecar_drain.pro_attempted_generation = Some(generation.to_owned());
        return Ok(None);
    }
    let run = run_pro_catch_up_with_retry(data_root, runtime, generation, authority)?;
    runtime.sidecar_drain.pro_attempted_generation = Some(generation.to_owned());
    runtime.sidecar_drain.pro_attempted_recheck = helper_recheck_request;
    runtime.sidecar_drain.generation = Some(generation.to_owned());
    Ok(Some(core_pro_catch_up_iteration(run.did_work)))
}

fn pro_installation_requires_recheck(data_root: &Path) -> bool {
    read_pro_status(data_root).is_some_and(|status| {
        status.get("error_code").and_then(Value::as_str) == Some("pro_not_installed")
            && ProFilesystemLayout::new(data_root).helper_path().exists()
    })
}

fn core_pro_catch_up_iteration(did_work: bool) -> DaemonIteration {
    let iteration = DaemonIteration::new(did_work, false, DaemonCycleStateV1::unknown());
    if did_work {
        immediate_follow_up(iteration)
    } else {
        iteration
    }
}

fn run_pending_core_refresh(
    data_root: &Path,
    runtime: &mut DaemonRuntime,
    source_refresh: Option<&CoreRefreshEngine>,
) -> Result<Option<DaemonIteration>> {
    let Some(coordinator) = source_refresh else {
        return Ok(None);
    };
    let Some(run) = coordinator.run_next(data_root) else {
        return Ok(None);
    };
    let job = record_source_refresh_retry(
        data_root,
        &mut runtime.history_retry,
        coordinator,
        run.job,
        run.terminal_persistence_pending,
    )?;
    Ok(Some(DaemonIteration::new(
        run.did_work,
        run.failed || run.terminal_persistence_pending,
        daemon_core_cycle_state(&job),
    )))
}

fn run_dirty_core_refresh(
    data_root: &Path,
    runtime: &mut DaemonRuntime,
    source_refresh: Option<&CoreRefreshEngine>,
) -> Result<DaemonIteration> {
    let Some(source_refresh) = source_refresh else {
        return Ok(DaemonIteration::new(
            false,
            false,
            DaemonCycleStateV1::unknown(),
        ));
    };
    if !runtime.background_refresh_cadence.ready(Instant::now()) {
        return Ok(DaemonIteration::new(
            false,
            false,
            DaemonCycleStateV1::unknown(),
        ));
    }
    if !source_refresh.enqueue_next_scheduled_refresh(data_root, source_route_ledger_now_ms())? {
        return Ok(DaemonIteration::new(
            false,
            false,
            DaemonCycleStateV1::unknown(),
        ));
    }
    let capture_started = Instant::now();
    let Some(run) = source_refresh.run_next(data_root) else {
        return Ok(DaemonIteration::new(
            false,
            false,
            DaemonCycleStateV1::unknown(),
        ));
    };
    runtime
        .background_refresh_cadence
        .record_completion(capture_started, Instant::now());
    let cold_all_refresh = run.scope == SourceBackedRefreshScope::All
        && run.job.get("trigger").and_then(Value::as_str) == Some("periodic")
        && run
            .job
            .get("previous_generation")
            .is_none_or(Value::is_null);
    debug_assert!(
        matches!(run.scope, SourceBackedRefreshScope::Exact(_))
            || (run.scope == SourceBackedRefreshScope::All
                && run.job.get("trigger").and_then(Value::as_str) == Some("import"))
            || cold_all_refresh,
        "dirty-route work may become All only for cold startup or when a manual import upgrades the queued exact refresh"
    );
    let terminal_persistence_pending = run.terminal_persistence_pending;
    let job = record_source_refresh_retry(
        data_root,
        &mut runtime.history_retry,
        source_refresh,
        run.job,
        terminal_persistence_pending,
    )?;
    let published_generation = (!run.failed
        && !terminal_persistence_pending
        && job.get("status").and_then(Value::as_str) == Some("completed"))
    .then(|| {
        job.get("published_generation")
            .and_then(Value::as_str)
            .map(str::to_owned)
    })
    .flatten();
    let iteration = DaemonIteration::new(
        run.did_work,
        run.failed || terminal_persistence_pending,
        daemon_core_cycle_state(&job),
    );
    if let Some(generation) = published_generation {
        runtime.sidecar_drain.generation = Some(generation);
        // A successor queued behind an exact-route fence must yield to the
        // daemon loop once so pending watcher events enter the dirty-route
        // ledger before that successor is admitted.
        if source_refresh.has_pending_request() {
            return Ok(iteration);
        }
        return Ok(immediate_follow_up(iteration));
    }
    Ok(iteration)
}

fn run_core_refresh(
    data_root: &Path,
    runtime: &mut DaemonRuntime,
    source_refresh: Option<&CoreRefreshEngine>,
) -> Result<DaemonIteration> {
    let Some(coordinator) = source_refresh else {
        return finish_core_refresh(
            data_root,
            runtime,
            None,
            core_refresh_failed_job(
                data_root,
                "daemon Core refresh engine is unavailable".to_owned(),
            ),
            false,
        );
    };
    let Some(run) = coordinator.run_next(data_root) else {
        return finish_core_refresh(
            data_root,
            runtime,
            Some(coordinator),
            core_refresh_failed_job(
                data_root,
                "daemon Core refresh engine has no admitted request".to_owned(),
            ),
            false,
        );
    };
    let terminal_persistence_pending = run.terminal_persistence_pending;
    let job = record_source_refresh_retry(
        data_root,
        &mut runtime.history_retry,
        coordinator,
        run.job,
        terminal_persistence_pending,
    )?;
    let failed = run.failed || terminal_persistence_pending;
    let state = daemon_core_cycle_state(&job);
    if !failed && job.get("status").and_then(Value::as_str) == Some("completed") {
        if let Some(generation) = job.get("published_generation").and_then(Value::as_str) {
            runtime.sidecar_drain.generation = Some(generation.to_owned());
        }
        return Ok(immediate_follow_up(DaemonIteration::new(
            run.did_work,
            false,
            state,
        )));
    }
    Ok(DaemonIteration::new(run.did_work, failed, state))
}

fn run_pro_catch_up_with_retry(
    data_root: &Path,
    runtime: &mut DaemonRuntime,
    core_generation_id: &str,
    authority: SourceBackedProCoreAuthority<'_>,
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

fn daemon_mode_runs_core_semantic_projection(mode: crate::config::DaemonMode) -> bool {
    !mode.runs_only_source_refresh()
}

fn finish_core_refresh(
    data_root: &Path,
    runtime: &mut DaemonRuntime,
    coordinator: Option<&CoreRefreshEngine>,
    job: Value,
    did_work: bool,
) -> Result<DaemonIteration> {
    let job = record_daemon_job_retry(&mut runtime.history_retry, job);
    let job = persist_core_scheduler_status(data_root, coordinator, job)?;
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

fn persist_core_scheduler_status(
    data_root: &Path,
    coordinator: Option<&CoreRefreshEngine>,
    job: Value,
) -> Result<Value> {
    let Some(coordinator) = coordinator else {
        write_daemon_job_status(&daemon_core_refresh_job_path(data_root), &job)?;
        return Ok(job);
    };
    coordinator.persist_scheduler_status(data_root, job)
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
    // Current records identify the only global retry domain explicitly. The
    // phase check is the narrow compatibility path for pre-domain records;
    // route-terminal capture failures must never restore global backoff.
    let finalization = status.as_ref().filter(|status| {
        status.get("retry_domain").and_then(Value::as_str) == Some("control_plane")
            || status
                .get("progress")
                .and_then(|progress| progress.get("phase"))
                .and_then(Value::as_str)
                == Some("persisting_terminal")
    });
    runtime.history_retry.restore(finalization);
}

pub(super) fn restore_daemon_consumer_retries(runtime: &mut DaemonRuntime, data_root: &Path) {
    let pro = read_pro_status(data_root);
    restore_consumer_retry(&mut runtime.pro_retry, pro.as_ref());
    let semantic = read_daemon_job_status(&daemon_semantic_job_path(data_root));
    restore_consumer_retry(&mut runtime.semantic_retry, semantic.as_ref());
}

fn restore_consumer_retry(backoff: &mut DaemonRetryBackoff, status: Option<&Value>) {
    backoff.restore(status);
    let persisted_failures = status
        .and_then(|status| status.get("consecutive_failures"))
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(u64::from(u32::MAX)) as u32;
    if backoff.consecutive_failures == 0
        && persisted_failures > 0
        && status
            .and_then(|status| status.get("retryable"))
            .and_then(Value::as_bool)
            == Some(true)
    {
        backoff.consecutive_failures = persisted_failures;
    }
}

pub(super) fn daemon_consumer_retry_due(runtime: &DaemonRuntime) -> bool {
    (runtime.pro_retry.consecutive_failures > 0 && runtime.pro_retry.ready())
        || (runtime.semantic_retry.consecutive_failures > 0 && runtime.semantic_retry.ready())
}

pub(super) fn daemon_retry_due(runtime: &DaemonRuntime) -> bool {
    (runtime.history_retry.consecutive_failures > 0 && runtime.history_retry.ready())
        || daemon_consumer_retry_due(runtime)
}

pub(super) fn daemon_scheduled_refresh_due(
    runtime: &DaemonRuntime,
    source_refresh: Option<&CoreRefreshEngine>,
    now: Instant,
    route_now_ms: u64,
) -> bool {
    runtime.background_refresh_cadence.ready(now)
        && source_refresh.and_then(|refresh| refresh.next_dirty_route_due_in_ms(route_now_ms))
            == Some(0)
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

fn semantic_page_continuation_pending(data_root: &Path, core_generation_id: &str) -> bool {
    read_daemon_job_status(&daemon_semantic_job_path(data_root)).is_some_and(|job| {
        job.get("core_generation_id").and_then(Value::as_str) == Some(core_generation_id)
            && job.get("source_generation_ready").and_then(Value::as_bool) == Some(false)
            && job.get("source_work_remaining").and_then(Value::as_bool) == Some(true)
            && !daemon_job_should_backoff(&job)
    })
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

fn record_source_refresh_retry(
    _data_root: &Path,
    backoff: &mut DaemonRetryBackoff,
    _coordinator: &CoreRefreshEngine,
    mut job: Value,
    status_persistence_pending: bool,
) -> Result<Value> {
    if status_persistence_pending {
        job["retryable"] = Value::Bool(true);
        job["retry_domain"] = Value::String("control_plane".to_owned());
        job["retry_advice"] = Value::String("retry_finalization".to_owned());
        return Ok(record_daemon_job_retry(backoff, job));
    }

    backoff.reset();
    // The engine already persisted this capture terminal together with its
    // logical-demand queue. Rewriting it here could discard the durable image
    // needed to cancel/replay attached demands after restart.
    Ok(job)
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
            .get("source_records_decoded")
            .and_then(Value::as_u64)
            .is_some_and(|records| records > 0)
}

fn source_route_ledger_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
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
use ctx_history_capture::SourceBackedRefreshScope;
use ctx_history_core::utc_now;
use ctx_pro_host_protocol::ProFilesystemLayout;
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
        helper_recheck_schedule, persist_status_json as persist_pro_status,
        read_status_json as read_pro_status, run_after_core_publication,
        status_generation as pro_status_generation, SourceBackedProCatchUpRun,
        SourceBackedProCoreAuthority,
    },
    source_backed_refresh_coordinator::{pin_published_generation, CoreRefreshEngine},
};

#[cfg(test)]
#[path = "daemon_scheduler_tests.rs"]
mod tests;
