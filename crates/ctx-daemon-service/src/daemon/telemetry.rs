use std::{
    path::Path,
    time::{Duration as StdDuration, Instant},
};

use crate::{
    analytics::{
        count_bucket, DaemonBackoffV1, DaemonCycleFactsV1, DaemonCycleResultV1, DaemonCycleStateV1,
        DaemonRunFactsV1, DaemonRuntimeObservationV1, DaemonRuntimeSnapshotV1,
        DaemonStorageFactsV1, Outcome, PublicEventV1, RuntimeObservationV1,
    },
    DaemonObservationPort,
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

    pub(super) fn ready_events(
        &self,
        recovered: bool,
        now: Instant,
        storage: Option<DaemonStorageFactsV1>,
    ) -> Vec<PublicEventV1> {
        let elapsed = now.saturating_duration_since(self.started);
        let mut events = vec![runtime_event(
            DaemonRuntimeObservationV1::ready_with_storage(self.run, storage),
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

    pub(super) fn liveness_due(&self, now: Instant) -> bool {
        now >= self.next_liveness
    }

    pub(super) fn liveness_events(
        &mut self,
        now: Instant,
        storage: Option<DaemonStorageFactsV1>,
    ) -> Vec<PublicEventV1> {
        if now < self.next_liveness {
            return Vec::new();
        }
        let mut events = Vec::new();
        self.flush_pending_idle(&mut events);
        events.push(runtime_event(
            DaemonRuntimeObservationV1::liveness_with_storage(self.snapshot(), storage),
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

pub(super) fn deliver_active(
    observation: &dyn DaemonObservationPort,
    data_root: &Path,
    uploader_enabled: bool,
    events: &[PublicEventV1],
) {
    if events.is_empty() && !uploader_enabled {
        return;
    }
    if uploader_enabled {
        observation.append_and_upload(data_root, events);
    } else {
        observation.append(data_root, events);
    }
}

pub(super) fn append_terminal_events(
    observation: &dyn DaemonObservationPort,
    data_root: &Path,
    events: &[PublicEventV1],
) {
    if events.is_empty() {
        return;
    }
    observation.append(data_root, events);
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::super::super::analytics;

    use super::*;

    #[derive(Default)]
    struct RecordingObservation {
        deliveries: Mutex<Vec<(usize, bool)>>,
    }

    impl RecordingObservation {
        fn deliveries(&self) -> Vec<(usize, bool)> {
            self.deliveries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }
    }

    impl DaemonObservationPort for RecordingObservation {
        fn provider_refresh_event(
            &self,
            _job: &serde_json::Value,
            _successor_pending: bool,
        ) -> Option<PublicEventV1> {
            None
        }

        fn append(&self, _data_root: &Path, events: &[PublicEventV1]) {
            self.deliveries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push((events.len(), false));
        }

        fn append_and_upload(&self, _data_root: &Path, events: &[PublicEventV1]) {
            self.deliveries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push((events.len(), true));
        }
    }

    fn test_run() -> DaemonRunFactsV1 {
        DaemonRunFactsV1::new(
            analytics::DaemonStartModeV1::Manual,
            analytics::DaemonSupervisorV1::User,
            None,
        )
    }

    #[test]
    fn active_empty_tick_requests_upload() {
        let observation = RecordingObservation::default();

        deliver_active(&observation, Path::new("test-root"), true, &[]);

        assert_eq!(observation.deliveries(), [(0, true)]);
    }

    #[test]
    fn finite_worker_events_append_without_upload() {
        let observation = RecordingObservation::default();
        let started = Instant::now();
        let telemetry = DaemonTelemetry::new(test_run(), started, 0);
        let events = telemetry.ready_events(false, started, None);

        deliver_active(&observation, Path::new("test-root"), false, &events);

        assert_eq!(observation.deliveries(), [(1, false)]);
    }

    #[test]
    fn ready_and_liveness_include_storage_without_copying_it_to_recovery() {
        let started = Instant::now();
        let mut telemetry = DaemonTelemetry::new(test_run(), started, 0);
        let storage =
            analytics::DaemonStorageFactsV1::from_exact(Some((1024, 512)), Some((256, 128)));
        let events = telemetry.ready_events(true, started, storage);
        assert_eq!(events.len(), 2);
        let PublicEventV1::RuntimeObservation(ready) = &events[0] else {
            panic!("ready event has wrong family");
        };
        let mut ready_properties = serde_json::Map::new();
        ready.kind.insert_properties(&mut ready_properties);
        assert!(ready_properties.contains_key("filesystem_total_bytes_bucket"));
        let PublicEventV1::RuntimeObservation(recovered) = &events[1] else {
            panic!("recovered event has wrong family");
        };
        let mut recovered_properties = serde_json::Map::new();
        recovered.kind.insert_properties(&mut recovered_properties);
        assert!(!recovered_properties.contains_key("filesystem_total_bytes_bucket"));

        let due = started + DAEMON_LIVENESS_MIN_INTERVAL;
        let liveness = telemetry.liveness_events(due, storage);
        let PublicEventV1::RuntimeObservation(liveness) = &liveness[0] else {
            panic!("liveness event has wrong family");
        };
        let mut liveness_properties = serde_json::Map::new();
        liveness.kind.insert_properties(&mut liveness_properties);
        assert!(liveness_properties.contains_key("core_active_logical_bytes_bucket"));
    }

    #[test]
    fn fatal_and_stopped_events_append_without_upload() {
        let observation = RecordingObservation::default();
        let started = Instant::now();
        let mut fatal = DaemonTelemetry::new(test_run(), started, 0);
        let mut stopped = DaemonTelemetry::new(test_run(), started, 1);

        append_terminal_events(
            &observation,
            Path::new("test-root"),
            &fatal.fatal_events(started),
        );
        append_terminal_events(
            &observation,
            Path::new("test-root"),
            &stopped.stopped_events(false, started),
        );

        assert_eq!(observation.deliveries(), [(1, false), (1, false)]);
    }

    #[test]
    fn safety_reconciliation_admits_a_sixty_minute_hermes_deadline_within_eighty_minutes() {
        const MINUTE_MS: i64 = 60_000;
        let profile_source_descriptor = [4_u8; 32];
        let database_identity = [1_u8; 32];
        let schema_evidence = [2_u8; 32];
        let physical_revision = [3_u8; 32];
        let control = serde_json::to_vec(&serde_json::json!({
            "kind": "hermes-route-control-v1",
            "version": 3,
            "parser_revision": "hermes-source-backed-v5-optional-admission",
            "profile_source_descriptor": profile_source_descriptor,
            "database_identity": database_identity,
            "physical_revision": physical_revision,
            "schema_evidence": schema_evidence,
            "session_rowid": 4,
            "message_rowid": 9,
            "last_successful_exhaustive_at_ms": 0,
            "exact_due_at_ms": 60 * MINUTE_MS,
            "exhaustive_sequence": 1,
            "mode": "exhaustive",
            "outcome": "successful",
        }))
        .unwrap();
        let mut fake_now_ms = 0_i64;

        assert_eq!(
            ctx_history_capture::hermes_route_control_exact_due(&control, fake_now_ms),
            Some(false)
        );
        for sequence in 0_u64..8 {
            let interval = daemon_safety_reconcile_interval(sequence);
            assert!(interval >= StdDuration::from_secs(15 * 60));
            assert!(interval < StdDuration::from_secs(20 * 60));
            fake_now_ms += i64::try_from(interval.as_millis()).unwrap();
            if ctx_history_capture::hermes_route_control_exact_due(&control, fake_now_ms)
                == Some(true)
            {
                break;
            }
        }

        assert!(fake_now_ms >= 60 * MINUTE_MS);
        assert!(fake_now_ms < 80 * MINUTE_MS);
    }
}
