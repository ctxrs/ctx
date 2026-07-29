use std::{
    path::Path,
    time::{Duration as StdDuration, Instant},
};

use crate::{
    analytics::{
        self, count_bucket, DaemonBackoffV1, DaemonCycleFactsV1, DaemonCycleResultV1,
        DaemonCycleStateV1, DaemonRunFactsV1, DaemonRuntimeObservationV1, DaemonRuntimeSnapshotV1,
        Outcome, PublicEventV1, RuntimeObservationV1,
    },
    config::AppConfig,
};

use super::DaemonIteration;

pub(super) const DAEMON_LIVENESS_MIN_INTERVAL: StdDuration = StdDuration::from_secs(23 * 60 * 60);
pub(super) const DAEMON_LIVENESS_JITTER_WINDOW: StdDuration = StdDuration::from_secs(60 * 60);
const DAEMON_SAFETY_RECONCILE_MIN_INTERVAL: StdDuration = StdDuration::from_secs(15 * 60);
const DAEMON_SAFETY_RECONCILE_JITTER_WINDOW: StdDuration = StdDuration::from_secs(5 * 60);

#[derive(Debug)]
pub(super) struct DaemonTelemetry {
    run: DaemonRunFactsV1,
    started: Instant,
    next_liveness: Instant,
    jitter_seed: u64,
    liveness_sequence: u64,
    current_state: DaemonCycleStateV1,
    idle_state: Option<DaemonCycleStateV1>,
    pending_idle_cycles: u64,
    pending_idle_duration: StdDuration,
    failure_active: bool,
}

impl DaemonTelemetry {
    pub(super) fn new(run: DaemonRunFactsV1, started: Instant, jitter_seed: u64) -> Self {
        Self {
            run,
            started,
            next_liveness: started + daemon_liveness_interval(jitter_seed),
            jitter_seed,
            liveness_sequence: 1,
            current_state: DaemonCycleStateV1::unknown(),
            idle_state: None,
            pending_idle_cycles: 0,
            pending_idle_duration: StdDuration::ZERO,
            failure_active: false,
        }
    }

    pub(super) fn ready_events(&self, recovered: bool, now: Instant) -> Vec<PublicEventV1> {
        let elapsed = now.saturating_duration_since(self.started);
        let mut events = vec![runtime_event(
            DaemonRuntimeObservationV1::ready(self.run),
            Outcome::Success,
            elapsed,
        )];
        if recovered {
            events.push(runtime_event(
                DaemonRuntimeObservationV1::recovered(self.snapshot()),
                Outcome::Success,
                elapsed,
            ));
        }
        events
    }

    pub(super) fn observe_cycle(
        &mut self,
        iteration: &mut DaemonIteration,
        duration: StdDuration,
    ) -> Vec<PublicEventV1> {
        let mut events = std::mem::take(&mut iteration.provider_refresh_events);
        let state = iteration.telemetry_state;
        self.current_state = state;
        let result = if iteration.failed {
            DaemonCycleResultV1::Failure
        } else if iteration.did_work {
            DaemonCycleResultV1::Work
        } else {
            DaemonCycleResultV1::NoWork
        };

        if result == DaemonCycleResultV1::NoWork {
            match self.idle_state {
                None => {
                    events.push(self.cycle_event(result, 1, state, duration));
                    self.idle_state = Some(state);
                }
                Some(previous) if previous == state => {
                    self.pending_idle_cycles = self.pending_idle_cycles.saturating_add(1);
                    self.pending_idle_duration =
                        self.pending_idle_duration.saturating_add(duration);
                }
                Some(_) => {
                    self.flush_pending_idle(&mut events);
                    events.push(self.cycle_event(result, 1, state, duration));
                    self.idle_state = Some(state);
                }
            }
        } else {
            self.flush_pending_idle(&mut events);
            self.idle_state = None;
            events.push(self.cycle_event(result, 1, state, duration));
        }
        if iteration.failed && !self.failure_active {
            events.push(runtime_event(
                DaemonRuntimeObservationV1::failed(self.snapshot()),
                Outcome::Failure,
                duration,
            ));
            self.failure_active = true;
        } else if !iteration.failed
            && self.failure_active
            && state.retry_backoff() == DaemonBackoffV1::None
        {
            events.push(runtime_event(
                DaemonRuntimeObservationV1::recovered(self.snapshot()),
                Outcome::Success,
                duration,
            ));
            self.failure_active = false;
        }
        events
    }

