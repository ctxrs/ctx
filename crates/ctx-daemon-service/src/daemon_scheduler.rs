#[derive(Default)]
pub(super) struct DaemonSidecarDrain {
    pub(super) generation: Option<String>,
    pub(super) semantic_attempted_generation: Option<String>,
}

pub(super) const DAEMON_CONSUMER_RETRY_QUERY_GRACE: StdDuration = StdDuration::from_secs(2);

#[derive(Debug, Default)]
pub(super) struct DaemonConsumerRetryDeferral {
    pub(super) retry_at: Option<Instant>,
}

pub(super) struct DaemonSchedulerCycleContext<'a> {
    pub(super) deadline: Option<Instant>,
    pub(super) semantic_enabled: bool,
    pub(super) query_activity: Option<&'a DaemonQueryActivity>,
    pub(super) source_refresh: Option<&'a CoreRefreshEngine>,
}

#[derive(Clone, Copy)]
pub(super) struct DaemonSemanticJobPorts<'a> {
    pub(super) artifact_fetcher: &'a dyn ctx_semantic_model::ArtifactFetcher,
    pub(super) config: &'a dyn crate::DaemonConfigPort,
}

pub(super) struct DaemonSchedulerPorts<'a, N: ?Sized> {
    pub(super) generation_published: &'a N,
    pub(super) semantic: DaemonSemanticJobPorts<'a>,
    pub(super) observation: &'a dyn crate::DaemonObservationPort,
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

fn with_provider_refresh(
    mut iteration: DaemonIteration,
    job: &Value,
    successor_pending: bool,
    terminal_persistence_pending: bool,
    observation: &dyn crate::DaemonObservationPort,
) -> DaemonIteration {
    if !terminal_persistence_pending {
        if let Some(event) = observation.provider_refresh_event(job, successor_pending) {
            iteration.provider_refresh_events.push(event);
        }
    }
    iteration
}

