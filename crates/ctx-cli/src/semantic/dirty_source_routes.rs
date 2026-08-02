use std::collections::BTreeMap;

use ctx_history_index::SourceRouteIdentity;

const DEBOUNCE_MS: u64 = 250;
const MAX_EVENT_LATENCY_MS: u64 = 2_000;
const RETRY_BASE_MS: u64 = 10_000;
const RETRY_MAX_MS: u64 = 5 * 60 * 1_000;

/// Monotonic position in one watcher's event stream.
///
/// A new watcher epoch supersedes every sequence from an older epoch. Within
/// an epoch, only a strictly greater sequence represents new work.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct EventWatermark {
    pub(super) watcher_epoch: u64,
    pub(super) sequence: u64,
}

impl EventWatermark {
    pub(super) const fn new(watcher_epoch: u64, sequence: u64) -> Self {
        Self {
            watcher_epoch,
            sequence,
        }
    }
}

/// One exact route admitted for future capture and publication verification.
///
/// The private revision and admission identifier bind a completion to the
/// precise dirty observation admitted by the ledger. A caller may inspect the
/// exact route and event watermark, but cannot manufacture a valid admission.
#[derive(Debug)]
pub(super) struct DirtySourceRouteAdmission {
    route: SourceRouteIdentity,
    watermark: EventWatermark,
    dirty_revision: u64,
    admission_id: u64,
}

impl DirtySourceRouteAdmission {
    pub(super) fn route(&self) -> &SourceRouteIdentity {
        &self.route
    }

    pub(super) fn watermark(&self) -> EventWatermark {
        self.watermark
    }
}

#[derive(Debug)]
struct InFlightAdmission {
    dirty_revision: u64,
    admission_id: u64,
}

#[derive(Debug)]
struct DirtyRouteState {
    dirty_revision: u64,
    dirty_order: u64,
    first_event_at_ms: u64,
    last_event_at_ms: u64,
    consecutive_retry_failures: u32,
    retry_not_before_ms: Option<u64>,
    permanently_blocked: bool,
    in_flight: Option<InFlightAdmission>,
}

impl DirtyRouteState {
    fn new(dirty_revision: u64, dirty_order: u64, observed_at_ms: u64) -> Self {
        Self {
            dirty_revision,
            dirty_order,
            first_event_at_ms: observed_at_ms,
            last_event_at_ms: observed_at_ms,
            consecutive_retry_failures: 0,
            retry_not_before_ms: None,
            permanently_blocked: false,
            in_flight: None,
        }
    }

    fn due_at_ms(&self) -> u64 {
        let debounce_due = self
            .last_event_at_ms
            .saturating_add(DEBOUNCE_MS)
            .min(self.first_event_at_ms.saturating_add(MAX_EVENT_LATENCY_MS));
        self.retry_not_before_ms.unwrap_or(0).max(debounce_due)
    }

    fn reset_retry(&mut self) {
        self.consecutive_retry_failures = 0;
        self.retry_not_before_ms = None;
    }
}

/// Daemon-owned dirty-route state with no route discovery or filesystem work.
#[derive(Debug, Default)]
pub(super) struct DirtySourceRoutes {
    // Watcher epochs are ledger-wide: once any event or explicit seed advances
    // the epoch, delayed events from older watcher instances are stale for
    // every route, including routes the ledger has not seen before.
    current_watcher_epoch: Option<u64>,
    // Acknowledged watermarks remain here so duplicate or out-of-order watcher
    // delivery cannot recreate work after a route becomes clean.
    watermarks: BTreeMap<SourceRouteIdentity, EventWatermark>,
    dirty: BTreeMap<SourceRouteIdentity, DirtyRouteState>,
    next_dirty_revision: u64,
    next_dirty_order: u64,
    next_admission_id: u64,
}

impl DirtySourceRoutes {
    pub(super) fn is_empty(&self) -> bool {
        self.dirty.is_empty()
    }

    pub(super) fn len(&self) -> usize {
        self.dirty.len()
    }

