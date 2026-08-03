#[derive(Default)]
pub(super) struct DaemonSidecarDrain {
    pub(super) generation: Option<String>,
    pub(super) pro_attempted_generation: Option<String>,
    pub(super) pro_attempted_recheck: Option<String>,
    pub(super) semantic_attempted_generation: Option<String>,
}

pub(super) const DAEMON_BACKGROUND_REFRESH_MIN_REST: StdDuration = StdDuration::from_secs(5);
const DAEMON_BACKGROUND_REFRESH_MAX_REST: StdDuration = StdDuration::from_secs(15 * 60);
const DAEMON_BACKGROUND_REFRESH_RECOVERY_FILE: &str = "background-refresh-recovery.json";

#[derive(Debug)]
struct DaemonBackgroundRefreshRecoveryProvenance {
    request_id: String,
    recovery_started_at_ms: u64,
}

impl DaemonBackgroundRefreshRecoveryProvenance {
    fn from_automatic_status(status: &Value, recovery_started_at_ms: u64) -> Option<Self> {
        if status.get("operation").and_then(Value::as_str) != Some("refresh")
            || status.get("trigger").and_then(Value::as_str) != Some("periodic")
            || status.get("trigger_provenance").and_then(Value::as_str) != Some("daemon_scheduler")
        {
            return None;
        }
        let request_id = status
            .get("request_id")
            .and_then(Value::as_str)
            .filter(|request_id| !request_id.is_empty())?
            .to_owned();
        Some(Self {
            request_id,
            recovery_started_at_ms,
        })
    }

    fn from_json(value: &Value) -> Option<Self> {
        if value.get("schema_version").and_then(Value::as_u64) != Some(1)
            || value.get("kind").and_then(Value::as_str)
                != Some("background_refresh_recovery_provenance")
            || value.get("trigger").and_then(Value::as_str) != Some("periodic")
            || value.get("trigger_provenance").and_then(Value::as_str) != Some("daemon_scheduler")
        {
            return None;
        }
        let request_id = value
            .get("request_id")
            .and_then(Value::as_str)
            .filter(|request_id| !request_id.is_empty())?
            .to_owned();
        let recovery_started_at_ms = value.get("recovery_started_at_ms")?.as_u64()?;
        Some(Self {
            request_id,
            recovery_started_at_ms,
        })
    }

    fn to_json(&self) -> Value {
        compact_json(json!({
            "schema_version": 1,
            "kind": "background_refresh_recovery_provenance",
            "request_id": self.request_id,
            "recovery_started_at_ms": self.recovery_started_at_ms,
            "trigger": "periodic",
            "trigger_provenance": "daemon_scheduler",
        }))
    }

    fn matches_recovered_publication(&self, status: &Value) -> bool {
        status.get("request_id").and_then(Value::as_str) == Some(self.request_id.as_str())
            && status.get("request_state").and_then(Value::as_str) == Some("published")
            && status.get("status").and_then(Value::as_str) == Some("completed")
            && status.get("trigger").and_then(Value::as_str) == Some("recovery")
            && status.get("trigger_provenance").and_then(Value::as_str) == Some("commit_payload")
            && status
                .get("published_generation")
                .and_then(Value::as_str)
                .is_some_and(|generation| !generation.is_empty())
            && status.get("receipt").is_some_and(Value::is_object)
    }
}

/// Monotonic, duration-aware cadence for automatic provider capture.
///
/// Explicit requests are admitted before this policy is consulted. Automatic
/// work rests for at least five seconds and, up to the cap, for as long as the
/// previous capture took. This prevents a continuously dirty route from
/// turning the daemon into a tight publisher loop while keeping the tuning
/// independent from route-local failure backoff.
#[derive(Debug, Default)]
pub(super) struct DaemonBackgroundRefreshCadence {
    not_before: Option<Instant>,
}

impl DaemonBackgroundRefreshCadence {
    fn ready(&self, now: Instant) -> bool {
        self.not_before.is_none_or(|not_before| now >= not_before)
    }

    pub(super) fn remaining(&self, now: Instant) -> Option<StdDuration> {
        self.not_before
            .and_then(|not_before| not_before.checked_duration_since(now))
    }

    pub(super) fn record_completion(&mut self, started: Instant, completed: Instant) {
        let capture_duration = completed.saturating_duration_since(started);
        let rest = background_refresh_rest(capture_duration);
        self.not_before = completed.checked_add(rest).or(Some(completed));
    }

    fn restore(
        &mut self,
        status: Option<&Value>,
        recovered_automatic_at_ms: Option<u64>,
        wall_now_ms: u64,
        now: Instant,
    ) {
        let Some(status) = status else {
            return;
        };
        let periodic = status.get("operation").and_then(Value::as_str) == Some("refresh")
            && status.get("trigger").and_then(Value::as_str) == Some("periodic")
            && status.get("trigger_provenance").and_then(Value::as_str) == Some("daemon_scheduler");
        if !periodic && recovered_automatic_at_ms.is_none() {
            return;
        }
        let Some(finished_at_ms) =
            status_timestamp_ms(status, "finished_at_ms").or(recovered_automatic_at_ms)
        else {
            return;
        };
        let started_at_ms = status_timestamp_ms(status, "started_at_ms")
            .or_else(|| status_timestamp_ms(status, "last_run_at_ms"))
            .unwrap_or(finished_at_ms);
        let maximum_rest_ms =
            u64::try_from(DAEMON_BACKGROUND_REFRESH_MAX_REST.as_millis()).unwrap_or(u64::MAX);
        let capture_duration = StdDuration::from_millis(
            finished_at_ms
                .saturating_sub(started_at_ms)
                .min(maximum_rest_ms),
        );
        let rest_ms = u64::try_from(background_refresh_rest(capture_duration).as_millis())
            .unwrap_or(u64::MAX);
        let not_before_at_ms = finished_at_ms.saturating_add(rest_ms);
        // Wall clocks may move backward or persisted status may be malformed.
        // Recovery never extends the monotonic cooldown beyond its normal cap.
        let remaining_ms = not_before_at_ms
            .saturating_sub(wall_now_ms)
            .min(maximum_rest_ms);
        if remaining_ms == 0 {
            self.not_before = None;
            return;
        }
        self.not_before = now
            .checked_add(StdDuration::from_millis(remaining_ms))
            .or(Some(now));
    }
}

