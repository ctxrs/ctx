//! Request-coalescing coverage owned by the refresh engine.

use super::*;

#[test]
fn exact_import_overlay_upgrades_queued_route_work_without_rescan() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let coordinator = CoreRefreshEngine::new();
    let route = route_identity(0xa1);
    let observation = format!("{:02x}", 0xa3).repeat(32);
    coordinator.reconcile_watch_routes([route.clone()], EventWatermark::new(1, 0), 0);
    assert!(coordinator
        .enqueue_next_dirty_route(&data_root, u64::MAX)
        .unwrap());
    let scheduled = read_daemon_job_status(&daemon_source_backed_refresh_job_path(&data_root))
        .expect("scheduled exact-route job");
    let scheduled_request_id = request_id(&scheduled);
    assert_eq!(scheduled["trigger"], "periodic");

    let authority = test_catalog_authority(1, 0xa2);
    let request = compact_json(json!({
        "schema_version": 1,
        "op": SOURCE_REFRESH_REQUEST_OP,
        "request_id": Uuid::now_v7().to_string(),
        "mode": "wait",
        "operation": "import",
        "explicit_source_catalog": authority.to_json(),
        "fresh_after_admitted_snapshot": true,
    }));
    assert_eq!(request["operation"], "import");
    assert_eq!(request["explicit_source_catalog"], authority.to_json());
    assert_eq!(request["fresh_after_admitted_snapshot"], true);

    let upgraded = coordinator
        .handle_ipc_request_with_admission_fence_for_test(
            &data_root,
            &request,
            BTreeMap::from([(route.clone(), Some(observation.clone()))]),
        )
        .unwrap()
        .expect("exact-overlay import response");
    let attached = coordinator
        .handle_ipc_request_with_admission_fence_for_test(
            &data_root,
            &request,
            BTreeMap::from([(route.clone(), Some(observation.clone()))]),
        )
        .unwrap()
        .expect("equivalent import response");

    let logical_request_id = request_id(&upgraded);
    assert_eq!(logical_request_id, request["request_id"]);
    assert_eq!(request_id(&attached), logical_request_id);
    assert_eq!(attached["coalesced_requests"], 0);
    assert_eq!(attached["coalesced_into_request_id"], scheduled_request_id);
    let upgraded_physical = coordinator
        .status_for_test(&scheduled_request_id)
        .expect("upgraded physical request");
    assert_eq!(upgraded_physical["operation"], "import");
    assert_eq!(upgraded_physical["trigger"], "import");
    assert_eq!(
        upgraded_physical["trigger_provenance"],
        "explicit_source_catalog"
    );
    assert_eq!(upgraded_physical["coalesced_logical_demands"], 1);

    let writer_launches = AtomicUsize::new(0);
    let run = coordinator
        .run_next_with(
            |request_id, coordinator| {
                writer_launches.fetch_add(1, Ordering::SeqCst);
                coordinator
                    .admit_refresh_scope_for_test(request_id, &SourceBackedRefreshScope::All)
                    .unwrap();
                let mut publication = test_publication("implicit-import-generation");
                publication.published_explicit_source_catalog = Some(authority);
                publication
                    .route_results
                    .push(SourceBackedRefreshRouteResult::succeeded(
                        route.as_str().to_owned(),
                        true,
                    ));
                coordinator.set_route_observations_for_test(
                    request_id,
                    BTreeMap::from([(route.clone(), observation.clone())]),
                );
                Ok(publication)
            },
            || Ok(Some("implicit-import-generation".to_owned())),
            |_| Ok(()),
            |_| Ok(()),
        )
        .expect("upgraded all-route refresh");
    assert_eq!(run.scope, SourceBackedRefreshScope::All);
    assert_eq!(writer_launches.load(Ordering::SeqCst), 1);
    assert!(coordinator.logical_continuation_is_fully_covered_for_test(&logical_request_id));
    assert_eq!(writer_launches.load(Ordering::SeqCst), 1);
    assert!(coordinator.has_pending_request());
}