    /// Records one exact-route watcher event if its watermark is strictly new.
    pub(super) fn record_event(
        &mut self,
        route: SourceRouteIdentity,
        watermark: EventWatermark,
        observed_at_ms: u64,
    ) -> bool {
        if self
            .current_watcher_epoch
            .is_some_and(|current| watermark.watcher_epoch < current)
        {
            return false;
        }
        self.current_watcher_epoch = Some(
            self.current_watcher_epoch
                .unwrap_or(watermark.watcher_epoch)
                .max(watermark.watcher_epoch),
        );
        if self
            .watermarks
            .get(&route)
            .is_some_and(|current| watermark <= *current)
        {
            return false;
        }
        self.watermarks.insert(route.clone(), watermark);
        self.mark_dirty(route, observed_at_ms);
        true
    }

    /// Marks only the provided exact routes dirty.
    ///
    /// The caller owns catalog, restart, overflow, and manual route selection;
    /// this ledger neither discovers routes nor derives identities. A seed is
    /// an explicit new dirty observation even when its supplied watermark was
    /// already seen, so it also reactivates a permanently blocked route.
    pub(super) fn seed_exact_routes<I>(
        &mut self,
        routes: I,
        watermark: EventWatermark,
        observed_at_ms: u64,
    ) where
        I: IntoIterator<Item = SourceRouteIdentity>,
    {
        self.current_watcher_epoch = Some(
            self.current_watcher_epoch
                .unwrap_or(watermark.watcher_epoch)
                .max(watermark.watcher_epoch),
        );
        for route in routes {
            self.watermarks
                .entry(route.clone())
                .and_modify(|current| *current = (*current).max(watermark))
                .or_insert(watermark);
            self.mark_dirty(route, observed_at_ms);
        }
    }

    /// Earliest time at which any non-blocked, non-admitted route can run.
    pub(super) fn next_due_at_ms(&self) -> Option<u64> {
        if self.dirty.is_empty() {
            return None;
        }
        self.dirty
            .values()
            .filter(|state| !state.permanently_blocked && state.in_flight.is_none())
            .map(DirtyRouteState::due_at_ms)
            .min()
    }

    /// Admits one eligible route, preferring the oldest dirty route first.
    pub(super) fn admit_next(&mut self, now_ms: u64) -> Option<DirtySourceRouteAdmission> {
        if self.dirty.is_empty() {
            return None;
        }
        let route = self
            .dirty
            .iter()
            .filter(|(_, state)| {
                !state.permanently_blocked
                    && state.in_flight.is_none()
                    && state.due_at_ms() <= now_ms
            })
            .min_by_key(|(route, state)| (state.first_event_at_ms, state.dirty_order, *route))
            .map(|(route, _)| route.clone())?;
        let watermark = self.watermarks.get(&route).copied()?;
        let admission_id = self.allocate_admission_id();
        let state = self.dirty.get_mut(&route)?;
        let dirty_revision = state.dirty_revision;
        state.in_flight = Some(InFlightAdmission {
            dirty_revision,
            admission_id,
        });
        Some(DirtySourceRouteAdmission {
            route,
            watermark,
            dirty_revision,
            admission_id,
        })
    }

    /// Acknowledges the admitted watermark after publication or no-op proof.
    ///
    /// Returns true only when that exact admitted observation became clean. A
    /// newer event or seed remains dirty and a stale admission changes nothing.
    pub(super) fn acknowledge(&mut self, admission: &DirtySourceRouteAdmission) -> bool {
        let should_remove = {
            let Some(state) = self.dirty.get_mut(&admission.route) else {
                return false;
            };
            if !admission_matches(state, admission) {
                return false;
            }
            state.in_flight = None;
            state.reset_retry();
            state.dirty_revision == admission.dirty_revision
                && self.watermarks.get(&admission.route).copied() == Some(admission.watermark)
        };
        if should_remove {
            self.dirty.remove(&admission.route);
        }
        should_remove
    }

    /// Records a retryable failure and returns its bounded backoff delay.
    pub(super) fn retryable_failure(
        &mut self,
        admission: &DirtySourceRouteAdmission,
        now_ms: u64,
    ) -> Option<u64> {
        let state = self.dirty.get_mut(&admission.route)?;
        if !admission_matches(state, admission) {
            return None;
        }
        state.in_flight = None;
        state.consecutive_retry_failures = state.consecutive_retry_failures.saturating_add(1);
        let delay_ms = retry_delay_ms(state.consecutive_retry_failures);
        state.retry_not_before_ms = Some(now_ms.saturating_add(delay_ms));
        Some(delay_ms)
    }

