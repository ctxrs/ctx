use super::*;

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
fn steady_appends_bound_publication_admissions_and_preserve_freshness() {
    let mut ledger = DirtySourceRoutes::default();
    let route = route(50);
    let mut oldest_unpublished = None;
    let mut publications = Vec::new();
    for now in (0..=10_000).step_by(50) {
        if now % 500 == 0 {
            ledger.record_event(route.clone(), watermark(1, now + 1), now);
            oldest_unpublished.get_or_insert(now);
        }
        if let Some(admitted) = ledger.admit_next(now) {
            assert!(now - oldest_unpublished.take().unwrap() <= 2_000);
            assert!(ledger.acknowledge(&admitted));
            publications.push(now);
        }
    }
    assert_eq!(publications, [250, 2_250, 4_250, 6_250, 8_250]);
    let last = ledger.admit_next(10_250).unwrap();
    assert!(ledger.acknowledge(&last));
    assert!(ledger.is_empty());
    // A new explicit request still bypasses the automatic cadence.
    ledger.seed_exact_routes([route], watermark(1, 20_000), 10_300);
    assert!(ledger.admit_next(10_550).is_some());
}

#[test]
fn event_arriving_during_admission_remains_dirty_until_the_next_capture() {
    let mut ledger = DirtySourceRoutes::default();
    let route = route(6);

    ledger.record_event(route.clone(), watermark(1, 1), 0);
    let first = ledger.admit_next(250).unwrap();
    ledger.record_event(route.clone(), watermark(1, 2), 300);

    assert!(!ledger.acknowledge(&first));
    assert_eq!(ledger.next_due_at_ms(), Some(2_250));
    assert!(ledger.admit_next(2_249).is_none());
    let second = ledger.admit_next(2_250).unwrap();
    assert_eq!(second.route(), &route);
    assert_eq!(second.watermark(), watermark(1, 2));
}

#[test]
fn safety_seed_bypasses_cadence_only_for_clean_peers() {
    let mut ledger = DirtySourceRoutes::default();
    let route = route(51);
    ledger.record_event(route.clone(), watermark(1, 1), 0);
    let first = ledger.admit_next(250).unwrap();
    assert!(ledger.acknowledge(&first));

    assert_eq!(
        ledger.seed_clean_exact_routes([route.clone()], watermark(1, 2), 300),
        1
    );
    let safety = ledger.admit_next(550).unwrap();
    assert!(ledger.acknowledge(&safety));

    ledger.record_event(route.clone(), watermark(1, 3), 600);
    assert_eq!(
        ledger.seed_clean_exact_routes([route], watermark(1, 3), 700),
        0
    );
    assert_eq!(ledger.next_due_at_ms(), Some(2_550));
}
