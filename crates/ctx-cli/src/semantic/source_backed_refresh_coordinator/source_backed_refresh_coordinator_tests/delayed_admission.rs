use super::logical_demand::verified_publication_with_successful_routes;
use super::*;

#[test]
fn delayed_real_route_fence_after_predecessor_publication_stays_logical() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let route = route_identity(0x62);
    let directory_route = route_identity(0x63);
    let routes = BTreeSet::from([route.clone(), directory_route.clone()]);
    let route_observation = format!("{:02x}", 0xa2).repeat(32);
    let publication_observations = BTreeMap::from([(route.clone(), route_observation.clone())]);
    let executor_observations = publication_observations.clone();
    let executor_routes = routes.clone();
    let execution_entered = Arc::new(Barrier::new(2));
    let execution_release = Arc::new(Barrier::new(2));
    let executor_entered = Arc::clone(&execution_entered);
    let executor_release = Arc::clone(&execution_release);
    let executions = Arc::new(AtomicUsize::new(0));
    let observed_executions = Arc::clone(&executions);
    let executor = Arc::new(move |execution: SourceBackedRefreshExecution<'_>| {
        assert_eq!(observed_executions.fetch_add(1, Ordering::SeqCst), 0);
        executor_entered.wait();
        executor_release.wait();
        verified_publication_with_successful_routes(
            &execution,
            &executor_observations,
            &executor_routes,
        )
    });
    let fence_entered = Arc::new(Barrier::new(2));
    let fence_release = Arc::new(Barrier::new(2));
    let admission_entered = Arc::clone(&fence_entered);
    let admission_release = Arc::clone(&fence_release);
    let fence_route = route.clone();
    let fence_observation = route_observation.clone();
    let admission_fence = Arc::new(
        move |_data_root: &Path, _catalog: Option<&ExplicitSourceCatalogAuthority>| {
            admission_entered.wait();
            admission_release.wait();
            Ok(BTreeMap::from([
                (fence_route.clone(), Some(fence_observation.clone())),
                (directory_route.clone(), None),
            ]))
        },
    );
    let coordinator = Arc::new(CoreRefreshEngine::with_runtime_for_test(
        executor,
        admission_fence,
        Arc::new(crate::semantic::paths_status::write_daemon_job_status),
    ));
    coordinator.initialize_watch_route_authority(routes.clone());
    coordinator.schedule_startup_route_reconciliation(
        routes,
        EventWatermark::new(0, 1),
        ledger_now_ms(),
    );
    let periodic = coordinator.enqueue_periodic(&data_root).unwrap();
    let periodic_id = request_id(&periodic);
    let manual_id = Uuid::from_u128(0x29403).to_string();
    let manual_request = json!({
        "schema_version": 1,
        "op": SOURCE_REFRESH_REQUEST_OP,
        "request_id": manual_id,
        "mode": "wait",
        "operation": "refresh",
        "fresh_after_admitted_snapshot": true,
    });

    std::thread::scope(|scope| {
        let running = Arc::clone(&coordinator);
        let running_root = data_root.clone();
        let predecessor = scope.spawn(move || running.run_next(&running_root));
        execution_entered.wait();
        let admitted = coordinator
            .handle_listener_ipc_request(&data_root, &manual_request)
            .unwrap()
            .expect("manual All admission acknowledgement");
        assert_eq!(admitted["request_state"], "admission_pending");
        assert_eq!(admitted["coalesced_into_request_id"], periodic_id);
        coordinator.finish_listener_admission_response(&manual_id);

        let planning = Arc::clone(&coordinator);
        let planning_root = data_root.clone();
        let planner = scope.spawn(move || planning.prepare_next_pending_admission(&planning_root));
        fence_entered.wait();
        execution_release.wait();
        let predecessor = predecessor
            .join()
            .unwrap()
            .expect("verified periodic predecessor");
        assert!(!predecessor.failed, "{:#}", predecessor.job);
        fence_release.wait();
        assert!(planner.join().unwrap().unwrap());
    });

    assert!(coordinator.logical_continuation_is_fully_covered_for_test(&manual_id));
    let sampled_route = route;
    let sampled_observation = route_observation;
    let logical = coordinator
        .run_next_with_post_publication_sampler_for_test(&data_root, move |_| {
            Ok(BTreeMap::from([(sampled_route, Some(sampled_observation))]))
        })
        .expect("covered logical demand resolution");
    assert_eq!(request_id(&logical.job), manual_id);
    assert_eq!(logical.job["scanned_routes"], 0);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert!(!coordinator.has_pending_request());
}