    pub(super) fn liveness_events(&mut self, now: Instant) -> Vec<PublicEventV1> {
        if now < self.next_liveness {
            return Vec::new();
        }
        let mut events = Vec::new();
        self.flush_pending_idle(&mut events);
        events.push(runtime_event(
            DaemonRuntimeObservationV1::liveness(self.snapshot()),
            Outcome::Success,
            now.saturating_duration_since(self.started),
        ));
        let seed = self
            .jitter_seed
            .wrapping_add(self.liveness_sequence.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        self.liveness_sequence = self.liveness_sequence.saturating_add(1);
        self.next_liveness = now + daemon_liveness_interval(seed);
        events
    }

    pub(super) fn stopped_events(&mut self, failed: bool, now: Instant) -> Vec<PublicEventV1> {
        let mut events = Vec::new();
        self.flush_pending_idle(&mut events);
        events.push(runtime_event(
            DaemonRuntimeObservationV1::stopped(self.snapshot()),
            if failed {
                Outcome::Failure
            } else {
                Outcome::Success
            },
            now.saturating_duration_since(self.started),
        ));
        events
    }

    pub(super) fn fatal_events(&mut self, now: Instant) -> Vec<PublicEventV1> {
        let mut events = Vec::new();
        self.flush_pending_idle(&mut events);
        if !self.failure_active {
            events.push(runtime_event(
                DaemonRuntimeObservationV1::failed(self.snapshot()),
                Outcome::Failure,
                now.saturating_duration_since(self.started),
            ));
            self.failure_active = true;
        }
        events
    }

    fn flush_pending_idle(&mut self, events: &mut Vec<PublicEventV1>) {
        if self.pending_idle_cycles == 0 {
            return;
        }
        let state = self.idle_state.unwrap_or(self.current_state);
        events.push(self.cycle_event(
            DaemonCycleResultV1::NoWork,
            self.pending_idle_cycles,
            state,
            self.pending_idle_duration,
        ));
        self.pending_idle_cycles = 0;
        self.pending_idle_duration = StdDuration::ZERO;
    }

    fn cycle_event(
        &self,
        result: DaemonCycleResultV1,
        cycles: u64,
        state: DaemonCycleStateV1,
        duration: StdDuration,
    ) -> PublicEventV1 {
        runtime_event(
            DaemonRuntimeObservationV1::cycle(DaemonCycleFactsV1::new(
                self.run,
                result,
                count_bucket(cycles),
                state,
            )),
            if result == DaemonCycleResultV1::Failure {
                Outcome::Failure
            } else {
                Outcome::Success
            },
            duration,
        )
    }

    fn snapshot(&self) -> DaemonRuntimeSnapshotV1 {
        DaemonRuntimeSnapshotV1::new(self.run, self.current_state)
    }
}

pub(super) fn daemon_liveness_interval(seed: u64) -> StdDuration {
    let jitter_window_secs = DAEMON_LIVENESS_JITTER_WINDOW.as_secs();
    let jitter_secs = if jitter_window_secs == 0 {
        0
    } else {
        seed % jitter_window_secs
    };
    DAEMON_LIVENESS_MIN_INTERVAL + StdDuration::from_secs(jitter_secs)
}

pub(super) fn daemon_safety_reconcile_interval(seed: u64) -> StdDuration {
    let jitter_window_secs = DAEMON_SAFETY_RECONCILE_JITTER_WINDOW.as_secs();
    let jitter_secs = if jitter_window_secs == 0 {
        0
    } else {
        seed % jitter_window_secs
    };
    DAEMON_SAFETY_RECONCILE_MIN_INTERVAL + StdDuration::from_secs(jitter_secs)
}

pub(super) fn runtime_event(
    observation: DaemonRuntimeObservationV1,
    outcome: Outcome,
    duration: StdDuration,
) -> PublicEventV1 {
    PublicEventV1::RuntimeObservation(RuntimeObservationV1::daemon(observation, outcome, duration))
}

pub(super) fn send_daemon_events(data_root: &Path, events: &[PublicEventV1]) {
    if events.is_empty() {
        return;
    }
    let Some(config) = reload_daemon_analytics_config(data_root) else {
        return;
    };
    analytics::send_batch(data_root, &config, events);
}

pub(super) fn reload_daemon_analytics_config(data_root: &Path) -> Option<AppConfig> {
    let config = AppConfig::load(data_root).ok()?;
    config.analytics.enabled.then_some(config)
}
