use super::*;

fn submit_automatic(coordinator: &CoreRefreshEngine, data_root: &Path) -> RefreshRequest {
    let request =
        RefreshRequest::automatic(Uuid::now_v7().to_string(), RefreshRequestTrigger::Search);
    let admission = coordinator.submit(data_root, request.clone()).unwrap();
    let (status, barrier) = admission.into_parts();
    assert_eq!(status.request_id(), Some(request.request_id()));
    barrier.unwrap().release(coordinator);
    request
}

fn queued_automatic(
    coordinator: &CoreRefreshEngine,
    data_root: &Path,
    observation: &str,
) -> RefreshRequest {
    let request = submit_automatic(coordinator, data_root);
    coordinator
        .complete_pending_admission_for_test(
            data_root,
            request.request_id(),
            BTreeMap::from([(route_identity(0x71), Some(observation.to_owned()))]),
        )
        .unwrap();
    request
}

fn certified_run<Before, Persist>(
    coordinator: &CoreRefreshEngine,
    generation: &str,
    observation: &str,
    before: Before,
    persist: Persist,
) -> SourceBackedRefreshRun
where
    Before: FnOnce(&super::super::super::CoreRefreshEngine),
    Persist: FnMut(&Value) -> Result<()>,
{
    coordinator
        .run_next_with(
            |id, engine| {
                engine.admit_refresh_scope_for_test(id, &SourceBackedRefreshScope::All)?;
                before(engine);
                engine.set_route_observations_for_test(
                    id,
                    BTreeMap::from([(route_identity(0x71), observation.to_owned())]),
                );
                let mut publication = test_publication(generation);
                publication.route_results = vec![SourceBackedRefreshRouteResult::succeeded(
                    route_identity(0x71).as_str().to_owned(),
                    true,
                )];
                Ok(publication)
            },
            || Ok(Some(generation.to_owned())),
            persist,
            |_| Ok(()),
        )
        .unwrap()
}

#[test]
fn queued_batch_keeps_eight_ids_fingerprints_receipts_and_bounded_replay() {
    let temp = tempfile::tempdir().unwrap();
    let engine = CoreRefreshEngine::new();
    let observation = "72".repeat(32);
    let requests = (0..SOURCE_REFRESH_ACTIVE_PENDING_LIMIT)
        .map(|_| queued_automatic(&engine, temp.path(), &observation))
        .collect::<Vec<_>>();
    let fingerprints = requests
        .iter()
        .map(|request| engine.status(request.request_id()).unwrap()["request_fingerprint"].clone())
        .collect::<Vec<_>>();
    let duplicate = engine.submit(temp.path(), requests[3].clone()).unwrap();
    assert_eq!(
        duplicate.status().request_id(),
        Some(requests[3].request_id())
    );
    assert!(duplicate.into_parts().1.is_none());
    let overloaded = engine
        .submit(
            temp.path(),
            RefreshRequest::automatic(Uuid::now_v7().to_string(), RefreshRequestTrigger::Search),
        )
        .unwrap();
    assert_eq!(
        overloaded.status()["error_code"],
        "source_refresh_queue_full"
    );
    let conflicting = engine
        .submit(
            temp.path(),
            requests[3]
                .clone()
                .with_trigger(RefreshRequestTrigger::Setup),
        )
        .unwrap();
    assert_eq!(conflicting.status()["error_code"], "request_id_conflict");
    let mut persisted = Vec::new();
    let run = certified_run(
        &engine,
        "batch-generation",
        &observation,
        |_| {},
        |job| {
            persisted.push(job.clone());
            Ok(())
        },
    );
    assert!(!run.failed, "{:?}", run.job);
    assert!(!engine.has_pending_request());
    assert_eq!(persisted.len(), requests.len());
    for ((request, fingerprint), job) in requests.iter().zip(fingerprints).zip(persisted) {
        let status = engine.status(request.request_id()).unwrap();
        assert_eq!(status["request_state"], "published");
        assert_eq!(status["request_id"], job["request_id"]);
        assert_eq!(status["request_fingerprint"], fingerprint);
        assert_eq!(status["published_generation"], "batch-generation");
        assert_eq!(
            status["receipt"]["published_generation"],
            "batch-generation"
        );
        assert_eq!(status["receipt"]["generation_changed"], true);
        assert!(status["receipt"].get("previous_generation").is_none());
        assert!(status["coalesced_into_request_id"].is_null());
        RefreshStatus::parse_schema_v1(status).unwrap();
    }
    assert_eq!(
        run.coverage_certificate().unwrap().request_id(),
        requests.last().unwrap().request_id()
    );
}