#[test]
fn duplicate_concurrent_requests_launch_one_writer() {
    const REQUESTS: usize = 16;

    let coordinator = Arc::new(CoreRefreshEngine::new());
    let barrier = Arc::new(Barrier::new(REQUESTS));
    let mut threads = Vec::new();
    for _ in 0..REQUESTS {
        let coordinator = coordinator.clone();
        let barrier = barrier.clone();
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            coordinator.enqueue(Some("generation-1".to_owned()))
        }));
    }
    let responses = threads
        .into_iter()
        .map(|thread| thread.join().expect("request thread"))
        .collect::<Vec<_>>();
    let expected_request_id = request_id(&responses[0]);
    assert!(responses
        .iter()
        .all(|response| request_id(response) == expected_request_id));

    let writer_launches = AtomicUsize::new(0);
    let run = coordinator
        .run_next_with(
            |request_id, coordinator| {
                writer_launches.fetch_add(1, Ordering::SeqCst);
                let _ = coordinator.set_progress(
                    request_id,
                    SourceBackedRefreshProgressUpdate {
                        phase: "refreshing".to_owned(),
                        completed_sources: 0,
                        total_sources: 1,
                        total_sources_known: true,
                        current_source: Some("source-a".to_owned()),
                        completed_records: Some(1),
                        completed_bytes: Some(128),
                        current_source_progress: None,
                    },
                );
                Ok(test_publication("generation-2"))
            },
            || Ok(Some("generation-2".to_owned())),
            |_| Ok(()),
            |_| Ok(()),
        )
        .expect("queued refresh");

    assert_eq!(writer_launches.load(Ordering::SeqCst), 1);
    assert!(run.did_work);
    assert!(!run.failed);
    let status = coordinator
        .status(&expected_request_id)
        .expect("published request status");
    assert_eq!(status["request_state"], "published");
    assert!(status["progress"].get("completed_records").is_none());
    assert!(status["progress"].get("completed_bytes").is_none());
    assert_eq!(status["published_generation"], "generation-2");
    assert_eq!(status["generation_changed"], true);
    assert_eq!(status["receipt"]["previous_generation"], "generation-1");
    assert_eq!(status["receipt"]["published_generation"], "generation-2");
    assert_eq!(status["receipt"]["generation_changed"], true);
    assert!(status.get("published_explicit_source_catalog").is_none());
    assert!(status["receipt"]
        .get("published_explicit_source_catalog")
        .is_none());
    assert_eq!(status["receipt"]["current"]["current_source_count"], 1);
    assert_eq!(status["receipt"]["current"]["current_indexed_documents"], 2);
    assert_eq!(status["receipt"]["current"]["current_rejected_records"], 1);
    assert_eq!(
        status["coalesced_requests"].as_u64(),
        Some((REQUESTS - 1) as u64)
    );
    assert_eq!(status["certified_source_count"], 1);
    assert_eq!(status["certified_source_bytes"], 128);
    assert_eq!(status["timings_us"]["discovery"], 11);
    assert_eq!(status["timings_us"]["scan_stage"], 22);
    assert_eq!(status["timings_us"]["commit"], 33);
    assert!(coordinator
        .run_next_with(
            |_, _| panic!("duplicate writer launched"),
            || Ok(Some("generation-2".to_owned())),
            |_| Ok(()),
            |_| Ok(()),
        )
        .is_none());
}

#[test]
fn unchanged_nonempty_publication_is_no_op_by_generation_identity() {
    let coordinator = CoreRefreshEngine::new();
    let request = coordinator.enqueue(Some("generation-1".to_owned()));
    let request_id = request_id(&request);
    let run = coordinator
        .run_next_with(
            |_, _| Ok(test_publication("generation-1")),
            || Ok(Some("generation-1".to_owned())),
            |_| Ok(()),
            |_| Ok(()),
        )
        .expect("queued refresh");

    assert!(!run.failed);
    assert!(!run.did_work);
    let status = coordinator.status(&request_id).expect("published request");
    assert_eq!(status["generation_changed"], false);
    assert_eq!(status["receipt"]["generation_changed"], false);
    assert_eq!(status["receipt"]["current"]["current_source_count"], 1);
    assert_eq!(status["receipt"]["current"]["current_indexed_documents"], 2);
    assert_eq!(
        status["structured_outcome"]["retained_generation"],
        "generation-1"
    );
    assert_eq!(
        status["structured_outcome"]["published_generation"],
        "generation-1"
    );
}

