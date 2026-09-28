use super::*;

#[test]
fn disk_failure_does_not_retry_immediately_when_capture_observes_a_new_watch_event() {
    let owner = Arc::new(Mutex::new(
        None::<(std::sync::Weak<CoreRefreshEngine>, SourceRouteIdentity)>,
    ));
    let execution_owner = Arc::clone(&owner);
    let executor = Arc::new(move |_: SourceBackedRefreshExecution<'_>| {
        let (engine, route) = execution_owner.lock().unwrap().clone().unwrap();
        engine
            .upgrade()
            .unwrap()
            .record_watch_routes([(route, EventWatermark::new(41, 1))], ledger_now_ms());
        Err(IndexError::CurrentRepublishInsufficientHeadroom {
            required: 1024,
            available: 512,
        }
        .into())
    });
    let (_temp, root, _source, engine, _catalog, route, generation) =
        automatic_retry_fixture(executor);
    let engine = Arc::new(engine);
    *owner.lock().unwrap() = Some((Arc::downgrade(&engine), route));
    assert!(engine
        .enqueue_next_dirty_route(&root, ledger_now_ms())
        .unwrap());
    let failure = engine.run_next(&root).unwrap();
    assert!(failure.failed);
    assert_eq!(failure.job["error_code"], "resource_unavailable");
    assert!(failure.job.get("automatic_retry").is_none());
    let delay = engine.next_dirty_route_due_in_ms(ledger_now_ms()).unwrap();
    assert!(
        delay > 2_000 && delay <= 10_000,
        "resource retry delay: {delay}"
    );
    assert!(!engine
        .enqueue_next_dirty_route(&root, ledger_now_ms())
        .unwrap());
    assert_eq!(
        VerifiedIndex::open_pinned(&source_backed_index_root(&root))
            .unwrap()
            .generation_id(),
        generation
    );
}