#[test]
fn queued_batch_late_arrival_with_newer_observation_keeps_its_next_run() {
    let temp = tempfile::tempdir().unwrap();
    let engine = CoreRefreshEngine::new();
    let route = route_identity(0x71);
    let old = "72".repeat(32);
    let new = "73".repeat(32);
    engine.initialize_watch_route_authority([route.clone()]);
    engine.record_watch_routes([(route.clone(), EventWatermark::new(91, 1))], 0);
    let first = queued_automatic(&engine, temp.path(), &old);
    let peer = queued_automatic(&engine, temp.path(), &old);
    let mut late = None;
    let run = certified_run(
        &engine,
        "old-generation",
        &old,
        |owner| {
            owner.record_watch_routes([(route.clone(), EventWatermark::new(91, 2))], 0);
            late = Some(queued_automatic(&engine, temp.path(), &new));
        },
        |_| Ok(()),
    );
    assert!(!run.failed);
    for request in [first, peer] {
        assert_eq!(
            engine.status(request.request_id()).unwrap()["published_generation"],
            "old-generation"
        );
    }
    let late = late.unwrap();
    assert_eq!(
        engine.status(late.request_id()).unwrap()["request_state"],
        "queued"
    );
    assert!(engine.has_scheduled_route_work());
    assert_eq!(
        run.coverage_certificate()
            .unwrap()
            .exact_route_boundaries()
            .next()
            .unwrap()
            .1,
        EventWatermark::new(91, 1)
    );
    let next = certified_run(&engine, "new-generation", &new, |_| {}, |_| Ok(()));
    assert!(!next.failed);
    assert_eq!(next.job["request_id"], late.request_id());
    assert_eq!(next.job["published_generation"], "new-generation");
}

#[test]
fn queued_batch_rejects_unproven_coverage_and_different_admission_authority() {
    for mismatch in [
        "observation",
        "omitted_outcome",
        "failed_outcome",
        "extra_outcome",
        "absent_admission",
        "watermark",
        "config",
        "demand",
        "unobserved",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let engine = CoreRefreshEngine::new();
        let old = "72".repeat(32);
        let new = "73".repeat(32);
        let _first = queued_automatic(&engine, temp.path(), &old);
        let peer = queued_automatic(
            &engine,
            temp.path(),
            if mismatch == "observation" {
                &new
            } else {
                &old
            },
        );
        {
            let mut state = engine.lock_state();
            let member = find_attempt_mut(&mut state, peer.request_id()).unwrap();
            match mismatch {
                "config" => {
                    member.admitted_authority = member.admitted_authority.take().map(|authority| {
                        authority.with_automatic_provider_discovery_for_test(false)
                    })
                }
                "demand" => {
                    member.reconciliation_demand = SourceBackedReconciliationDemand::Exhaustive
                }
                "unobserved" => member.route_observations.clear(),
                _ => {}
            }
            if mismatch == "watermark" {
                state
                    .route_event_watermarks
                    .insert(route_identity(0x71), EventWatermark::new(92, 2));
            }
        }
        let run = engine
            .run_next_with(
                |id, owner| {
                    owner.admit_refresh_scope_for_test(id, &SourceBackedRefreshScope::All)?;
                    if mismatch == "watermark" {
                        owner
                            .lock_state()
                            .route_admission_watermarks
                            .values_mut()
                            .for_each(|routes| {
                                routes.insert(route_identity(0x71), EventWatermark::new(92, 1));
                            });
                    }
                    if mismatch == "absent_admission" {
                        owner.lock_state().route_admission_watermarks.remove(id);
                    }
                    let mut publication = test_publication("generation");
                    if mismatch != "omitted_outcome" {
                        publication
                            .route_results
                            .push(if mismatch == "failed_outcome" {
                                SourceBackedRefreshRouteResult::failed(
                                    route_identity(0x71).as_str().to_owned(),
                                    "unavailable".to_owned(),
                                    false,
                                )
                            } else {
                                SourceBackedRefreshRouteResult::succeeded(
                                    route_identity(0x71).as_str().to_owned(),
                                    true,
                                )
                            });
                    }
                    if mismatch == "extra_outcome" {
                        publication
                            .route_results
                            .push(SourceBackedRefreshRouteResult::succeeded(
                                route_identity(0x74).as_str().to_owned(),
                                true,
                            ));
                    }
                    Ok(publication)
                },
                || Ok(Some("generation".to_owned())),
                |_| Ok(()),
                |_| Ok(()),
            )
            .unwrap();
        assert!(!run.failed, "{mismatch}: {:?}", run.job);
        assert_eq!(
            engine.status(peer.request_id()).unwrap()["request_state"],
            "queued",
            "{mismatch}"
        );
    }
}