    /// Blocks the admitted observation until a newer event or explicit seed.
    ///
    /// If a trigger arrived during the admitted work, it is already a newer
    /// dirty observation and is deliberately not blocked by this stale result.
    pub(super) fn permanent_failure(&mut self, admission: &DirtySourceRouteAdmission) -> bool {
        let Some(state) = self.dirty.get_mut(&admission.route) else {
            return false;
        };
        if !admission_matches(state, admission) {
            return false;
        }
        state.in_flight = None;
        if state.dirty_revision != admission.dirty_revision {
            return false;
        }
        state.permanently_blocked = true;
        true
    }

    fn mark_dirty(&mut self, route: SourceRouteIdentity, observed_at_ms: u64) {
        let dirty_revision = self.allocate_dirty_revision();
        let needs_new_order = self
            .dirty
            .get(&route)
            .is_none_or(|state| state.permanently_blocked);
        let new_order = needs_new_order.then(|| self.allocate_dirty_order());
        let Some(state) = self.dirty.get_mut(&route) else {
            self.dirty.insert(
                route,
                DirtyRouteState::new(dirty_revision, new_order.unwrap_or(0), observed_at_ms),
            );
            return;
        };

        let starts_work_after_admission = state
            .in_flight
            .as_ref()
            .is_some_and(|in_flight| state.dirty_revision == in_flight.dirty_revision);
        state.dirty_revision = dirty_revision;
        if state.permanently_blocked {
            state.dirty_order = new_order.unwrap_or(state.dirty_order);
            state.first_event_at_ms = observed_at_ms;
            state.last_event_at_ms = observed_at_ms;
            state.permanently_blocked = false;
            state.reset_retry();
        } else if starts_work_after_admission {
            state.first_event_at_ms = observed_at_ms;
            state.last_event_at_ms = observed_at_ms;
        } else {
            state.last_event_at_ms = observed_at_ms;
        }
    }

    fn allocate_dirty_revision(&mut self) -> u64 {
        self.next_dirty_revision = self.next_dirty_revision.saturating_add(1);
        self.next_dirty_revision
    }

    fn allocate_dirty_order(&mut self) -> u64 {
        self.next_dirty_order = self.next_dirty_order.saturating_add(1);
        self.next_dirty_order
    }

    fn allocate_admission_id(&mut self) -> u64 {
        self.next_admission_id = self.next_admission_id.saturating_add(1);
        self.next_admission_id
    }
}

fn admission_matches(state: &DirtyRouteState, admission: &DirtySourceRouteAdmission) -> bool {
    state.in_flight.as_ref().is_some_and(|in_flight| {
        in_flight.dirty_revision == admission.dirty_revision
            && in_flight.admission_id == admission.admission_id
    })
}