pub(super) fn run_daemon_scheduler_cycle_with_activity<N>(
    args: &DaemonRunArgs,
    data_root: &Path,
    runtime: &mut DaemonRuntime,
    cycle: DaemonSchedulerCycleContext<'_>,
    ports: DaemonSchedulerPorts<'_, N>,
) -> Result<DaemonIteration>
where
    N: crate::CoreGenerationPublishedPort + ?Sized,
{
    let DaemonSchedulerCycleContext {
        deadline,
        semantic_enabled,
        query_activity,
        source_refresh,
    } = cycle;
    let DaemonSchedulerPorts {
        generation_published,
        semantic,
        observation,
    } = ports;
    let source_refresh_requested =
        source_refresh.is_some_and(CoreRefreshEngine::has_pending_request);
    if args.profile == crate::DaemonRunProfile::FiniteCoreWorker {
        runtime.consumer_retry_deferral.reset();
        if source_refresh_requested && !runtime.history_retry.ready() {
            return Ok(deferred_pending_core_refresh(data_root, runtime));
        }
        return Ok(run_pending_core_refresh(
            data_root,
            runtime,
            source_refresh,
            // Finite workers publish Core only; this notification seam may
            // wake adjacent persistent maintenance consumers.
            false,
            generation_published,
            observation,
        )?
        .unwrap_or_else(|| DaemonIteration::new(false, false, DaemonCycleStateV1::unknown())));
    }
    refresh_semantic_intensity_observation(data_root, runtime)?;
    if runtime.config.daemon.mode.runs_only_source_refresh() {
        runtime.consumer_retry_deferral.reset();
        if let Some(activity) = query_activity {
            activity.cancel_idle_wakeup();
        }
        if source_refresh_requested && !runtime.history_retry.ready() {
            return Ok(deferred_pending_core_refresh(data_root, runtime));
        }
        if let Some(iteration) = run_pending_core_refresh(
            data_root,
            runtime,
            source_refresh,
            true,
            generation_published,
            observation,
        )? {
            return Ok(iteration);
        }
        let iteration = run_dirty_core_refresh(
            data_root,
            runtime,
            source_refresh,
            generation_published,
            observation,
        )?;
        return Ok(iteration);
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
        return run_core_refresh(
            data_root,
            runtime,
            source_refresh,
            generation_published,
            observation,
        );
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
    if let Some(iteration) = run_pending_core_semantic_catch_up(
        data_root,
        runtime,
        deadline,
        semantic_enabled,
        semantic,
    )? {
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
    run_dirty_core_refresh(
        data_root,
        runtime,
        source_refresh,
        generation_published,
        observation,
    )
}

fn refresh_semantic_intensity_observation(data_root: &Path, runtime: &DaemonRuntime) -> Result<()> {
    let path = daemon_semantic_job_path(data_root);
    let Some(mut job) = read_daemon_job_status(&path) else {
        return Ok(());
    };
    let intensity = runtime
        .semantic_intensity_leases
        .snapshot(runtime.config.semantic_indexing_intensity());
    let configured = intensity.configured.as_str();
    let effective = intensity.effective.as_str();
    if job
        .get("configured_indexing_intensity")
        .and_then(Value::as_str)
        == Some(configured)
        && job
            .get("effective_indexing_intensity")
            .and_then(Value::as_str)
            == Some(effective)
    {
        return Ok(());
    }
    annotate_current_semantic_indexing_intensity(&mut job, intensity);
    write_daemon_job_status(&path, &job)
}

fn deferred_pending_core_refresh(data_root: &Path, runtime: &DaemonRuntime) -> DaemonIteration {
    let job = core_refresh_retry_backoff_job(data_root, &runtime.history_retry);
    DaemonIteration::new(false, false, daemon_core_cycle_state(&job))
}

fn run_pending_core_semantic_catch_up(
    data_root: &Path,
    runtime: &mut DaemonRuntime,
    deadline: Option<Instant>,
    semantic_enabled: bool,
    semantic_ports: DaemonSemanticJobPorts<'_>,
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
        data_root,
        runtime,
        deadline,
        true,
        Some(generation_id),
        semantic_ports,
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

fn run_pending_core_refresh<N>(
    data_root: &Path,
    runtime: &mut DaemonRuntime,
    source_refresh: Option<&CoreRefreshEngine>,
    notify_generation_published: bool,
    generation_published: &N,
    observation: &dyn crate::DaemonObservationPort,
) -> Result<Option<DaemonIteration>>
where
    N: crate::CoreGenerationPublishedPort + ?Sized,
{
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
    let failed = run.failed || run.terminal_persistence_pending;
    if notify_generation_published && !run.terminal_persistence_pending {
        notify_core_generation_published(data_root, &job, generation_published);
    }
    let iteration = DaemonIteration::new(run.did_work, failed, daemon_core_cycle_state(&job));
    Ok(Some(with_provider_refresh(
        iteration,
        &job,
        coordinator.has_pending_request(),
        run.terminal_persistence_pending,
        observation,
    )))
}

fn run_dirty_core_refresh<N>(
    data_root: &Path,
    runtime: &mut DaemonRuntime,
    source_refresh: Option<&CoreRefreshEngine>,
    generation_published: &N,
    observation: &dyn crate::DaemonObservationPort,
) -> Result<DaemonIteration>
where
    N: crate::CoreGenerationPublishedPort + ?Sized,
{
    let Some(source_refresh) = source_refresh else {
        return Ok(DaemonIteration::new(
            false,
            false,
            DaemonCycleStateV1::unknown(),
        ));
    };
    if !source_refresh.enqueue_next_scheduled_refresh(data_root, source_route_ledger_now_ms())? {
        return Ok(DaemonIteration::new(
            false,
            false,
            DaemonCycleStateV1::unknown(),
        ));
    }
    let Some(run) = source_refresh.run_next(data_root) else {
        return Ok(DaemonIteration::new(
            false,
            false,
            DaemonCycleStateV1::unknown(),
        ));
    };
    let cold_all_refresh = run.scope == SourceBackedRefreshScope::All
        && matches!(
            run.job.get("trigger").and_then(Value::as_str),
            Some("periodic" | "setup")
        )
        && run
            .job
            .get("previous_generation")
            .is_none_or(Value::is_null);
    debug_assert!(
        matches!(run.scope, SourceBackedRefreshScope::Exact(_))
            || (run.scope == SourceBackedRefreshScope::All
                && run.job.get("trigger").and_then(Value::as_str) == Some("import"))
            || cold_all_refresh,
        "dirty-route work may become All only for cold startup/setup or when a manual import upgrades the queued exact refresh"
    );
    let terminal_persistence_pending = run.terminal_persistence_pending;
    let job = record_source_refresh_retry(
        data_root,
        &mut runtime.history_retry,
        source_refresh,
        run.job,
        terminal_persistence_pending,
    )?;
    if !terminal_persistence_pending {
        notify_core_generation_published(data_root, &job, generation_published);
    }
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
    let iteration = with_provider_refresh(
        iteration,
        &job,
        source_refresh.has_pending_request(),
        terminal_persistence_pending,
        observation,
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

fn run_core_refresh<N>(
    data_root: &Path,
    runtime: &mut DaemonRuntime,
    source_refresh: Option<&CoreRefreshEngine>,
    generation_published: &N,
    observation: &dyn crate::DaemonObservationPort,
) -> Result<DaemonIteration>
where
    N: crate::CoreGenerationPublishedPort + ?Sized,
{
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
            generation_published,
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
            generation_published,
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
    if !terminal_persistence_pending {
        notify_core_generation_published(data_root, &job, generation_published);
    }
    let failed = run.failed || terminal_persistence_pending;
    let state = daemon_core_cycle_state(&job);
    if !failed && job.get("status").and_then(Value::as_str) == Some("completed") {
        if let Some(generation) = job.get("published_generation").and_then(Value::as_str) {
            runtime.sidecar_drain.generation = Some(generation.to_owned());
        }
        let iteration = with_provider_refresh(
            DaemonIteration::new(run.did_work, false, state),
            &job,
            coordinator.has_pending_request(),
            terminal_persistence_pending,
            observation,
        );
        return Ok(immediate_follow_up(iteration));
    }
    Ok(with_provider_refresh(
        DaemonIteration::new(run.did_work, failed, state),
        &job,
        coordinator.has_pending_request(),
        terminal_persistence_pending,
        observation,
    ))
}

fn daemon_mode_runs_core_semantic_projection(mode: crate::config::DaemonMode) -> bool {
    !mode.runs_only_source_refresh()
}

fn finish_core_refresh<N>(
    data_root: &Path,
    runtime: &mut DaemonRuntime,
    coordinator: Option<&CoreRefreshEngine>,
    job: Value,
    did_work: bool,
    generation_published: &N,
) -> Result<DaemonIteration>
where
    N: crate::CoreGenerationPublishedPort + ?Sized,
{
    let job = record_daemon_job_retry(&mut runtime.history_retry, job);
    let job = persist_core_scheduler_status(data_root, coordinator, job)?;
    notify_core_generation_published(data_root, &job, generation_published);
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

fn notify_core_generation_published<N>(data_root: &Path, job: &Value, port: &N)
where
    N: crate::CoreGenerationPublishedPort + ?Sized,
{
    let Some(publication) = crate::CoreGenerationPublished::from_job(job) else {
        return;
    };
    // Core is already durable at this point. Notification is a publication
    // seam, not a second commit phase, so delivery errors are intentionally
    // quarantined and never become daemon retry state.
    let _ = port.notify(data_root, &publication);
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
    runtime.semantic_retry.consecutive_failures > 0 && runtime.semantic_retry.ready()
}

pub(super) fn daemon_retry_due(runtime: &DaemonRuntime) -> bool {
    (runtime.history_retry.consecutive_failures > 0 && runtime.history_retry.ready())
        || daemon_consumer_retry_due(runtime)
}

pub(super) fn daemon_scheduled_refresh_due(
    source_refresh: Option<&CoreRefreshEngine>,
    route_now_ms: u64,
) -> bool {
    source_refresh.and_then(|refresh| refresh.next_dirty_route_due_in_ms(route_now_ms)) == Some(0)
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
    data_root: &Path,
    runtime: &mut DaemonRuntime,
    deadline: Option<Instant>,
    semantic_enabled: bool,
    core_generation_id: Option<&str>,
    ports: DaemonSemanticJobPorts<'_>,
) -> Value {
    let intensity = runtime
        .semantic_intensity_leases
        .snapshot(runtime.config.semantic_indexing_intensity());
    if let Some(job) = runtime.semantic_blocked_job.as_ref() {
        return annotate_semantic_indexing_intensity(job.clone(), intensity);
    }
    if !runtime.semantic_retry.ready() {
        let job = daemon_semantic_retry_backoff_job(data_root, &runtime.semantic_retry);
        return annotate_semantic_indexing_intensity(
            bind_semantic_generation(job, core_generation_id),
            intensity,
        );
    }
    let job = run_daemon_semantic_job(
        data_root,
        runtime,
        deadline,
        semantic_enabled,
        ports.artifact_fetcher,
        ports.config,
    )
    .unwrap_or_else(|error| daemon_semantic_failed_job(data_root, error));
    let job = bind_semantic_generation(job, core_generation_id);
    let current_intensity = runtime
        .semantic_intensity_leases
        .snapshot(runtime.config.semantic_indexing_intensity());
    let job = annotate_semantic_indexing_intensity(job, current_intensity);
    let job = record_daemon_job_retry(&mut runtime.semantic_retry, job);
    if semantic_failure_class_from_job(&job).is_some_and(SemanticFailureClass::blocks_until_restart)
    {
        runtime.semantic_blocked_job = Some(job.clone());
    }
    job
}

fn bind_semantic_generation(mut job: Value, core_generation_id: Option<&str>) -> Value {
    if let Ok(fingerprint) = ctx_semantic_index::source_backed_semantic_contract_fingerprint() {
        job["source_contract_fingerprint"] = Value::String(fingerprint);
    }
    if let Some(core_generation_id) = core_generation_id {
        job["core_generation_id"] = Value::String(core_generation_id.to_owned());
    }
    job
}

fn semantic_generation_needs_catch_up(data_root: &Path, core_generation_id: &str) -> bool {
    let Ok(contract_fingerprint) =
        ctx_semantic_index::source_backed_semantic_contract_fingerprint()
    else {
        return true;
    };
    let Some(job) = read_daemon_job_status(&daemon_semantic_job_path(data_root)) else {
        return true;
    };
    job.get("core_generation_id").and_then(Value::as_str) != Some(core_generation_id)
        || job.get("status").and_then(Value::as_str) != Some("ready")
        || job
            .get("source_contract_fingerprint")
            .and_then(Value::as_str)
            != Some(contract_fingerprint.as_str())
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
    coordinator: &CoreRefreshEngine,
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
    let status = RefreshStatus::classify_schema_v1(&job)?;
    if status
        .terminal_outcome()
        .and_then(|outcome| outcome.retry_advice)
        == Some(RefreshRetryAdvice::RetryAdmission)
    {
        let request_id = job
            .get("request_id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("terminal retry-admission status has no request ID"))?;
        coordinator.complete_retry_admission_handoff(request_id)?;
    }
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
use ctx_history_refresh::{RefreshRetryAdvice, RefreshStatus};
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
        annotate_current_semantic_indexing_intensity, annotate_semantic_indexing_intensity,
        daemon_semantic_failed_job, daemon_semantic_retry_backoff_job, run_daemon_semantic_job,
    },
    paths_status::{
        daemon_core_refresh_job_path, daemon_semantic_job_path, read_daemon_job_status,
        write_daemon_job_status,
    },
    query_service::DaemonQueryActivity,
    runtime_limits::DAEMON_MIN_REMAINING_FOR_JOB_SECS,
    source_backed_refresh_coordinator::{pin_published_generation, CoreRefreshEngine},
};

#[cfg(test)]
#[path = "daemon_scheduler_tests.rs"]
mod tests;