#[test]
fn concurrent_refresh_request_uses_active_generation_without_reopening_inflight_metadata() {
    let temp = tempfile::tempdir().unwrap();
    let coordinator = CoreRefreshEngine::new();
    let request = coordinator.enqueue(None);
    assert_eq!(request["request_state"], "queued");

    let index_root = source_backed_index_root(temp.path());
    let inactive = index_root.join("index-generations/in-flight");
    std::fs::create_dir_all(&inactive).unwrap();
    std::fs::write(inactive.join("meta.json"), b"in-flight metadata").unwrap();

    let coalesced = coordinator
        .handle_ipc_request(
            temp.path(),
            &json!({
                "op": SOURCE_REFRESH_REQUEST_OP,
                "mode": "wait",
                "operation": "refresh",
            }),
        )
        .unwrap()
        .expect("coalesced refresh response");
    assert_eq!(coalesced["request_id"], request["request_id"]);
    assert_eq!(coalesced["coalesced_requests"], 1);
}

#[test]
fn wait_request_with_equivalent_catalog_attaches_to_running_refresh() {
    let temp = tempfile::tempdir().unwrap();
    let coordinator = Arc::new(CoreRefreshEngine::new());
    let authority = load_explicit_source_catalog_authority(temp.path()).unwrap();
    let first = coordinator
        .handle_ipc_request(
            temp.path(),
            &json!({
                "op": SOURCE_REFRESH_REQUEST_OP,
                "mode": "wait",
                "operation": "import",
                "explicit_source_catalog": authority.to_json(),
            }),
        )
        .unwrap()
        .expect("first exact import request");
    assert_eq!(first["trigger"], "import");
    let first_request_id = request_id(&first);
    let (gate, runner_started, runner_release) = RunningRefreshGate::new();
    let executor_runs = Arc::new(AtomicUsize::new(0));

    let attached = std::thread::scope(|scope| {
        let runner = Arc::clone(&coordinator);
        let runner_authority = authority.clone();
        let runner_executor_runs = Arc::clone(&executor_runs);
        scope.spawn(move || {
            let run = runner
                .run_next_with(
                    |_, _| {
                        runner_executor_runs.fetch_add(1, Ordering::SeqCst);
                        runner_started.send(()).expect("signal running refresh");
                        let _ = runner_release.recv();
                        let mut publication = test_publication("generation-1");
                        publication.published_explicit_source_catalog = Some(runner_authority);
                        Ok(publication)
                    },
                    || Ok(Some("generation-1".to_owned())),
                    |_| Ok(()),
                    |_| Ok(()),
                )
                .expect("running refresh");
            assert!(!run.failed);
        });
        gate.wait_until_started();

        let attached = coordinator
            .handle_ipc_request(
                temp.path(),
                &json!({
                    "op": SOURCE_REFRESH_REQUEST_OP,
                    "mode": "wait",
                    "operation": "import",
                    "explicit_source_catalog": authority.to_json(),
                }),
            )
            .unwrap()
            .expect("wait refresh response");
        gate.release();
        attached
    });

    assert_eq!(request_id(&attached), first_request_id);
    assert_eq!(attached["request_state"], "running");
    assert_eq!(attached["coalesced_requests"], 1);
    assert_eq!(attached["trigger"], "import");
    assert_eq!(attached["trigger_provenance"], "explicit_source_catalog");
    assert_eq!(executor_runs.load(Ordering::SeqCst), 1);
    let terminal = coordinator.status(&first_request_id).unwrap();
    assert_eq!(terminal["request_state"], "published");
    assert_eq!(terminal["receipt"]["published_generation"], "generation-1");
    assert!(coordinator
        .run_next_with(
            |_, _| panic!("equivalent wait launched a successor executor"),
            || Ok(Some("generation-1".to_owned())),
            |_| Ok(()),
            |_| Ok(()),
        )
        .is_none());
}