#[test]
fn queued_batch_terminal_partial_failure_keeps_pending_gate_and_restart_queue() {
    for fail_at in [1, 2, 3] {
        let temp = tempfile::tempdir().unwrap();
        let engine = CoreRefreshEngine::new();
        let observation = "72".repeat(32);
        let requests = (0..4)
            .map(|_| queued_automatic(&engine, temp.path(), &observation))
            .collect::<Vec<_>>();
        let mut writes = 0;
        let run = certified_run(
            &engine,
            "generation",
            &observation,
            |_| {},
            |job| {
                writes += 1;
                if writes == fail_at {
                    bail!("injected partial batch terminal failure");
                }
                engine.journal.store(temp.path(), job)
            },
        );
        assert!(run.terminal_persistence_pending);
        assert!(!run.did_work);
        assert!(run.coverage_certificate().is_none());
        let pending_id = requests[fail_at - 1].request_id();
        let pending = engine.status(pending_id).unwrap();
        assert_eq!(pending["request_state"], "running");
        assert_eq!(pending["progress"]["phase"], "persisting_terminal");
        assert!(pending.get("receipt").is_none());
        assert!(pending.get("structured_outcome").is_none());
        assert_eq!(
            engine.status(requests[fail_at].request_id()).unwrap()["request_state"],
            "queued"
        );
        let retry = engine
            .run_next_with(
                |_, _| panic!("persistence retry must not capture"),
                || panic!("persistence retry must not reopen Core"),
                |job| engine.journal.store(temp.path(), job),
                |_| Ok(()),
            )
            .unwrap();
        assert!(!retry.terminal_persistence_pending);
        assert_eq!(retry.job["request_id"], pending_id);
        assert_eq!(
            engine.status(pending_id).unwrap()["request_state"],
            "published"
        );
        let restarted = super::super::super::CoreRefreshEngine::with_journal_for_test(
            Arc::clone(&engine.journal),
            test_refresh_runtime(),
            Arc::new(|_: SourceBackedRefreshExecution<'_>| panic!("restart must not execute")),
        );
        assert!(restarted
            .recover_interrupted_publication(temp.path())
            .unwrap());
        assert_eq!(
            restarted.status(pending_id).unwrap()["request_state"],
            "published"
        );
        for request in &requests[fail_at..] {
            assert_eq!(
                restarted.status(request.request_id()).unwrap()["request_state"],
                "admission_pending"
            );
        }
    }
}

#[test]
fn queued_batch_failed_capture_leaves_peers_for_their_own_attempt() {
    let temp = tempfile::tempdir().unwrap();
    let engine = CoreRefreshEngine::new();
    let observation = "72".repeat(32);
    queued_automatic(&engine, temp.path(), &observation);
    let peer = queued_automatic(&engine, temp.path(), &observation);
    let run = engine
        .run_next_with(
            |_, _| Err(anyhow!("capture failed")),
            || Ok(None),
            |_| Ok(()),
            |_| Ok(()),
        )
        .unwrap();
    assert!(run.failed);
    assert_eq!(
        engine.status(peer.request_id()).unwrap()["request_state"],
        "queued"
    );
}

#[test]
fn queued_batch_production_run_resolves_pending_peers_and_pins_one_publication() {
    let temp = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    let engine = CoreRefreshEngine::with_executor_and_admitted_routes(
        Arc::new(move |execution: SourceBackedRefreshExecution<'_>| {
            counted.fetch_add(1, Ordering::SeqCst);
            publish_pin_fixture(&execution, false)
        }),
        [route_identity(0x71)],
    );
    let requests = (0..4)
        .map(|_| submit_automatic(&engine, temp.path()))
        .collect::<Vec<_>>();
    let run = engine.run_next(temp.path()).unwrap();
    assert_eq!(run.job["certified_source_count"], 1);
    assert_eq!(
        run.coverage_certificate()
            .unwrap()
            .exact_route_boundaries()
            .len(),
        0
    );
    assert!(!run.failed, "{:?}", run.job);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(!engine.has_pending_request());
    let pinned = engine.pinned_core_publication().unwrap();
    for request in requests {
        let status = engine.status(request.request_id()).unwrap();
        assert_eq!(status["request_state"], "published");
        assert_eq!(status["published_generation"], pinned.generation_id());
        assert_eq!(
            status["receipt"]["published_generation"],
            pinned.generation_id()
        );
    }
}