fn background_refresh_rest(capture_duration: StdDuration) -> StdDuration {
    capture_duration
        .max(DAEMON_BACKGROUND_REFRESH_MIN_REST)
        .min(DAEMON_BACKGROUND_REFRESH_MAX_REST)
}

fn status_timestamp_ms(status: &Value, field: &str) -> Option<u64> {
    status
        .get(field)
        .and_then(Value::as_i64)
        .and_then(|value| u64::try_from(value).ok())
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
        return run_core_refresh(data_root, runtime, source_refresh, false);
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
    if !runtime.history_retry.ready() {
        let job = core_refresh_retry_backoff_job(data_root, &runtime.history_retry);
        let job = persist_core_scheduler_status(data_root, source_refresh, job)?;
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
    if runtime.history_retry.consecutive_failures > 0 {
        return run_core_refresh(data_root, runtime, source_refresh, true);
    }
    run_dirty_core_refresh(data_root, runtime, source_refresh)
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
    let job =
        record_source_refresh_retry(data_root, &mut runtime.history_retry, coordinator, run.job)?;
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
    enqueue_periodic: bool,
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
    if enqueue_periodic {
        if let Err(error) = coordinator.enqueue_periodic(data_root) {
            return finish_core_refresh(
                data_root,
                runtime,
                Some(coordinator),
                core_refresh_failed_job(
                    data_root,
                    format!("schedule periodic Core refresh: {error:#}"),
                ),
                false,
            );
        }
    }
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
    let job =
        record_source_refresh_retry(data_root, &mut runtime.history_retry, coordinator, run.job)?;
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
    run_before_core_scheduler_status_hook();
    let Some(coordinator) = coordinator else {
        write_daemon_job_status(&daemon_core_refresh_job_path(data_root), &job)?;
        return Ok(job);
    };
    coordinator.persist_scheduler_status(data_root, job)
}

#[cfg(test)]
thread_local! {
    static BEFORE_CORE_SCHEDULER_STATUS_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn install_before_core_scheduler_status_hook_for_test(hook: impl FnOnce() + 'static) {
    BEFORE_CORE_SCHEDULER_STATUS_HOOK.with(|slot| {
        let previous = slot.replace(Some(Box::new(hook)));
        assert!(
            previous.is_none(),
            "scheduler status test hooks must not nest"
        );
    });
}

#[cfg(test)]
fn run_before_core_scheduler_status_hook() {
    BEFORE_CORE_SCHEDULER_STATUS_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(not(test))]
fn run_before_core_scheduler_status_hook() {}

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

pub(super) fn restore_daemon_background_refresh_cadence(
    runtime: &mut DaemonRuntime,
    data_root: &Path,
) {
    let status = read_daemon_job_status(&daemon_core_refresh_job_path(data_root));
    let recovered_automatic_at_ms = read_daemon_job_status(
        &daemon_background_refresh_recovery_provenance_path(data_root),
    )
    .as_ref()
    .and_then(DaemonBackgroundRefreshRecoveryProvenance::from_json)
    .zip(status.as_ref())
    .and_then(|(provenance, status)| {
        provenance
            .matches_recovered_publication(status)
            .then_some(provenance.recovery_started_at_ms)
    });
    runtime.background_refresh_cadence.restore(
        status.as_ref(),
        recovered_automatic_at_ms,
        source_route_ledger_now_ms(),
        Instant::now(),
    );
}

pub(super) fn preserve_daemon_background_refresh_recovery_provenance(
    data_root: &Path,
) -> Result<()> {
    let status = read_daemon_job_status(&daemon_core_refresh_job_path(data_root));
    let recovery_started_at_ms = source_route_ledger_now_ms();
    let existing = read_daemon_job_status(&daemon_background_refresh_recovery_provenance_path(
        data_root,
    ))
    .as_ref()
    .and_then(DaemonBackgroundRefreshRecoveryProvenance::from_json);
    let Some(provenance) = status.as_ref().and_then(|status| {
        DaemonBackgroundRefreshRecoveryProvenance::from_automatic_status(
            status,
            recovery_started_at_ms,
        )
        .or_else(|| {
            existing.and_then(|existing| {
                existing.matches_recovered_publication(status).then_some(
                    DaemonBackgroundRefreshRecoveryProvenance {
                        request_id: existing.request_id,
                        recovery_started_at_ms,
                    },
                )
            })
        })
    }) else {
        return Ok(());
    };
    write_daemon_job_status(
        &daemon_background_refresh_recovery_provenance_path(data_root),
        &provenance.to_json(),
    )
}

fn daemon_background_refresh_recovery_provenance_path(data_root: &Path) -> std::path::PathBuf {
    daemon_core_refresh_job_path(data_root).with_file_name(DAEMON_BACKGROUND_REFRESH_RECOVERY_FILE)
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
    data_root: &Path,
    backoff: &mut DaemonRetryBackoff,
    coordinator: &CoreRefreshEngine,
    job: Value,
) -> Result<Value> {
    let persist_retry = daemon_job_should_backoff(&job);
    let job = record_daemon_job_retry(backoff, job);
    if persist_retry {
        return coordinator.persist_retry_status(data_root, job);
    }
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
