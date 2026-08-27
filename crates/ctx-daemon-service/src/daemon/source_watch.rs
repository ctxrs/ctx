use std::{
    sync::Arc,
    time::{Duration as StdDuration, Instant},
};

use super::super::{
    daemon_wakeup::{DaemonWakeup, SourceWatchBatch},
    source_backed_refresh_coordinator::CoreRefreshEngine,
};
use super::DaemonRuntime;

/// Scheduler callers borrow the stable owner retained outside the daemon loop;
/// selecting an active refresh engine must not clone its `Arc` per cycle.
pub(super) fn daemon_scheduler_source_refresh(
    source_refresh_coordinator: &Option<Arc<CoreRefreshEngine>>,
) -> Option<&CoreRefreshEngine> {
    source_refresh_coordinator.as_deref()
}

pub(super) fn install_source_watch_ingress(
    wakeup: &DaemonWakeup,
    source_refresh: Option<&Arc<CoreRefreshEngine>>,
) {
    if wakeup.has_source_watch_sink() {
        return;
    }
    let Some(source_refresh) = source_refresh.cloned() else {
        return;
    };
    let pressure_refresh = Arc::clone(&source_refresh);
    wakeup.install_source_watch_pressure_sink(Arc::new(move |watermark| {
        pressure_refresh.fence_watch_uncertainty(watermark);
    }));
    wakeup.install_source_watch_sink(Arc::new(move |batch: &SourceWatchBatch| {
        if let Some(watermark) = batch.reconcile {
            source_refresh.fence_watch_uncertainty(watermark);
            return;
        }
        let observed_at_ms = source_route_ledger_now_ms();
        source_refresh.record_watch_routes_with_members(
            batch
                .routes
                .iter()
                .map(|(route, watermark)| (route.clone(), *watermark)),
            batch.members.clone(),
            observed_at_ms,
        );
    }));
}

pub(super) fn source_route_ledger_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

pub(crate) fn daemon_wait_duration(
    runtime: &DaemonRuntime,
    source_refresh: Option<&CoreRefreshEngine>,
    next_safety_reconcile: Instant,
    now: Instant,
) -> StdDuration {
    let pending_source_refresh = source_refresh.is_some_and(CoreRefreshEngine::has_pending_request);
    let mut wait_for = next_safety_reconcile.saturating_duration_since(now);
    if pending_source_refresh {
        if runtime.history_retry.ready()
            && source_refresh.is_none_or(|refresh| !refresh.watch_uncertainty_pending())
        {
            return StdDuration::ZERO;
        }
        // A retained Core request owns the scheduler while its control-plane
        // retry is backed off. The scheduler returns before dirty routes and
        // every optional consumer/sidecar, so only the owning retry deadline
        // can usefully wake it in this state.
        if let Some(retry_after_ms) = runtime.history_retry.retry_after_ms() {
            wait_for = wait_for.min(StdDuration::from_millis(retry_after_ms));
        }
        return wait_for;
    }
    if let Some(remaining) = runtime.consumer_retry_deferral.remaining(now) {
        wait_for = wait_for.min(remaining);
    } else {
        if let Some(retry_after_ms) = runtime.history_retry.retry_after_ms() {
            wait_for = wait_for.min(StdDuration::from_millis(retry_after_ms));
        }
        if let Some(retry_after_ms) = runtime.semantic_retry.retry_after_ms() {
            wait_for = wait_for.min(StdDuration::from_millis(retry_after_ms));
        }
    }
    if let Some(route_due_ms) = source_refresh
        .and_then(|refresh| refresh.next_dirty_route_due_in_ms(source_route_ledger_now_ms()))
    {
        let route_wait = StdDuration::from_millis(route_due_ms);
        wait_for = wait_for.min(route_wait);
    }
    wait_for
}
