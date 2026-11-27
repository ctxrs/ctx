//! `scheduled` indexing mode: the daemon runs without a filesystem watcher and
//! submits the same request as `ctx import --all --incremental` on a timer.

use std::{
    path::Path,
    time::{Duration, Instant},
};

use ctx_history_refresh::{RefreshRequest, RefreshSelection, SourceBackedReconciliationDemand};

use super::super::source_backed_refresh_coordinator::CoreRefreshEngine;

/// Retry delay when a tick finds other refresh work still queued.
const BUSY_RETRY: Duration = Duration::from_secs(60);

pub(super) struct ScheduledImport {
    next_due: Instant,
    /// The first refresh after the daemon starts authenticates existing
    /// history exhaustively, mirroring the startup boundary of `auto` mode.
    startup_pending: bool,
}

impl ScheduledImport {
    pub(super) const fn new(now: Instant) -> Self {
        Self {
            next_due: now,
            startup_pending: true,
        }
    }

    /// Submits one refresh of all history when the interval has elapsed and
    /// returns the submitted request id.
    pub(super) fn submit_if_due(
        &mut self,
        data_root: &Path,
        interval: Option<Duration>,
        engine: Option<&CoreRefreshEngine>,
        now: Instant,
    ) -> Option<String> {
        let (Some(interval), Some(engine)) = (interval, engine) else {
            return None;
        };
        if now < self.next_due {
            return None;
        }
        if engine.has_pending_request() {
            self.next_due = now + BUSY_RETRY.min(interval);
            return None;
        }
        let demand = if self.startup_pending {
            SourceBackedReconciliationDemand::Exhaustive
        } else {
            SourceBackedReconciliationDemand::Incremental
        };
        let request_id = uuid::Uuid::now_v7().to_string();
        let request =
            RefreshRequest::selected_import(request_id.clone(), RefreshSelection::All, demand);
        match engine.submit(data_root, request) {
            Ok(admission) => {
                let (_status, barrier) = admission.into_parts();
                // An unreleased barrier would keep the request unclaimable.
                if let Some(barrier) = barrier {
                    barrier.release(engine);
                }
                self.startup_pending = false;
                self.next_due = now + interval;
                Some(request_id)
            }
            Err(_) => {
                self.next_due = now + BUSY_RETRY.min(interval);
                None
            }
        }
    }

    /// Bounds the daemon's idle wait so the next tick is not missed.
    pub(super) fn bound_wait(
        &self,
        wait_for: Duration,
        interval: Option<Duration>,
        now: Instant,
    ) -> Duration {
        match interval {
            Some(_) => wait_for.min(self.next_due.saturating_duration_since(now)),
            None => wait_for,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INTERVAL: Duration = Duration::from_secs(10 * 60);

    fn engine() -> CoreRefreshEngine {
        crate::source_backed_refresh_adapter::refresh_engine(&crate::test_support::CONFIG)
    }

    fn demand(engine: &CoreRefreshEngine, request_id: &str) -> String {
        engine
            .status_for_test(request_id)
            .and_then(|status| status["reconciliation_demand"].as_str().map(str::to_owned))
            .unwrap_or_default()
    }

    #[test]
    fn other_modes_never_submit_or_bound_the_wait() {
        let temp = tempfile::tempdir().unwrap();
        let engine = engine();
        let now = Instant::now();
        let mut schedule = ScheduledImport::new(now);

        assert_eq!(schedule.submit_if_due(temp.path(), None, Some(&engine), now), None);
        assert!(!engine.has_pending_request());
        let idle = Duration::from_secs(900);
        assert_eq!(schedule.bound_wait(idle, None, now), idle);
    }

    #[test]
    fn first_refresh_is_exhaustive_then_waits_for_the_interval() {
        let temp = tempfile::tempdir().unwrap();
        let engine = engine();
        let now = Instant::now();
        let mut schedule = ScheduledImport::new(now);

        let first = schedule
            .submit_if_due(temp.path(), Some(INTERVAL), Some(&engine), now)
            .expect("the startup refresh is due immediately");
        assert!(engine.has_pending_request());
        assert_eq!(demand(&engine, &first), "exhaustive");
        assert_eq!(
            schedule.bound_wait(Duration::from_secs(3600), Some(INTERVAL), now),
            INTERVAL
        );
        assert_eq!(
            schedule.submit_if_due(temp.path(), Some(INTERVAL), Some(&engine), now),
            None,
            "nothing is due before the interval elapses"
        );
    }

    #[test]
    fn queued_work_defers_the_tick_instead_of_stacking_requests() {
        let temp = tempfile::tempdir().unwrap();
        let engine = engine();
        let now = Instant::now();
        let mut schedule = ScheduledImport::new(now);
        schedule
            .submit_if_due(temp.path(), Some(INTERVAL), Some(&engine), now)
            .unwrap();

        let later = now + INTERVAL;
        assert_eq!(
            schedule.submit_if_due(temp.path(), Some(INTERVAL), Some(&engine), later),
            None
        );
        assert_eq!(
            schedule.bound_wait(Duration::from_secs(3600), Some(INTERVAL), later),
            BUSY_RETRY
        );
    }

    #[test]
    fn later_refreshes_are_incremental() {
        let temp = tempfile::tempdir().unwrap();
        let now = Instant::now();
        let mut schedule = ScheduledImport::new(now);
        schedule.startup_pending = false;
        let engine = engine();

        let request = schedule
            .submit_if_due(temp.path(), Some(INTERVAL), Some(&engine), now)
            .unwrap();
        assert_eq!(demand(&engine, &request), "incremental");
    }
}