fn retry_delay_ms(consecutive_failures: u32) -> u64 {
    let exponent = consecutive_failures.saturating_sub(1).min(63);
    RETRY_BASE_MS
        .checked_mul(1_u64 << exponent)
        .unwrap_or(RETRY_MAX_MS)
        .min(RETRY_MAX_MS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(byte: u8) -> SourceRouteIdentity {
        SourceRouteIdentity::from_sha256(format!("{byte:02x}").repeat(32)).unwrap()
    }

    fn watermark(watcher_epoch: u64, sequence: u64) -> EventWatermark {
        EventWatermark::new(watcher_epoch, sequence)
    }

    #[test]
    fn empty_ledger_is_idle_without_a_due_time() {
        let mut ledger = DirtySourceRoutes::default();

        assert!(ledger.is_empty());
        assert_eq!(ledger.len(), 0);
        assert_eq!(ledger.next_due_at_ms(), None);
        assert!(ledger.admit_next(u64::MAX).is_none());

        ledger.seed_exact_routes(Vec::new(), watermark(1, 0), 10);
        assert!(ledger.is_empty());
        assert_eq!(ledger.next_due_at_ms(), None);
    }

    #[test]
    fn debounce_moves_with_events_but_never_exceeds_first_event_max_latency() {
        let mut ledger = DirtySourceRoutes::default();
        let route = route(1);

        assert!(ledger.record_event(route.clone(), watermark(1, 1), 1_000));
        assert_eq!(ledger.next_due_at_ms(), Some(1_250));
        assert!(ledger.record_event(route.clone(), watermark(1, 2), 1_400));
        assert_eq!(ledger.next_due_at_ms(), Some(1_650));
        assert!(ledger.record_event(route.clone(), watermark(1, 3), 2_900));
        assert_eq!(ledger.next_due_at_ms(), Some(3_000));
        assert!(ledger.admit_next(2_999).is_none());
        assert_eq!(
            ledger
                .admit_next(3_000)
                .map(|admission| admission.watermark()),
            Some(watermark(1, 3))
        );
    }

    #[test]
    fn exact_route_events_coalesce_monotonically() {
        let mut ledger = DirtySourceRoutes::default();
        let route = route(2);

        assert!(ledger.record_event(route.clone(), watermark(7, 10), 100));
        assert!(!ledger.record_event(route.clone(), watermark(7, 10), 200));
        assert!(!ledger.record_event(route.clone(), watermark(7, 9), 300));
        assert!(!ledger.record_event(route.clone(), watermark(6, u64::MAX), 400));
        assert_eq!(ledger.next_due_at_ms(), Some(350));

        assert!(ledger.record_event(route.clone(), watermark(8, 0), 500));
        assert_eq!(ledger.next_due_at_ms(), Some(750));
        let admission = ledger.admit_next(750).unwrap();
        assert_eq!(admission.route(), &route);
        assert_eq!(admission.watermark(), watermark(8, 0));
        assert!(ledger.acknowledge(&admission));

        assert!(!ledger.record_event(route, watermark(7, u64::MAX), 1_000));
        assert!(ledger.is_empty());
    }

    #[test]
    fn watcher_epoch_advance_rejects_older_events_for_unseen_routes() {
        let mut ledger = DirtySourceRoutes::default();
        let seeded = route(15);
        let unseen = route(16);

        ledger.seed_exact_routes([seeded], watermark(2, 0), 100);

        assert!(!ledger.record_event(unseen.clone(), watermark(1, u64::MAX), 200));
        assert_eq!(ledger.len(), 1);
        assert!(ledger.record_event(unseen, watermark(2, 1), 200));
        assert_eq!(ledger.len(), 2);
    }

    #[test]
    fn admission_is_fair_and_oldest_first() {
        let mut ledger = DirtySourceRoutes::default();
        let oldest = route(3);
        let middle = route(4);
        let newest = route(5);

        ledger.record_event(newest.clone(), watermark(1, 3), 30);
        ledger.record_event(oldest.clone(), watermark(1, 1), 10);
        ledger.record_event(middle.clone(), watermark(1, 2), 20);

        assert_eq!(ledger.admit_next(1_000).unwrap().route(), &oldest);
        assert_eq!(ledger.admit_next(1_000).unwrap().route(), &middle);
        assert_eq!(ledger.admit_next(1_000).unwrap().route(), &newest);
        assert!(ledger.admit_next(1_000).is_none());
    }

    #[test]
    fn event_arriving_during_admission_remains_dirty_with_a_fresh_debounce() {
        let mut ledger = DirtySourceRoutes::default();
        let route = route(6);

        ledger.record_event(route.clone(), watermark(1, 1), 0);
        let first = ledger.admit_next(250).unwrap();
        ledger.record_event(route.clone(), watermark(1, 2), 300);

        assert!(!ledger.acknowledge(&first));
        assert_eq!(ledger.next_due_at_ms(), Some(550));
        assert!(ledger.admit_next(549).is_none());
        let second = ledger.admit_next(550).unwrap();
        assert_eq!(second.route(), &route);
        assert_eq!(second.watermark(), watermark(1, 2));
    }

    #[test]
    fn stale_acknowledgement_cannot_clear_a_newer_admission() {
        let mut ledger = DirtySourceRoutes::default();
        let route = route(7);

        ledger.record_event(route.clone(), watermark(1, 1), 0);
        let stale = ledger.admit_next(250).unwrap();
        ledger.record_event(route.clone(), watermark(1, 2), 300);
        assert!(!ledger.acknowledge(&stale));

        let current = ledger.admit_next(550).unwrap();
        assert!(!ledger.acknowledge(&stale));
        assert_eq!(ledger.len(), 1);
        assert!(ledger.acknowledge(&current));
        assert!(ledger.is_empty());
    }

    #[test]
    fn retryable_failures_back_off_exponentially_and_cap_at_five_minutes() {
        let mut ledger = DirtySourceRoutes::default();
        let route = route(8);
        let expected_delays = [10_000, 20_000, 40_000, 80_000, 160_000, 300_000, 300_000];

        ledger.record_event(route, watermark(1, 1), 0);
        let mut now_ms = 250;
        for expected_delay in expected_delays {
            let admission = ledger.admit_next(now_ms).unwrap();
            assert_eq!(
                ledger.retryable_failure(&admission, now_ms),
                Some(expected_delay)
            );
            let due_at_ms = now_ms + expected_delay;
            assert_eq!(ledger.next_due_at_ms(), Some(due_at_ms));
            assert!(ledger.admit_next(due_at_ms - 1).is_none());
            now_ms = due_at_ms;
        }
    }

    #[test]
    fn permanent_failure_requires_a_newer_event_or_an_explicit_seed() {
        let mut ledger = DirtySourceRoutes::default();
        let route = route(9);

        ledger.record_event(route.clone(), watermark(4, 10), 0);
        let first = ledger.admit_next(250).unwrap();
        assert!(ledger.permanent_failure(&first));
        assert_eq!(ledger.next_due_at_ms(), None);
        assert!(!ledger.record_event(route.clone(), watermark(4, 10), 500));
        assert!(!ledger.record_event(route.clone(), watermark(3, u64::MAX), 500));
        assert_eq!(ledger.next_due_at_ms(), None);

        assert!(ledger.record_event(route.clone(), watermark(4, 11), 1_000));
        assert_eq!(ledger.next_due_at_ms(), Some(1_250));
        let second = ledger.admit_next(1_250).unwrap();
        assert!(ledger.permanent_failure(&second));

        ledger.seed_exact_routes([route.clone()], watermark(4, 11), 2_000);
        assert_eq!(ledger.next_due_at_ms(), Some(2_250));
        let seeded = ledger.admit_next(2_250).unwrap();
        assert_eq!(seeded.route(), &route);
        assert_eq!(seeded.watermark(), watermark(4, 11));
    }

    #[test]
    fn restart_or_overflow_seed_marks_only_the_exact_provided_routes_dirty() {
        let mut ledger = DirtySourceRoutes::default();
        let first = route(10);
        let omitted = route(11);
        let second = route(12);

        ledger.seed_exact_routes([first.clone(), second.clone()], watermark(20, 0), 100);

        assert_eq!(ledger.len(), 2);
        let admitted_first = ledger.admit_next(350).unwrap();
        let admitted_second = ledger.admit_next(350).unwrap();
        assert_eq!(admitted_first.route(), &first);
        assert_eq!(admitted_second.route(), &second);
        assert_ne!(admitted_first.route(), &omitted);
        assert_ne!(admitted_second.route(), &omitted);
        assert!(ledger.admit_next(350).is_none());
    }

    #[test]
    fn route_failure_and_acknowledgement_state_is_independent() {
        let mut ledger = DirtySourceRoutes::default();
        let blocked = route(13);
        let healthy = route(14);

        ledger.record_event(blocked.clone(), watermark(1, 1), 0);
        ledger.record_event(healthy.clone(), watermark(1, 1), 0);
        let blocked_admission = ledger.admit_next(250).unwrap();
        let healthy_admission = ledger.admit_next(250).unwrap();
        assert_eq!(blocked_admission.route(), &blocked);
        assert_eq!(healthy_admission.route(), &healthy);

        assert!(ledger.permanent_failure(&blocked_admission));
        assert!(ledger.acknowledge(&healthy_admission));
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger.next_due_at_ms(), None);

        ledger.seed_exact_routes([blocked.clone()], watermark(1, 1), 1_000);
        let reactivated = ledger.admit_next(1_250).unwrap();
        assert_eq!(reactivated.route(), &blocked);
        assert!(ledger.acknowledge(&reactivated));
        assert!(ledger.is_empty());
    }
}
