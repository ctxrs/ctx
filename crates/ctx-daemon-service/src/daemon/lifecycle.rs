use super::*;

const FINITE_WORKER_REQUEST_TIMEOUT: StdDuration = StdDuration::from_secs(5);
const FINITE_WORKER_QUIET_GRACE: StdDuration = StdDuration::from_millis(300);

/// Bounds one finite Core worker around one explicit IPC refresh demand.
#[derive(Debug)]
pub(super) struct FiniteCoreWorkerExit {
    started_at: Instant,
    demand_observed: bool,
    idle_since: Option<Instant>,
    observed_ipc_activity_generation: u64,
    observed_request_activity_generation: u64,
}

impl FiniteCoreWorkerExit {
    pub(super) fn new(refresh_service: Option<&DaemonQueryService>) -> Self {
        Self {
            started_at: Instant::now(),
            demand_observed: false,
            idle_since: None,
            observed_ipc_activity_generation: refresh_service
                .map(|service| service.activity.snapshot().1)
                .unwrap_or(0),
            // The engine is process-local and starts at zero. Preserve any
            // request admitted during startup so the first loop observation
            // still recognizes it even if the request already completed.
            observed_request_activity_generation: 0,
        }
    }

    pub(super) fn observe(
        &mut self,
        source_refresh: Option<&CoreRefreshEngine>,
        refresh_service: Option<&DaemonQueryService>,
        now: Instant,
    ) -> bool {
        let pending = source_refresh.is_some_and(CoreRefreshEngine::has_pending_request);
        let (active, ipc_generation) = refresh_service
            .map(|service| service.activity.snapshot())
            .unwrap_or((0, self.observed_ipc_activity_generation));
        let request_generation = source_refresh
            .map(CoreRefreshEngine::request_activity_generation)
            .unwrap_or(self.observed_request_activity_generation);
        self.observe_state(pending, active, ipc_generation, request_generation, now)
    }

    /// Atomically closes refresh admission once the finite worker is quiet,
    /// then immediately withdraws lifecycle readiness so a new explicit
    /// waiter starts or recovers a replacement instead of joining this owner.
    pub(super) fn begin_stopping(
        &mut self,
        source_refresh: Option<&CoreRefreshEngine>,
        refresh_service: Option<&DaemonQueryService>,
        lifecycle: &DaemonLifecycleState,
        now: Instant,
    ) -> bool {
        if !self.observe(source_refresh, refresh_service, now)
            || !refresh_service.is_some_and(|service| service.activity.begin_stopping_if_idle())
        {
            return false;
        }
        lifecycle.mark_stopping();
        true
    }

    fn observe_state(
        &mut self,
        pending: bool,
        active: usize,
        ipc_generation: u64,
        request_generation: u64,
        now: Instant,
    ) -> bool {
        let ipc_activity_changed = ipc_generation != self.observed_ipc_activity_generation;
        self.observed_ipc_activity_generation = ipc_generation;
        let request_activity_changed =
            request_generation != self.observed_request_activity_generation;
        self.observed_request_activity_generation = request_generation;
        if pending || request_activity_changed {
            self.demand_observed = true;
            self.idle_since = None;
            return false;
        }
        if !self.demand_observed {
            return now.saturating_duration_since(self.started_at) >= FINITE_WORKER_REQUEST_TIMEOUT;
        }
        if active > 0 || ipc_activity_changed {
            self.idle_since = None;
            return false;
        }
        let idle_since = self.idle_since.get_or_insert(now);
        now.saturating_duration_since(*idle_since) >= FINITE_WORKER_QUIET_GRACE
    }

    pub(super) fn wait_duration(&self, now: Instant) -> StdDuration {
        if self.demand_observed {
            self.idle_since.map_or(FINITE_WORKER_QUIET_GRACE, |idle| {
                FINITE_WORKER_QUIET_GRACE.saturating_sub(now.saturating_duration_since(idle))
            })
        } else {
            FINITE_WORKER_REQUEST_TIMEOUT
                .saturating_sub(now.saturating_duration_since(self.started_at))
        }
    }
}

