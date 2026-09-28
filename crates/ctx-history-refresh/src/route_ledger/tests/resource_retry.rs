use super::*;

#[test]
fn resource_failures_keep_backoff_across_events_during_and_between_attempts() {
    let mut ledger = DirtySourceRoutes::default();
    let route = route(29);
    ledger.record_event(route.clone(), watermark(1, 1), 0);
    let mut now = 250;
    for (index, delay) in [10_000, 20_000, 40_000, 80_000, 160_000, 300_000, 300_000]
        .into_iter()
        .enumerate()
    {
        let admission = ledger.admit_next(now).unwrap();
        let sequence = 2 + index as u64 * 2;
        ledger.record_event(route.clone(), watermark(1, sequence), now + 1);
        assert_eq!(ledger.resource_failure(&admission, now + 2), Some(delay));
        let due = now + 2 + delay;
        ledger.record_event(route.clone(), watermark(1, sequence + 1), now + 3);
        assert_eq!(ledger.next_due_at_ms(), Some(due));
        assert!(ledger.admit_next(due - 1).is_none());
        now = due;
    }
    let recovered = ledger.admit_next(now).unwrap();
    assert!(ledger.acknowledge(&recovered));
    ledger.record_event(route, watermark(1, 99), now + 1);
    let next = ledger.admit_next(now + 251).unwrap();
    assert_eq!(ledger.resource_failure(&next, now + 252), Some(10_000));
}