#[test]
fn queued_batch_follower_admission_failure_keeps_the_root_and_pending_peer() {
    let temp = tempfile::tempdir().unwrap();
    let journal = Arc::new(TestRefreshJournal::default());
    let engine = CoreRefreshEngine(
        super::super::super::CoreRefreshEngine::with_runtime_for_test(
            journal.clone(),
            test_refresh_runtime(),
            Arc::new(|_: SourceBackedRefreshExecution<'_>| panic!("static admission witness")),
            Arc::new(|_, _, _, _| Err(anyhow!("follower discovery failed"))),
        ),
    );
    let root = queued_automatic(&engine, temp.path(), &"72".repeat(32));
    let peer = submit_automatic(&engine, temp.path());
    engine.prepare_queued_batch_admissions(temp.path());
    assert_eq!(
        engine.status(root.request_id()).unwrap()["request_state"],
        "queued"
    );
    assert_eq!(
        engine.status(peer.request_id()).unwrap()["request_state"],
        "admission_pending"
    );
    let stored = journal.load(temp.path()).unwrap().unwrap();
    assert_eq!(stored["request_id"], root.request_id());
    assert_eq!(
        stored["queued_successors"][0]["request_id"],
        peer.request_id()
    );
    assert_eq!(
        stored["queued_successors"][0]["request_state"],
        "admission_pending"
    );
    assert!(!engine
        .lock_state()
        .admission_resolutions_in_flight
        .contains(peer.request_id()));
    certified_run(
        &engine,
        "generation",
        &"72".repeat(32),
        |_| {},
        |job| journal.store(temp.path(), job),
    );
    let failure = engine
        .resolve_pending_admission(temp.path())
        .unwrap()
        .unwrap();
    assert!(failure.failed);
    assert_eq!(failure.job["request_id"], peer.request_id());
    assert!(!engine.has_pending_request());
}

#[test]
fn queued_batch_restart_retains_all_admissions_but_only_last_drained_terminal() {
    let temp = tempfile::tempdir().unwrap();
    let engine = CoreRefreshEngine::new();
    let observation = "72".repeat(32);
    let requests = (0..SOURCE_REFRESH_ACTIVE_PENDING_LIMIT)
        .map(|_| queued_automatic(&engine, temp.path(), &observation))
        .collect::<Vec<_>>();
    let before = engine.journal.load(temp.path()).unwrap().unwrap();
    let before_journal = Arc::new(TestRefreshJournal::default());
    before_journal.store(temp.path(), &before).unwrap();
    let before_restart = super::super::super::CoreRefreshEngine::with_journal_for_test(
        before_journal,
        test_refresh_runtime(),
        Arc::new(|_: SourceBackedRefreshExecution<'_>| panic!("restart does not capture")),
    );
    assert!(before_restart
        .recover_interrupted_publication(temp.path())
        .unwrap());
    for request in &requests {
        let status = before_restart.status(request.request_id()).unwrap();
        assert_eq!(status["request_state"], "admission_pending");
        assert_eq!(
            status["request_fingerprint"],
            engine.status(request.request_id()).unwrap()["request_fingerprint"]
        );
    }
    certified_run(
        &engine,
        "generation",
        &observation,
        |_| {},
        |job| engine.journal.store(temp.path(), job),
    );
    let after_restart = super::super::super::CoreRefreshEngine::with_journal_for_test(
        Arc::clone(&engine.journal),
        test_refresh_runtime(),
        Arc::new(|_: SourceBackedRefreshExecution<'_>| panic!("restart does not capture")),
    );
    assert!(!after_restart
        .recover_interrupted_publication(temp.path())
        .unwrap());
    for request in &requests[..requests.len() - 1] {
        assert!(after_restart.status(request.request_id()).is_none());
    }
    assert_eq!(
        after_restart
            .status(requests.last().unwrap().request_id())
            .unwrap()["request_state"],
        "published"
    );
}