pub(super) fn daemon_should_schedule_auto_upgrade(
    daemon_enabled: bool,
    daemon_mode: DaemonMode,
    automatic_upgrade_enabled: bool,
) -> bool {
    daemon_enabled && daemon_mode == DaemonMode::Full && automatic_upgrade_enabled
}

pub(super) fn daemon_automatic_recovery_allowed(
    config: &AppConfig,
    finite_core_worker: bool,
) -> bool {
    !finite_core_worker
        && daemon_should_schedule_auto_upgrade(
            config.daemon.enabled,
            config.daemon.mode,
            config.automatic_upgrade_enabled,
        )
}

#[cfg(any(test, feature = "test-support"))]
pub(super) fn fail_daemon_before_ready_for_test(data_root: &Path) -> Result<()> {
    if data_root
        .join(".fail-daemon-before-ready-for-test")
        .exists()
    {
        return Err(anyhow!("injected daemon failure before readiness"));
    }
    Ok(())
}

pub(super) fn ensure_daemon_ipc_services_healthy(
    query_service: Option<&DaemonQueryService>,
    refresh_service: Option<&DaemonQueryService>,
) -> Result<()> {
    for service in [refresh_service, query_service].into_iter().flatten() {
        if service.listener_finished() {
            return Err(anyhow!(
                "daemon {} IPC listener exited unexpectedly",
                service.service_id().as_str()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracker(started_at: Instant) -> FiniteCoreWorkerExit {
        FiniteCoreWorkerExit {
            started_at,
            demand_observed: false,
            idle_since: None,
            observed_ipc_activity_generation: 0,
            observed_request_activity_generation: 0,
        }
    }

    #[test]
    fn exits_if_no_request_arrives_within_admission_timeout() {
        let started_at = Instant::now();
        let mut exit = tracker(started_at);

        assert!(!exit.observe_state(false, 0, 0, 0, started_at));
        assert!(exit.observe_state(false, 0, 0, 0, started_at + FINITE_WORKER_REQUEST_TIMEOUT));
    }

    #[test]
    fn exits_only_after_observed_demand_finishes_and_quiets() {
        let started_at = Instant::now();
        let mut exit = tracker(started_at);

        assert!(!exit.observe_state(true, 0, 1, 1, started_at));
        assert!(!exit.observe_state(false, 1, 2, 1, started_at + StdDuration::from_millis(1)));
        let idle = started_at + StdDuration::from_millis(2);
        assert!(!exit.observe_state(false, 0, 3, 1, idle));
        assert!(!exit.observe_state(false, 0, 3, 1, idle + FINITE_WORKER_QUIET_GRACE));
        assert!(exit.observe_state(
            false,
            0,
            3,
            1,
            idle + FINITE_WORKER_QUIET_GRACE + FINITE_WORKER_QUIET_GRACE,
        ));
    }

    #[test]
    fn completed_request_activity_is_still_observed_as_demand() {
        let started_at = Instant::now();
        let mut exit = tracker(started_at);

        assert!(!exit.observe_state(false, 0, 2, 1, started_at));
        assert!(exit.demand_observed);
        assert!(!exit.observe_state(false, 0, 2, 1, started_at + FINITE_WORKER_QUIET_GRACE));
        assert!(exit.observe_state(
            false,
            0,
            2,
            1,
            started_at + FINITE_WORKER_QUIET_GRACE + FINITE_WORKER_QUIET_GRACE,
        ));
    }

    #[test]
    fn queued_successor_prevents_retirement_after_first_completion() {
        let started_at = Instant::now();
        let mut exit = tracker(started_at);

        assert!(!exit.observe_state(true, 1, 1, 1, started_at));
        let after_first_completion = started_at + FINITE_WORKER_REQUEST_TIMEOUT;
        assert!(!exit.observe_state(true, 0, 2, 2, after_first_completion,));
        let successor_completed = after_first_completion + StdDuration::from_millis(1);
        assert!(!exit.observe_state(false, 0, 3, 2, successor_completed));
        let quiet_started = successor_completed + StdDuration::from_millis(1);
        assert!(!exit.observe_state(false, 0, 3, 2, quiet_started));
        assert!(exit.observe_state(false, 0, 3, 2, quiet_started + FINITE_WORKER_QUIET_GRACE,));
    }
}