#[test]
fn multiple_equivalent_waiters_share_one_request_and_terminal_receipt() {
    const WAITERS: usize = 8;

    let temp = tempfile::tempdir().unwrap();
    let coordinator = Arc::new(CoreRefreshEngine::new());
    let authority = load_explicit_source_catalog_authority(temp.path()).unwrap();
    let first = coordinator
        .handle_ipc_request(
            temp.path(),
            &json!({
                "op": SOURCE_REFRESH_REQUEST_OP,
                "mode": "wait",
                "operation": "import",
                "explicit_source_catalog": authority.to_json(),
            }),
        )
        .unwrap()
        .expect("first exact import request");
    let first_request_id = request_id(&first);
    let (gate, runner_started, runner_release) = RunningRefreshGate::new();
    let executor_runs = Arc::new(AtomicUsize::new(0));

    let waiter_responses = std::thread::scope(|scope| {
        let runner = Arc::clone(&coordinator);
        let runner_authority = authority.clone();
        let runner_executor_runs = Arc::clone(&executor_runs);
        scope.spawn(move || {
            let run = runner
                .run_next_with(
                    |_, _| {
                        runner_executor_runs.fetch_add(1, Ordering::SeqCst);
                        runner_started.send(()).expect("signal running refresh");
                        let _ = runner_release.recv();
                        let mut publication = test_publication("shared-generation");
                        publication.published_explicit_source_catalog = Some(runner_authority);
                        Ok(publication)
                    },
                    || Ok(Some("shared-generation".to_owned())),
                    |_| Ok(()),
                    |_| Ok(()),
                )
                .expect("running refresh");
            assert!(!run.failed);
        });
        gate.wait_until_started();

        let responses = (0..WAITERS)
            .map(|_| {
                coordinator
                    .handle_ipc_request(
                        temp.path(),
                        &json!({
                            "op": SOURCE_REFRESH_REQUEST_OP,
                            "mode": "wait",
                            "operation": "import",
                            "explicit_source_catalog": authority.to_json(),
                        }),
                    )
                    .unwrap()
                    .expect("wait refresh response")
            })
            .collect::<Vec<_>>();
        gate.release();
        responses
    });

    assert!(waiter_responses
        .iter()
        .all(|response| request_id(response) == first_request_id));
    assert_eq!(executor_runs.load(Ordering::SeqCst), 1);
    let terminal = coordinator.status(&first_request_id).unwrap();
    assert_eq!(terminal["request_state"], "published");
    assert_eq!(terminal["coalesced_requests"], WAITERS as u64);
    assert_eq!(terminal["trigger"], "import");
    assert_eq!(
        terminal["receipt"]["published_generation"],
        "shared-generation"
    );
    assert!(waiter_responses
        .iter()
        .all(|response| { coordinator.status(&request_id(response)).as_ref() == Some(&terminal) }));
    assert!(!coordinator.has_pending_request());
}

#[test]
fn equivalent_waiters_share_the_same_terminal_failure_status() {
    let temp = tempfile::tempdir().unwrap();
    let coordinator = Arc::new(CoreRefreshEngine::new());
    let authority = load_explicit_source_catalog_authority(temp.path()).unwrap();
    let first = coordinator
        .handle_ipc_request(
            temp.path(),
            &json!({
                "op": SOURCE_REFRESH_REQUEST_OP,
                "mode": "wait",
                "operation": "import",
                "explicit_source_catalog": authority.to_json(),
            }),
        )
        .unwrap()
        .expect("first exact import request");
    let first_request_id = request_id(&first);
    let (gate, runner_started, runner_release) = RunningRefreshGate::new();

    let waiter_request_ids = std::thread::scope(|scope| {
        let runner = Arc::clone(&coordinator);
        scope.spawn(move || {
            let run = runner
                .run_next_with(
                    |_, _| {
                        runner_started.send(()).expect("signal running refresh");
                        let _ = runner_release.recv();
                        Err(anyhow!("injected equivalent refresh failure"))
                    },
                    || Ok(None),
                    |_| Ok(()),
                    |_| Ok(()),
                )
                .expect("running refresh");
            assert!(run.failed);
        });
        gate.wait_until_started();

        let request_ids = (0..2)
            .map(|_| {
                coordinator
                    .handle_ipc_request(
                        temp.path(),
                        &json!({
                            "op": SOURCE_REFRESH_REQUEST_OP,
                            "mode": "wait",
                            "operation": "import",
                            "explicit_source_catalog": authority.to_json(),
                        }),
                    )
                    .unwrap()
                    .and_then(|response| {
                        response
                            .get("request_id")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .expect("wait refresh request ID")
            })
            .collect::<Vec<_>>();
        gate.release();
        request_ids
    });

    assert!(waiter_request_ids
        .iter()
        .all(|request_id| request_id == &first_request_id));
    let terminal = coordinator.status(&first_request_id).unwrap();
    assert_eq!(terminal["request_state"], "failed");
    assert!(terminal["receipt"].is_null());
    assert!(terminal["last_error"]
        .as_str()
        .is_some_and(|error| error.contains("injected equivalent refresh failure")));
    assert!(waiter_request_ids
        .iter()
        .all(|request_id| { coordinator.status(request_id).as_ref() == Some(&terminal) }));
    assert!(!coordinator.has_pending_request());
}

#[test]
fn explicit_fresh_after_admitted_snapshot_queues_one_successor() {
    let temp = tempfile::tempdir().unwrap();
    let coordinator = Arc::new(CoreRefreshEngine::new());
    let authority = test_catalog_authority(1, 0x11);
    let first = coordinator
        .handle_ipc_request(
            temp.path(),
            &json!({
                "op": SOURCE_REFRESH_REQUEST_OP,
                "mode": "wait",
                "operation": "import",
                "explicit_source_catalog": authority.to_json(),
            }),
        )
        .unwrap()
        .expect("queued import response");
    let first_request_id = request_id(&first);
    let (gate, runner_started, runner_release) = RunningRefreshGate::new();

    let (successor, replay) = std::thread::scope(|scope| {
        let runner = Arc::clone(&coordinator);
        let runner_authority = authority.clone();
        scope.spawn(move || {
            runner
                .run_next_with(
                    |_, _| {
                        runner_started.send(()).expect("signal running refresh");
                        let _ = runner_release.recv();
                        let mut publication = test_publication("generation-1");
                        publication.published_explicit_source_catalog = Some(runner_authority);
                        Ok(publication)
                    },
                    || Ok(Some("generation-1".to_owned())),
                    |_| Ok(()),
                    |_| Ok(()),
                )
                .expect("running refresh");
        });
        gate.wait_until_started();

        let logical_request_id = Uuid::now_v7().to_string();
        let request_value = json!({
            "op": SOURCE_REFRESH_REQUEST_OP,
            "request_id": logical_request_id,
            "mode": "wait",
            "operation": "import",
            "explicit_source_catalog": authority.to_json(),
            "fresh_after_admitted_snapshot": true,
        });
        let request = || {
            coordinator
                .handle_ipc_request_with_admission_fence_for_test(
                    temp.path(),
                    &request_value,
                    BTreeMap::new(),
                )
                .unwrap()
                .expect("fresh-after-admitted-snapshot response")
        };
        let successor = request();
        let replay = request();
        gate.release();
        (successor, replay)
    });

    let successor_request_id = request_id(&successor);
    assert_ne!(successor_request_id, first_request_id);
    assert_eq!(request_id(&replay), successor_request_id);
    assert_eq!(replay["coalesced_requests"], 0);
    let successor_run = coordinator
        .run_next_with(
            |_, _| {
                let mut publication = test_publication("generation-2");
                publication.published_explicit_source_catalog = Some(authority);
                Ok(publication)
            },
            || Ok(Some("generation-2".to_owned())),
            |_| Ok(()),
            |_| Ok(()),
        )
        .expect("fresh successor");
    assert!(!successor_run.failed);
    assert_eq!(request_id(&successor_run.job), successor_request_id);
    assert!(!coordinator.has_pending_request());
}

#[test]
fn manual_all_fresh_after_running_startup_scan_queues_one_successor() {
    let temp = tempfile::tempdir().unwrap();
    let coordinator = Arc::new(CoreRefreshEngine::new());
    let first = coordinator.enqueue_periodic(temp.path()).unwrap();
    let first_request_id = request_id(&first);
    let (gate, runner_started, runner_release) = RunningRefreshGate::new();

    let (successor, replay) = std::thread::scope(|scope| {
        let runner = Arc::clone(&coordinator);
        scope.spawn(move || {
            runner
                .run_next_with(
                    |_, _| {
                        runner_started.send(()).expect("signal startup scan");
                        let _ = runner_release.recv();
                        Ok(test_publication("startup-generation"))
                    },
                    || Ok(Some("startup-generation".to_owned())),
                    |_| Ok(()),
                    |_| Ok(()),
                )
                .expect("running startup scan");
        });
        gate.wait_until_started();

        let logical_request_id = Uuid::now_v7().to_string();
        let request_value = json!({
            "op": SOURCE_REFRESH_REQUEST_OP,
            "request_id": logical_request_id,
            "mode": "wait",
            "operation": "refresh",
            "fresh_after_admitted_snapshot": true,
        });
        let request = || {
            coordinator
                .handle_ipc_request_with_admission_fence_for_test(
                    temp.path(),
                    &request_value,
                    BTreeMap::new(),
                )
                .unwrap()
                .expect("fresh manual all response")
        };
        let successor = request();
        let replay = request();
        gate.release();
        (successor, replay)
    });

    let successor_request_id = request_id(&successor);
    assert_ne!(successor_request_id, first_request_id);
    assert_eq!(request_id(&replay), successor_request_id);
    assert_eq!(replay["coalesced_requests"], 0);
    let successor_run = coordinator
        .run_next_with(
            |_, _| Ok(test_publication("manual-generation")),
            || Ok(Some("manual-generation".to_owned())),
            |_| Ok(()),
            |_| Ok(()),
        )
        .expect("fresh manual successor");
    assert!(!successor_run.failed);
    assert_eq!(request_id(&successor_run.job), successor_request_id);
    assert!(!coordinator.has_pending_request());
}

#[test]
fn attached_logical_status_projects_one_physical_progress_owner_and_replays_stably() {
    let temp = tempfile::tempdir().unwrap();
    let coordinator = Arc::new(CoreRefreshEngine::new());
    let predecessor = coordinator.enqueue_periodic(temp.path()).unwrap();
    let predecessor_id = request_id(&predecessor);
    assert_eq!(predecessor["progress"]["total_sources"], 0);
    assert_eq!(predecessor["progress"]["total_sources_known"], false);
    let (gate, runner_started, runner_release) = RunningRefreshGate::new();

    std::thread::scope(|scope| {
        let runner = Arc::clone(&coordinator);
        scope.spawn(move || {
            runner
                .run_next_with(
                    |request_id, engine| {
                        engine.set_progress(
                            request_id,
                            SourceBackedRefreshProgressUpdate {
                                phase: "parsing".to_owned(),
                                completed_sources: 2,
                                total_sources: 5,
                                total_sources_known: true,
                                current_source: Some("codex".to_owned()),
                                completed_records: Some(89),
                                completed_bytes: Some(4_096),
                                current_source_progress: None,
                            },
                        );
                        runner_started.send(()).expect("signal physical progress");
                        let _ = runner_release.recv();
                        Ok(test_publication("attached-generation"))
                    },
                    || Ok(Some("attached-generation".to_owned())),
                    |_| Ok(()),
                    |_| Ok(()),
                )
                .expect("running predecessor");
        });
        gate.wait_until_started();

        let demand_id = Uuid::from_u128(0x30501).to_string();
        let attached = coordinator
            .enqueue_fresh_demand_for_test(None, demand_id.clone(), BTreeMap::new())
            .unwrap();
        assert_eq!(attached["request_id"], demand_id);
        assert_eq!(attached["logical_phase"], "attached");
        assert_eq!(attached["physical_attempt_id"], predecessor_id);
        assert_eq!(attached["physical_attempt_state"], "running");
        assert_eq!(attached["progress_owner_request_id"], predecessor_id);
        assert_eq!(attached["progress"]["phase"], "parsing");
        assert_eq!(attached["progress"]["completed_sources"], 2);
        assert_eq!(attached["progress"]["total_sources"], 5);
        assert_eq!(attached["progress"]["completed_records"], 89);
        assert_eq!(attached["progress"]["completed_bytes"], 4_096);

        let replay = coordinator
            .enqueue_fresh_demand_for_test(None, demand_id, BTreeMap::new())
            .unwrap();
        assert_eq!(replay, attached);
        gate.release();
    });
}
