use super::*;
use crate::engine::admission::admission_failure_fence_matches;
use ctx_history_capture::{
    SourceBackedProviderRegistry, SourceBackedRoute, SourceBackedRouteDriver,
    SourceBackedSelectorAuthority,
};
use ctx_history_capture_model::ProviderSourceStatus;
#[test]
fn ordinary_manual_all_still_scans_every_current_route() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let routes = BTreeSet::from([route_identity(0x45), route_identity(0x46)]);
    let scans = Arc::new(Mutex::new(BTreeMap::<SourceRouteIdentity, usize>::new()));
    let executor_routes = routes.clone();
    let executor_scans = Arc::clone(&scans);
    let coordinator = CoreRefreshEngine::with_executor_and_admitted_routes(
        Arc::new(move |execution: SourceBackedRefreshExecution<'_>| {
            assert_eq!(
                execution.admitted_refresh().publication_scope(),
                SourceBackedRefreshScope::All
            );
            let selected = physically_selected_routes(&execution, &executor_routes);
            for route in &selected {
                *executor_scans
                    .lock()
                    .unwrap()
                    .entry(route.clone())
                    .or_default() += 1;
            }
            publish_selected_routes(&execution, &selected, None)
        }),
        routes.clone(),
    );
    coordinator.reconcile_watch_routes(
        routes.clone(),
        EventWatermark::new(5, 0),
        ledger_now_ms().saturating_sub(1_000),
    );
    let manual = manual_all_request_without_catalog(&coordinator, &data_root);

    let run = coordinator
        .run_next(&data_root)
        .expect("ordinary manual all");
    assert!(!run.failed);
    assert_eq!(request_id(&run.job), request_id(&manual));
    assert_eq!(
        *scans.lock().unwrap(),
        routes
            .iter()
            .cloned()
            .map(|route| (route, 1))
            .collect::<BTreeMap<_, _>>()
    );
    assert!(!coordinator.has_scheduled_route_work());
}

#[test]
fn exact_route_event_during_execution_creates_one_successor_and_noop_ack_cleans_it() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let route = route_identity(0x51);
    let calls = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let executor_calls = Arc::clone(&calls);
    let executor_entered = Arc::clone(&entered);
    let executor_release = Arc::clone(&release);
    let executor_route = route.clone();
    let executor: Arc<dyn SourceBackedRefreshExecutor> =
        Arc::new(move |execution: SourceBackedRefreshExecution<'_>| {
            assert_eq!(
                execution.admitted_refresh().publication_scope(),
                SourceBackedRefreshScope::exact([executor_route.clone()])
            );
            let call = executor_calls.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                executor_entered.wait();
                executor_release.wait();
            }
            let commit = ctx_history_index::GenerationWriter::open(
                execution.index_root,
                WriterOptions::default(),
            )?
            .into_writer()
            .map_err(crate::committed_generation_recovery_error)?
            .commit(|_| true)?;
            let mut publication = empty_test_publication(commit.generation_id);
            publication.published_explicit_source_catalog =
                execution.explicit_source_catalog.cloned();
            publication.route_results = vec![SourceBackedRefreshRouteResult::succeeded(
                executor_route.as_str().to_owned(),
                true,
            )];
            Ok(publication)
        });
    let coordinator = Arc::new(CoreRefreshEngine::with_executor_and_admitted_routes(
        executor,
        [route.clone()],
    ));
    let observed_at_ms = ledger_now_ms().saturating_sub(1_000);
    coordinator.reconcile_watch_routes([route.clone()], EventWatermark::new(1, 0), observed_at_ms);
    assert!(coordinator
        .enqueue_next_dirty_route(&data_root, ledger_now_ms())
        .unwrap());

    std::thread::scope(|scope| {
        let runner = Arc::clone(&coordinator);
        let runner_data_root = data_root.clone();
        scope.spawn(move || {
            let run = runner
                .run_next(&runner_data_root)
                .expect("first exact route run");
            assert!(!run.failed);
            assert!(matches!(run.scope, SourceBackedRefreshScope::Exact(_)));
        });
        entered.wait();
        coordinator
            .record_watch_routes([(route.clone(), EventWatermark::new(1, 1))], observed_at_ms);
        release.wait();
    });

    assert!(coordinator
        .enqueue_next_dirty_route(&data_root, ledger_now_ms())
        .unwrap());
    let successor = coordinator
        .run_next(&data_root)
        .expect("successor exact route run");
    assert!(!successor.failed);
    assert!(!successor.did_work, "unchanged exact route must be a no-op");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(!coordinator.has_scheduled_route_work());
    assert!(!coordinator
        .enqueue_next_dirty_route(&data_root, ledger_now_ms())
        .unwrap());
}

fn route_failure_executor(
    route: SourceRouteIdentity,
    class: &'static str,
) -> Arc<dyn SourceBackedRefreshExecutor> {
    Arc::new(move |execution: SourceBackedRefreshExecution<'_>| {
        let commit = ctx_history_index::GenerationWriter::open(
            execution.index_root,
            WriterOptions::default(),
        )?
        .into_writer()
        .map_err(crate::committed_generation_recovery_error)?
        .commit(|_| true)?;
        let mut publication = empty_test_publication(commit.generation_id);
        publication.published_explicit_source_catalog = execution.explicit_source_catalog.cloned();
        let mut result = SourceBackedRefreshRouteResult::failed(
            route.as_str().to_owned(),
            class.to_owned(),
            true,
        );
        result.source_failures = vec![SourceBackedRefreshSourceFailure {
            route_identity: route.as_str().to_owned(),
            source_identity: "cd".repeat(32),
            provider: "fixture".to_owned(),
            class: class.to_owned(),
            carried_forward: true,
            source_selector: "fixture source".to_owned(),
            detail: "fixture failure".to_owned(),
        }];
        publication.route_results = vec![result];
        Ok(publication)
    })
}

#[test]
fn exact_route_receipt_failures_back_off_or_block_until_a_new_event() {
    let route = route_identity(0x61);
    let observed_at_ms = ledger_now_ms().saturating_sub(1_000);

    let retry_temp = tempfile::tempdir().unwrap();
    let retry_root = retry_temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&retry_root).unwrap();
    let retry = CoreRefreshEngine::with_executor_and_admitted_routes(
        route_failure_executor(route.clone(), "unavailable"),
        [route.clone()],
    );
    retry.reconcile_watch_routes([route.clone()], EventWatermark::new(1, 0), observed_at_ms);
    assert!(retry
        .enqueue_next_dirty_route(&retry_root, ledger_now_ms())
        .unwrap());
    let retry_run = retry.run_next(&retry_root).unwrap();
    assert!(!retry_run.failed);
    assert!(retry_run.job.get("automatic_retry").is_none());
    let retry_after = retry
        .next_dirty_route_due_in_ms(ledger_now_ms())
        .expect("retryable route remains scheduled");
    assert!(
        retry_after <= 10_000 && retry_after > 0,
        "unexpected retry delay: {retry_after}ms"
    );
    assert!(!retry
        .enqueue_next_dirty_route(&retry_root, ledger_now_ms())
        .unwrap());

    let blocked_temp = tempfile::tempdir().unwrap();
    let blocked_root = blocked_temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&blocked_root).unwrap();
    let blocked = CoreRefreshEngine::with_executor_and_admitted_routes(
        route_failure_executor(route.clone(), "incompatible"),
        [route.clone()],
    );
    blocked.reconcile_watch_routes([route.clone()], EventWatermark::new(2, 0), observed_at_ms);
    assert!(blocked
        .enqueue_next_dirty_route(&blocked_root, ledger_now_ms())
        .unwrap());
    let blocked_run = blocked.run_next(&blocked_root).unwrap();
    assert!(!blocked_run.failed);
    assert!(blocked_run.job.get("automatic_retry").is_none());
    assert!(!blocked.has_scheduled_route_work());
    blocked.record_watch_routes([(route.clone(), EventWatermark::new(2, 0))], observed_at_ms);
    assert!(!blocked.has_scheduled_route_work());
    blocked.record_watch_routes([(route, EventWatermark::new(2, 1))], observed_at_ms);
    assert!(blocked.has_scheduled_route_work());
}

#[test]
fn successful_partial_publication_retains_mixed_route_retry_dispositions() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let retryable_route = route_identity(0x62);
    let blocked_route = route_identity(0x63);
    let routes = BTreeSet::from([retryable_route.clone(), blocked_route.clone()]);
    let executor_retryable_route = retryable_route.clone();
    let executor_blocked_route = blocked_route.clone();
    let executor: Arc<dyn SourceBackedRefreshExecutor> =
        Arc::new(move |execution: SourceBackedRefreshExecution<'_>| {
            let commit = ctx_history_index::GenerationWriter::open(
                execution.index_root,
                WriterOptions::default(),
            )?
            .into_writer()
            .map_err(crate::committed_generation_recovery_error)?
            .commit(|_| true)?;
            let mut publication = empty_test_publication(commit.generation_id);
            publication.route_results = [
                (&executor_retryable_route, "unavailable"),
                (&executor_blocked_route, "incompatible"),
            ]
            .into_iter()
            .map(|(route, class)| {
                let mut result =
                    SourceBackedRefreshRouteResult::succeeded(route.as_str().to_owned(), false);
                result.source_failure_total = 1;
                result.source_retryable_failure_total = usize::from(class == "unavailable");
                result.source_failures = vec![SourceBackedRefreshSourceFailure {
                    route_identity: route.as_str().to_owned(),
                    source_identity: "ef".repeat(32),
                    provider: "fixture".to_owned(),
                    class: class.to_owned(),
                    carried_forward: true,
                    source_selector: "fixture logical source".to_owned(),
                    detail: "fixture partial source failure".to_owned(),
                }];
                result
            })
            .collect();
            Ok(publication)
        });
    let coordinator =
        CoreRefreshEngine::with_executor_and_admitted_routes(executor, routes.clone());
    let observed_at_ms = ledger_now_ms().saturating_sub(1_000);
    coordinator.reconcile_watch_routes(routes.clone(), EventWatermark::new(4, 0), observed_at_ms);
    assert!(coordinator
        .enqueue_next_dirty_route(&data_root, ledger_now_ms())
        .unwrap());

    let run = coordinator
        .run_next(&data_root)
        .expect("partial publication");

    assert!(!run.failed, "{:#}", run.job);
    assert_eq!(
        run.job["structured_outcome"]["code"],
        "completed_with_source_failures"
    );
    assert_eq!(run.job["structured_outcome"]["retryable"], true);
    assert_eq!(
        run.job["structured_outcome"]["retryable_routes"],
        json!([retryable_route.as_str()])
    );
    assert_eq!(
        run.job["structured_outcome"]["blocked_routes"],
        json!([blocked_route.as_str()])
    );
    assert_eq!(
        run.job["structured_outcome"]["retained_generation"],
        run.job["published_generation"]
    );
    assert_eq!(
        run.job["structured_outcome"]["published_generation"],
        run.job["published_generation"]
    );
    assert_eq!(coordinator.dirty_route_ids_for_test(), routes);
    assert!(!coordinator.route_is_permanently_blocked_for_test(&retryable_route));
    assert!(coordinator.route_is_permanently_blocked_for_test(&blocked_route));
    assert!(coordinator
        .next_dirty_route_due_in_ms(ledger_now_ms())
        .is_some());
    coordinator.record_watch_routes(
        [(blocked_route.clone(), EventWatermark::new(4, 1))],
        observed_at_ms,
    );
    assert!(!coordinator.route_is_permanently_blocked_for_test(&blocked_route));
}

#[test]
fn bounded_nonretryable_partial_publication_does_not_schedule_route_retry() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let route = route_identity(0x64);
    let executor_route = route.clone();
    let executor: Arc<dyn SourceBackedRefreshExecutor> =
        Arc::new(move |execution: SourceBackedRefreshExecution<'_>| {
            let commit = ctx_history_index::GenerationWriter::open(
                execution.index_root,
                WriterOptions::default(),
            )?
            .into_writer()
            .map_err(crate::committed_generation_recovery_error)?
            .commit(|_| true)?;
            let mut publication = empty_test_publication(commit.generation_id);
            let mut result = SourceBackedRefreshRouteResult::succeeded(
                executor_route.as_str().to_owned(),
                false,
            );
            result.source_failure_total = 234;
            result.source_retryable_failure_total = 0;
            publication.route_results = vec![result];
            Ok(publication)
        });
    let coordinator =
        CoreRefreshEngine::with_executor_and_admitted_routes(executor, [route.clone()]);
    coordinator.reconcile_watch_routes(
        [route.clone()],
        EventWatermark::new(5, 0),
        ledger_now_ms().saturating_sub(1_000),
    );
    assert!(coordinator
        .enqueue_next_dirty_route(&data_root, ledger_now_ms())
        .unwrap());

    let run = coordinator
        .run_next(&data_root)
        .expect("partial publication");

    assert!(!run.failed, "{:#}", run.job);
    assert_eq!(
        run.job["structured_outcome"]["code"],
        "completed_with_source_failures"
    );
    assert_eq!(run.job["structured_outcome"]["retryable"], false);
    assert_eq!(
        run.job["structured_outcome"]["blocked_routes"],
        json!([route.as_str()])
    );
    assert!(!coordinator.has_scheduled_route_work());
}

#[test]
fn systemic_exact_publication_failure_leaves_the_route_dirty_with_backoff() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let route = route_identity(0x71);
    let executor: Arc<dyn SourceBackedRefreshExecutor> =
        Arc::new(|_: SourceBackedRefreshExecution<'_>| Err(anyhow!("systemic fixture failure")));
    let coordinator =
        CoreRefreshEngine::with_executor_and_admitted_routes(executor, [route.clone()]);
    coordinator.reconcile_watch_routes(
        [route],
        EventWatermark::new(3, 0),
        ledger_now_ms().saturating_sub(1_000),
    );
    assert!(coordinator
        .enqueue_next_dirty_route(&data_root, ledger_now_ms())
        .unwrap());
    assert!(coordinator.run_next(&data_root).unwrap().failed);
    let retry_after = coordinator
        .next_dirty_route_due_in_ms(ledger_now_ms())
        .expect("systemic failure remains dirty");
    assert!(retry_after <= 10_000 && retry_after > 0);
}

fn automatic_retry_route(path: PathBuf) -> (SourceBackedRoute, SourceRouteIdentity) {
    automatic_retry_route_for_provider(path, CaptureProvider::Codex, "codex_history_jsonl")
}

fn automatic_retry_route_for_provider(
    path: PathBuf,
    provider: CaptureProvider,
    source_format: &'static str,
) -> (SourceBackedRoute, SourceRouteIdentity) {
    let source = ProviderSource {
        provider,
        exists: true,
        path,
        source_format,
        source_kind: ProviderSourceKind::NativeHistory,
        import_support: ProviderImportSupport::Native,
        catalog_support: ProviderCatalogSupport::None,
        status: ProviderSourceStatus::Available,
        unsupported_reason: None,
        route_provenance: Default::default(),
    };
    let route = SourceBackedRoute::automatic(
        source,
        SourceBackedSelectorAuthority::DiscoveredWinner,
        SourceBackedRouteDriver::new(|_| Ok(()), |_| false, |_| true),
    )
    .unwrap();
    let identity = route.metadata().route_identity.clone().unwrap();
    (route, identity)
}

fn automatic_retry_catalog(path: PathBuf) -> (SourceBackedWatchCatalog, SourceRouteIdentity) {
    let (route, identity) = automatic_retry_route(path);
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(route);
    (registry.watch_catalog(), identity)
}

fn automatic_retry_fixture(
    executor: Arc<dyn SourceBackedRefreshExecutor>,
) -> (
    tempfile::TempDir,
    PathBuf,
    PathBuf,
    CoreRefreshEngine,
    SourceBackedWatchCatalog,
    SourceRouteIdentity,
    String,
) {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let source_path = temp.path().join("history.jsonl");
    fs::write(&source_path, b"stable\n").unwrap();
    let (catalog, route) = automatic_retry_catalog(source_path.clone());
    let retained_generation = publish_pin_source(
        &source_backed_index_root(&data_root),
        publication_pin_source(),
    );
    let coordinator =
        CoreRefreshEngine::with_executor_and_admitted_routes(executor, [route.clone()]);
    coordinator.install_watch_catalog(catalog.clone());
    coordinator.reconcile_watch_routes(
        [route.clone()],
        EventWatermark::new(40, 1),
        ledger_now_ms().saturating_sub(1_000),
    );
    (
        temp,
        data_root,
        source_path,
        coordinator,
        catalog,
        route,
        retained_generation,
    )
}

fn failing_executor(calls: Arc<AtomicUsize>) -> Arc<dyn SourceBackedRefreshExecutor> {
    Arc::new(move |_: SourceBackedRefreshExecution<'_>| {
        calls.fetch_add(1, Ordering::SeqCst);
        Err(anyhow!("stable internal refresh fixture failure"))
    })
}

fn coverage_failure_executor(calls: Arc<AtomicUsize>) -> Arc<dyn SourceBackedRefreshExecutor> {
    Arc::new(move |_: SourceBackedRefreshExecution<'_>| {
        calls.fetch_add(1, Ordering::SeqCst);
        Err(ZeroSourcePublicationBlocked::new("stable terminal coverage failure").into())
    })
}

#[test]
fn mixed_automatic_retry_serialization_round_trips_through_recovery() {
    let paused_route = route_identity(0x72);
    let confirming_route = route_identity(0x73);
    let observation = "ab".repeat(32);
    let retryable_routes = BTreeSet::from([confirming_route.clone()]);
    let blocked_routes = BTreeSet::from([paused_route.clone()]);
    let outcome = SourceBackedRefreshFailureOutcome::with_route_dispositions(
        RefreshOutcomeCode::SourceRefreshFailed,
        RefreshOutcomeClass::Internal,
        true,
        retryable_routes,
        blocked_routes,
        Some(RefreshRetryAdvice::RetryAffectedRoutes),
    );
    let mut paused = SourceBackedAutomaticRetryCheckpoint::confirming(
        &outcome,
        &paused_route,
        &observation,
        "stable internal failure",
    );
    paused.pause();
    let confirming = SourceBackedAutomaticRetryCheckpoint::confirming(
        &outcome,
        &confirming_route,
        &observation,
        "stable internal failure",
    );
    let expected = BTreeMap::from([
        (paused_route.clone(), paused),
        (confirming_route.clone(), confirming),
    ]);
    let mut attempt = new_refresh_attempt(
        None,
        SourceRefreshRuntimeMetadata::periodic(),
        RefreshIntent::AutomaticMaintenance,
        SourceBackedRefreshScope::exact([paused_route, confirming_route]),
    );
    attempt.state = SourceBackedRefreshState::Failed;
    attempt.failure_outcome = Some(outcome);
    attempt.last_error = Some("stable internal failure".to_owned());
    attempt.automatic_retry_checkpoints = expected.clone();

    let job = attempt.job_json();

    assert_eq!(job["automatic_retry"]["state"], "mixed");
    assert_eq!(
        job["automatic_retry"]["reason"],
        "repeated_internal_failure"
    );
    assert_eq!(
        request_lifecycle::recover_automatic_retry_checkpoints(&job).unwrap(),
        expected
    );
}

#[test]
fn cold_scheduler_does_not_widen_around_an_unchanged_paused_route() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let paused_path = temp.path().join("paused.jsonl");
    let healthy_path = temp.path().join("healthy.jsonl");
    fs::write(&paused_path, b"paused\n").unwrap();
    fs::write(&healthy_path, b"healthy\n").unwrap();
    let (paused_source, paused_route) = automatic_retry_route(paused_path);
    let (healthy_source, healthy_route) = automatic_retry_route_for_provider(
        healthy_path,
        CaptureProvider::Claude,
        "claude_projects_jsonl_tree",
    );
    assert_ne!(paused_route, healthy_route);
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(paused_source);
    registry.register(healthy_source);
    let catalog = registry.watch_catalog();
    let observation = catalog.certify_route_observation(&paused_route).unwrap();
    let outcome = SourceBackedRefreshFailureOutcome::new(
        RefreshOutcomeCode::SourceRefreshFailed,
        RefreshOutcomeClass::Internal,
        true,
        BTreeSet::from([paused_route.clone()]),
        Some(RefreshRetryAdvice::RetryAffectedRoutes),
    );
    let mut checkpoint = SourceBackedAutomaticRetryCheckpoint::confirming(
        &outcome,
        &paused_route,
        &observation,
        "stable internal failure",
    );
    checkpoint.pause();
    let coordinator = CoreRefreshEngine::with_executor_and_admitted_routes(
        failing_executor(Arc::new(AtomicUsize::new(0))),
        [paused_route.clone(), healthy_route.clone()],
    );
    coordinator.install_watch_catalog(catalog);
    {
        let mut state = coordinator.lock_state();
        state
            .automatic_retry_checkpoints
            .insert(paused_route.clone(), checkpoint);
        state.dirty_routes.seed_exact_routes(
            [paused_route.clone()],
            EventWatermark::new(80, 1),
            ledger_now_ms().saturating_sub(1_000),
        );
        state.dirty_routes.block_exact_routes([&paused_route]);
    }
    coordinator.schedule_startup_route_reconciliation(
        [healthy_route.clone()],
        EventWatermark::new(81, 1),
        ledger_now_ms().saturating_sub(1_000),
    );

    assert!(coordinator
        .enqueue_next_scheduled_refresh(&data_root, ledger_now_ms())
        .unwrap());
    let state = coordinator.lock_state();
    let request_id = state.active_request_id.as_deref().unwrap();
    let attempt = find_attempt(&state, request_id).unwrap();
    assert_eq!(
        attempt.refresh_scope,
        SourceBackedRefreshScope::exact([healthy_route])
    );
    assert!(state.dirty_routes.is_permanently_blocked(&paused_route));
}

#[test]
fn periodic_catalog_refresh_excludes_an_unchanged_paused_route() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let paused_path = temp.path().join("paused.jsonl");
    let healthy_path = temp.path().join("healthy.jsonl");
    fs::write(&paused_path, b"paused\n").unwrap();
    fs::write(&healthy_path, b"healthy\n").unwrap();
    let (paused_source, paused_route) = automatic_retry_route(paused_path);
    let (healthy_source, healthy_route) = automatic_retry_route_for_provider(
        healthy_path,
        CaptureProvider::Claude,
        "claude_projects_jsonl_tree",
    );
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(paused_source);
    registry.register(healthy_source);
    let catalog = registry.watch_catalog();
    let observation = catalog.certify_route_observation(&paused_route).unwrap();
    let outcome = SourceBackedRefreshFailureOutcome::new(
        RefreshOutcomeCode::SourceRefreshFailed,
        RefreshOutcomeClass::Internal,
        true,
        BTreeSet::from([paused_route.clone()]),
        Some(RefreshRetryAdvice::RetryAffectedRoutes),
    );
    let mut checkpoint = SourceBackedAutomaticRetryCheckpoint::confirming(
        &outcome,
        &paused_route,
        &observation,
        "stable internal failure",
    );
    checkpoint.pause();
    let coordinator = CoreRefreshEngine::with_executor_and_admitted_routes(
        failing_executor(Arc::new(AtomicUsize::new(0))),
        [paused_route.clone(), healthy_route.clone()],
    );
    coordinator.install_watch_catalog(catalog);
    coordinator
        .lock_state()
        .automatic_retry_checkpoints
        .insert(paused_route.clone(), checkpoint);

    let response = coordinator.enqueue_periodic(&data_root).unwrap();
    let request_id = response["request_id"].as_str().unwrap();

    let scope = {
        let state = coordinator.lock_state();
        find_attempt(&state, request_id)
            .unwrap()
            .refresh_scope
            .clone()
    };
    assert_eq!(
        scope,
        SourceBackedRefreshScope::exact([healthy_route.clone()])
    );
    assert!(coordinator
        .scheduled_route_ids_for_test()
        .contains(&healthy_route));
    assert!(!coordinator
        .scheduled_route_ids_for_test()
        .contains(&paused_route));
}

#[test]
fn periodic_catalog_refresh_does_not_enqueue_when_every_route_is_paused() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (_temp, data_root, _source_path, coordinator, _catalog, route, _generation) =
        automatic_retry_fixture(failing_executor(Arc::clone(&calls)));
    let _ = run_due_failure(&coordinator, &data_root, &route);
    let paused = run_due_failure(&coordinator, &data_root, &route);
    let paused_request_id = paused.job["request_id"].as_str().unwrap();

    let response = coordinator.enqueue_periodic(&data_root).unwrap();

    assert_eq!(response["request_id"], paused_request_id);
    assert!(!coordinator.has_pending_request());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn admission_observation_fence_rejects_a_newer_route_event() {
    let temp = tempfile::tempdir().unwrap();
    let source_path = temp.path().join("history.jsonl");
    fs::write(&source_path, b"stable\n").unwrap();
    let (catalog, route) = automatic_retry_catalog(source_path);
    let coordinator = CoreRefreshEngine::with_executor_and_admitted_routes(
        failing_executor(Arc::new(AtomicUsize::new(0))),
        [route.clone()],
    );
    coordinator.install_watch_catalog(catalog.clone());
    coordinator.reconcile_watch_routes(
        [route.clone()],
        EventWatermark::new(70, 1),
        ledger_now_ms().saturating_sub(1_000),
    );
    let authority = ctx_history_refresh_execution::AdmittedRefresh::from_exact_catalog_authority(
        BTreeSet::from([route.clone()]),
        StdDuration::ZERO,
        catalog,
    )
    .unwrap();

    let (catalog_revision, route_event_watermarks) = {
        let state = coordinator.lock_state();
        (
            state.watch_catalog_revision,
            state.route_event_watermarks.clone(),
        )
    };
    let fence = coordinator
        .sample_admission_observations(&authority, catalog_revision, &route_event_watermarks)
        .unwrap();
    assert!(fence.still_matches(&coordinator.lock_state()));

    coordinator.record_watch_routes([(route, EventWatermark::new(70, 2))], ledger_now_ms());
    assert!(!fence.still_matches(&coordinator.lock_state()));
}

#[test]
fn exact_admission_failure_ignores_an_unrelated_route_event() {
    let selected = route_identity(0x74);
    let unrelated = route_identity(0x75);
    let coordinator = CoreRefreshEngine::with_executor_and_admitted_routes(
        failing_executor(Arc::new(AtomicUsize::new(0))),
        [selected.clone(), unrelated.clone()],
    );
    coordinator.set_route_event_watermark_for_test(selected.clone(), EventWatermark::new(90, 1));
    coordinator.set_route_event_watermark_for_test(unrelated.clone(), EventWatermark::new(90, 1));
    let claimed = coordinator.lock_state().route_event_watermarks.clone();

    coordinator.set_route_event_watermark_for_test(unrelated, EventWatermark::new(90, 2));
    let state = coordinator.lock_state();

    assert!(admission_failure_fence_matches(
        &state,
        &SourceBackedRefreshScope::exact([selected]),
        &claimed,
    ));
    assert!(!admission_failure_fence_matches(
        &state,
        &SourceBackedRefreshScope::All,
        &claimed,
    ));
}

static AUTOMATIC_RETRY_TEST_EVENT_EPOCH: AtomicU64 = AtomicU64::new(100);

fn automatic_retry_test_watermark() -> EventWatermark {
    EventWatermark::new(
        AUTOMATIC_RETRY_TEST_EVENT_EPOCH.fetch_add(1, Ordering::SeqCst),
        1,
    )
}

fn run_due_failure(
    coordinator: &CoreRefreshEngine,
    data_root: &Path,
    route: &SourceRouteIdentity,
) -> SourceBackedRefreshRun {
    coordinator.schedule_startup_route_reconciliation(
        [route.clone()],
        automatic_retry_test_watermark(),
        ledger_now_ms().saturating_sub(1_000),
    );
    assert!(coordinator
        .enqueue_next_dirty_route(data_root, ledger_now_ms())
        .unwrap());
    coordinator
        .run_next(data_root)
        .expect("due internal failure")
}

#[test]
fn automatic_internal_retry_confirms_once_then_pauses_without_losing_last_good() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (_temp, data_root, _source_path, coordinator, _catalog, route, retained_generation) =
        automatic_retry_fixture(failing_executor(Arc::clone(&calls)));

    let first = run_due_failure(&coordinator, &data_root, &route);
    assert_eq!(first.job["automatic_retry"]["state"], "confirming");
    assert_eq!(
        first.job["automatic_retry"]["routes"][route.as_str()]["matching_failures"],
        1
    );
    assert_eq!(first.job["structured_outcome"]["retryable"], true);
    assert!(coordinator
        .next_dirty_route_due_in_ms(ledger_now_ms())
        .is_some());

    let second = run_due_failure(&coordinator, &data_root, &route);
    let terminal = coordinator
        .status(second.job["request_id"].as_str().unwrap())
        .unwrap();
    assert_eq!(
        terminal["automatic_retry"]["state"],
        "paused",
        "first checkpoint={:#}; first error={:#}; second checkpoint={:#}; second error={:#}",
        first.job["automatic_retry"],
        first.job["last_error"],
        terminal["automatic_retry"],
        terminal["last_error"]
    );
    assert_eq!(
        terminal["automatic_retry"]["routes"][route.as_str()]["matching_failures"],
        2
    );
    assert_eq!(terminal["structured_outcome"]["retryable"], false);
    assert_eq!(
        terminal["structured_outcome"]["retry_advice"],
        "inspect_sources"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(!coordinator.has_scheduled_route_work());

    coordinator.schedule_startup_route_reconciliation(
        [route],
        automatic_retry_test_watermark(),
        ledger_now_ms().saturating_sub(1_000),
    );
    assert!(!coordinator
        .enqueue_next_dirty_route(&data_root, u64::MAX)
        .unwrap());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        pin_published_generation(&data_root)
            .unwrap()
            .unwrap()
            .generation_id(),
        retained_generation
    );
}

#[test]
fn automatic_terminal_coverage_retry_confirms_once_then_pauses() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (_temp, data_root, _source_path, coordinator, _catalog, route, _generation) =
        automatic_retry_fixture(coverage_failure_executor(Arc::clone(&calls)));

    let first = run_due_failure(&coordinator, &data_root, &route);
    assert_eq!(
        first.job["structured_outcome"]["code"],
        "all_provider_terminal_coverage_unavailable"
    );
    assert_eq!(first.job["automatic_retry"]["state"], "confirming");
    assert_eq!(
        first.job["automatic_retry"]["reason"],
        "internal_failure_confirmation"
    );

    let second = run_due_failure(&coordinator, &data_root, &route);
    assert_eq!(second.job["automatic_retry"]["state"], "paused");
    assert_eq!(
        second.job["automatic_retry"]["reason"],
        "repeated_internal_failure"
    );
    assert_eq!(second.job["structured_outcome"]["retryable"], false);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(!coordinator.has_scheduled_route_work());
}

#[test]
fn automatic_retry_rearms_for_source_change_and_manual_import() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (_temp, data_root, source_path, coordinator, _catalog, route, _generation) =
        automatic_retry_fixture(failing_executor(Arc::clone(&calls)));
    let _ = run_due_failure(&coordinator, &data_root, &route);
    let _ = run_due_failure(&coordinator, &data_root, &route);

    fs::write(&source_path, b"changed\n").unwrap();
    coordinator.schedule_startup_route_reconciliation(
        [route.clone()],
        automatic_retry_test_watermark(),
        ledger_now_ms().saturating_sub(1_000),
    );
    let changed = run_due_failure(&coordinator, &data_root, &route);
    assert_eq!(changed.job["automatic_retry"]["state"], "confirming");
    assert_eq!(
        changed.job["automatic_retry"]["routes"][route.as_str()]["matching_failures"],
        1
    );
    let _ = run_due_failure(&coordinator, &data_root, &route);

    let manual = manual_all_request_without_catalog(&coordinator, &data_root);
    let manual_run = coordinator
        .run_next(&data_root)
        .expect("explicit import rearm must execute");
    assert_eq!(manual_run.job["request_id"], manual["request_id"]);
    assert_eq!(manual_run.job["automatic_retry"]["state"], "confirming");
    assert_eq!(
        manual_run.job["automatic_retry"]["routes"][route.as_str()]["matching_failures"],
        1
    );
    assert_eq!(manual_run.job["structured_outcome"]["retryable"], true);
    assert_eq!(calls.load(Ordering::SeqCst), 5);
}

#[test]
fn missing_route_observation_rearms_a_paused_automatic_retry() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (_temp, data_root, source_path, coordinator, _catalog, route, _generation) =
        automatic_retry_fixture(failing_executor(Arc::clone(&calls)));
    let _ = run_due_failure(&coordinator, &data_root, &route);
    let _ = run_due_failure(&coordinator, &data_root, &route);

    fs::remove_file(source_path).unwrap();
    coordinator.schedule_startup_route_reconciliation(
        [route.clone()],
        automatic_retry_test_watermark(),
        ledger_now_ms().saturating_sub(1_000),
    );
    let missing = run_due_failure(&coordinator, &data_root, &route);
    assert!(missing.job.get("automatic_retry").is_none());
    assert_eq!(missing.job["structured_outcome"]["retryable"], true);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[test]
fn paused_automatic_retry_survives_restart_and_build_change_rearms() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (_temp, data_root, source_path, coordinator, catalog, route, _generation) =
        automatic_retry_fixture(failing_executor(Arc::clone(&calls)));
    let _ = run_due_failure(&coordinator, &data_root, &route);
    let paused = run_due_failure(&coordinator, &data_root, &route);
    let paused_request_id = paused.job["request_id"].as_str().unwrap().to_owned();
    drop(coordinator);

    let restarted = CoreRefreshEngine::with_executor_and_admitted_routes(
        failing_executor(Arc::clone(&calls)),
        [route.clone()],
    );
    assert!(!restarted
        .recover_interrupted_publication(&data_root)
        .unwrap());
    restarted.install_watch_catalog(catalog.clone());
    restarted.schedule_startup_route_reconciliation(
        [route.clone()],
        automatic_retry_test_watermark(),
        ledger_now_ms().saturating_sub(1_000),
    );
    assert!(!restarted
        .enqueue_next_dirty_route(&data_root, u64::MAX)
        .unwrap());
    assert_eq!(
        restarted.status(&paused_request_id).unwrap()["automatic_retry"]["state"],
        "paused"
    );
    drop(restarted);

    let status_path = daemon_source_backed_refresh_job_path(&data_root);
    let mut durable = read_daemon_job_status(&status_path).unwrap();
    durable["automatic_retry"]["routes"][route.as_str()]["build_version"] =
        json!("previous-test-build");
    write_daemon_job_status(&status_path, &durable).unwrap();
    let upgraded = CoreRefreshEngine::with_executor_and_admitted_routes(
        failing_executor(Arc::clone(&calls)),
        [route.clone()],
    );
    assert!(!upgraded
        .recover_interrupted_publication(&data_root)
        .unwrap());
    upgraded.install_watch_catalog(automatic_retry_catalog(source_path).0);
    upgraded.schedule_startup_route_reconciliation(
        [route.clone()],
        automatic_retry_test_watermark(),
        ledger_now_ms().saturating_sub(1_000),
    );
    let upgraded_failure = run_due_failure(&upgraded, &data_root, &route);
    assert_eq!(
        upgraded_failure.job["automatic_retry"]["state"],
        "confirming"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[test]
fn checkpointless_internal_pause_recovers_as_retryable_work() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (_temp, data_root, _source_path, coordinator, catalog, route, _generation) =
        automatic_retry_fixture(failing_executor(Arc::clone(&calls)));
    let _ = run_due_failure(&coordinator, &data_root, &route);
    let paused = run_due_failure(&coordinator, &data_root, &route);
    let request_id = paused.job["request_id"].as_str().unwrap().to_owned();
    drop(coordinator);

    let status_path = daemon_source_backed_refresh_job_path(&data_root);
    let mut durable = read_daemon_job_status(&status_path).unwrap();
    durable.as_object_mut().unwrap().remove("automatic_retry");
    write_daemon_job_status(&status_path, &durable).unwrap();

    let restarted = CoreRefreshEngine::with_executor_and_admitted_routes(
        failing_executor(calls),
        [route.clone()],
    );
    assert!(!restarted
        .recover_interrupted_publication(&data_root)
        .unwrap());
    restarted.install_watch_catalog(catalog);

    let status = restarted.status(&request_id).unwrap();
    assert_eq!(status["structured_outcome"]["retryable"], true);
    assert_eq!(
        status["structured_outcome"]["retryable_routes"],
        json!([route.as_str()])
    );
    assert_eq!(status["structured_outcome"]["blocked_routes"], json!([]));
    assert!(status.get("automatic_retry").is_none());
    assert!(restarted.has_scheduled_route_work());
    let persisted = read_daemon_job_status(&status_path).unwrap();
    assert_eq!(persisted["structured_outcome"]["retryable"], true);
}

fn selective_automatic_retry_executor(
    paused_route: SourceRouteIdentity,
    calls: Arc<AtomicUsize>,
) -> Arc<dyn SourceBackedRefreshExecutor> {
    Arc::new(move |execution: SourceBackedRefreshExecution<'_>| {
        calls.fetch_add(1, Ordering::SeqCst);
        let selected = execution.admitted_refresh().exact_routes().clone();
        if selected.contains(&paused_route) {
            return Err(anyhow!("stable internal refresh fixture failure"));
        }
        publish_selected_routes_with_request_metadata(&execution, &selected)
    })
}

fn publish_selected_routes_with_request_metadata(
    execution: &SourceBackedRefreshExecution<'_>,
    selected: &BTreeSet<SourceRouteIdentity>,
) -> Result<SourceBackedRefreshPublication> {
    let request_id = execution.request_id.to_owned();
    let operation = execution.operation;
    let scope = execution.admitted_refresh().publication_scope().clone();
    let previous_generation = open_verified_index(execution.index_root)
        .ok()
        .map(|index| index.generation_id().to_owned());
    let metadata_routes = selected.clone();
    let mut writer =
        ctx_history_index::GenerationWriter::open(execution.index_root, WriterOptions::default())?
            .into_writer()
            .map_err(crate::committed_generation_recovery_error)?;
    if previous_generation.is_some() {
        let source = publication_pin_source_with_anchor(0x93);
        writer.begin_source(source.clone())?;
        writer.add_core_record(publication_pin_record(&source))?;
        writer.certify_source(publication_rejection_certificate(&source))?;
    }
    let published = writer.commit_with_publication_metadata(
        |_| true,
        move |context| {
            let mut publication = empty_test_publication(context.generation_id().to_owned());
            publication.current =
                SourceBackedRefreshCurrent::from_sources(&context.manifest().sources, 0)
                    .map_err(|error| IndexError::PublicationMetadata(format!("{error:#}")))?;
            publication.certified_source_count = publication.current.source_count;
            publication.certified_source_bytes = publication.current.certified_source_bytes;
            publication.route_results = metadata_routes
                .iter()
                .map(|route| {
                    SourceBackedRefreshRouteResult::succeeded(route.as_str().to_owned(), true)
                })
                .collect();
            let receipt = SourceBackedRefreshReceipt::from_verified_publication(
                previous_generation.clone(),
                context.generation_id().to_owned(),
                &publication,
            )
            .map_err(|error| IndexError::PublicationMetadata(format!("{error:#}")))?;
            SourceBackedPublicationMetadata {
                version: SOURCE_REFRESH_PUBLICATION_METADATA_VERSION,
                request_id: request_id.clone(),
                operation,
                refresh_scope: scope.clone(),
                receipt: receipt.to_json(),
                route_observations: BTreeMap::new(),
                route_controls: BTreeMap::new(),
            }
            .encode()
        },
    )?;
    let generation_id = published.receipt().generation_id.clone();
    let (_, _, verified_index) = published.into_parts();
    let mut publication = empty_test_publication(generation_id);
    publication.current =
        SourceBackedRefreshCurrent::from_sources(&verified_index.manifest().sources, 0)?;
    publication.certified_source_count = publication.current.source_count;
    publication.certified_source_bytes = publication.current.certified_source_bytes;
    publication.route_results = selected
        .iter()
        .map(|route| SourceBackedRefreshRouteResult::succeeded(route.as_str().to_owned(), true))
        .collect();
    publication.verified_index = Some(Arc::new(verified_index));
    Ok(publication)
}

#[test]
fn upgrade_rearms_a_paused_route_carried_by_a_later_healthy_publication() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    publish_pin_source(
        &source_backed_index_root(&data_root),
        publication_pin_source(),
    );
    let paused_path = temp.path().join("paused.jsonl");
    let healthy_path = temp.path().join("healthy.jsonl");
    fs::write(&paused_path, b"paused\n").unwrap();
    fs::write(&healthy_path, b"healthy\n").unwrap();
    let (paused_source, paused_route) = automatic_retry_route(paused_path);
    let (healthy_source, healthy_route) = automatic_retry_route_for_provider(
        healthy_path,
        CaptureProvider::Claude,
        "claude_projects_jsonl_tree",
    );
    let mut registry = SourceBackedProviderRegistry::new();
    registry.register(paused_source);
    registry.register(healthy_source);
    let catalog = registry.watch_catalog();
    let routes = BTreeSet::from([paused_route.clone(), healthy_route.clone()]);
    let calls = Arc::new(AtomicUsize::new(0));
    let coordinator = CoreRefreshEngine::with_executor_and_admitted_routes(
        selective_automatic_retry_executor(paused_route.clone(), Arc::clone(&calls)),
        routes.clone(),
    );
    coordinator.install_watch_catalog(catalog.clone());
    let _ = run_due_failure(&coordinator, &data_root, &paused_route);
    let _ = run_due_failure(&coordinator, &data_root, &paused_route);
    coordinator.schedule_startup_route_reconciliation(
        [healthy_route.clone()],
        automatic_retry_test_watermark(),
        ledger_now_ms().saturating_sub(1_000),
    );
    assert!(coordinator
        .enqueue_next_dirty_route(&data_root, ledger_now_ms())
        .unwrap());
    let healthy = coordinator.run_next(&data_root).unwrap();
    assert!(!healthy.failed, "{:#}", healthy.job);
    assert_eq!(healthy.job["automatic_retry"]["state"], "paused");
    drop(coordinator);

    let status_path = daemon_source_backed_refresh_job_path(&data_root);
    let mut durable = read_daemon_job_status(&status_path).unwrap();
    durable["automatic_retry"]["routes"][paused_route.as_str()]["build_version"] =
        json!("previous-test-build");
    write_daemon_job_status(&status_path, &durable).unwrap();

    let upgraded = CoreRefreshEngine::with_executor_and_admitted_routes(
        selective_automatic_retry_executor(paused_route.clone(), Arc::clone(&calls)),
        routes,
    );
    assert!(!upgraded
        .recover_interrupted_publication(&data_root)
        .unwrap());
    upgraded.install_watch_catalog(catalog);
    assert!(
        upgraded.has_scheduled_route_work(),
        "scheduled={:?}; status={:#?}",
        upgraded.scheduled_route_ids_for_test(),
        upgraded.status(healthy.job["request_id"].as_str().unwrap())
    );
    assert!(upgraded
        .enqueue_next_dirty_route(&data_root, ledger_now_ms())
        .unwrap());
    let retried = upgraded.run_next(&data_root).unwrap();
    assert_eq!(retried.job["automatic_retry"]["state"], "confirming");
    assert_eq!(
        retried.job["automatic_retry"]["routes"][paused_route.as_str()]["matching_failures"],
        1
    );
}

#[test]
fn newer_event_during_confirmation_attempt_is_not_paused_by_stale_admission() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let executor_calls = Arc::clone(&calls);
    let executor_release = Mutex::new(release_rx);
    let executor: Arc<dyn SourceBackedRefreshExecutor> =
        Arc::new(move |_: SourceBackedRefreshExecution<'_>| {
            if executor_calls.fetch_add(1, Ordering::SeqCst) == 1 {
                entered_tx.send(()).expect("signal confirmation attempt");
                executor_release
                    .lock()
                    .unwrap()
                    .recv_timeout(StdDuration::from_secs(5))
                    .expect("release confirmation attempt");
            }
            Err(anyhow!("stable internal refresh fixture failure"))
        });
    let (_temp, data_root, _source_path, coordinator, _catalog, route, _generation) =
        automatic_retry_fixture(executor);
    let coordinator = Arc::new(coordinator);
    let _ = run_due_failure(&coordinator, &data_root, &route);
    coordinator.schedule_startup_route_reconciliation(
        [route.clone()],
        automatic_retry_test_watermark(),
        ledger_now_ms().saturating_sub(1_000),
    );
    assert!(coordinator
        .enqueue_next_dirty_route(&data_root, ledger_now_ms())
        .unwrap());
    let runner = Arc::clone(&coordinator);
    let run_root = data_root.clone();
    let running = std::thread::spawn(move || runner.run_next(&run_root).unwrap());
    entered_rx
        .recv_timeout(StdDuration::from_secs(5))
        .expect("confirmation attempt must reach executor");
    coordinator.schedule_startup_route_reconciliation(
        [route.clone()],
        automatic_retry_test_watermark(),
        ledger_now_ms().saturating_sub(1_000),
    );
    release_tx.send(()).expect("release confirmation attempt");
    let stale = running.join().unwrap();
    let terminal = coordinator
        .status(stale.job["request_id"].as_str().unwrap())
        .unwrap();
    assert_eq!(terminal["structured_outcome"]["retryable"], true);
    assert!(terminal.get("automatic_retry").is_none());
    assert!(!coordinator.route_is_permanently_blocked_for_test(&route));
    coordinator.schedule_startup_route_reconciliation(
        [route.clone()],
        automatic_retry_test_watermark(),
        ledger_now_ms().saturating_sub(1_000),
    );
    assert!(coordinator
        .enqueue_next_dirty_route(&data_root, ledger_now_ms())
        .unwrap());
}

#[test]
fn differing_catalog_authority_queues_one_successor_behind_a_running_refresh() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let first_authority =
        test_exact_catalog_authority(&data_root, &temp.path().join("exact-source-1"));
    let (gate, executor_started, executor_release) = RunningRefreshGate::new();
    let executor_release = Mutex::new(executor_release);
    let executor_calls = AtomicUsize::new(0);
    let coordinator = Arc::new(CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            if executor_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                executor_started.send(()).expect("signal running refresh");
                let _ = executor_release.lock().unwrap().recv();
            }
            CaptureOwnedSourceBackedRefreshExecutor.refresh(execution)
        },
    )));
    let request = |authority: &ExplicitSourceCatalogAuthority, logical_request_id: &str| {
        coordinator
            .handle_ipc_request(
                &data_root,
                &json!({
                    "schema_version": 1,
                    "op": SOURCE_REFRESH_REQUEST_OP,
                    "request_id": logical_request_id,
                    "mode": "wait",
                    "operation": "import",
                    "explicit_source_catalog": authority.to_json(),
                }),
            )
            .unwrap()
            .expect("source refresh response")
    };

    let first = request(&first_authority, &Uuid::now_v7().to_string());
    assert_eq!(first["refresh_intent"]["selection"]["kind"], "exact_source");
    let first_request_id = request_id(&first);
    assert!(coordinator
        .prepare_next_pending_admission(&data_root)
        .unwrap());

    let (second, second_replay, second_authority) = std::thread::scope(|scope| {
        let runner = Arc::clone(&coordinator);
        let runner_root = data_root.clone();
        scope.spawn(move || {
            let first_run = runner
                .run_next(&runner_root)
                .expect("running first catalog refresh");
            assert!(!first_run.failed, "{:#}", first_run.job);
        });
        gate.wait_until_started();

        let second_authority =
            test_exact_catalog_authority(&data_root, &temp.path().join("exact-source-2"));
        assert_ne!(first_authority, second_authority);
        let second_logical_request_id = Uuid::now_v7().to_string();
        let second = request(&second_authority, &second_logical_request_id);
        let second_replay = request(&second_authority, &second_logical_request_id);
        gate.release();
        (second, second_replay, second_authority)
    });

    let second_request_id = request_id(&second);
    assert_ne!(first_request_id, second_request_id);
    assert_eq!(request_id(&second_replay), second_request_id);
    assert_eq!(second_replay["coalesced_requests"], 0);
    assert_eq!(second["request_state"], "admission_pending");
    assert_eq!(
        coordinator.status(&first_request_id).unwrap()["request_state"],
        "published"
    );
    let pending_second = coordinator.status(&second_request_id).unwrap();
    assert_eq!(pending_second["request_state"], "admission_pending");
    let first_generation = coordinator.status(&first_request_id).unwrap()["published_generation"]
        .as_str()
        .expect("first exact generation")
        .to_owned();
    assert!(coordinator
        .prepare_next_pending_admission(&data_root)
        .unwrap());
    let queued_second = coordinator.status(&second_request_id).unwrap();
    assert_eq!(queued_second["request_state"], "queued");
    assert_eq!(queued_second["previous_generation"], first_generation);

    let second_run = coordinator.run_next(&data_root).unwrap();
    assert!(!second_run.failed);
    assert!(!coordinator.has_pending_request());
    let published_second = coordinator.status(&second_request_id).unwrap();
    assert_eq!(published_second["request_state"], "published");
    assert_eq!(
        ExplicitSourceCatalogAuthority::from_json(
            &published_second["receipt"]["published_explicit_source_catalog"]
        )
        .unwrap(),
        second_authority
    );
}

#[test]
fn active_and_pending_refreshes_are_bounded_with_a_typed_busy_response() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let coordinator = CoreRefreshEngine::new();
    let request = |revision: u64| {
        let authority = test_exact_catalog_authority(
            &data_root,
            &temp.path().join(format!("exact-source-{revision}")),
        );
        coordinator
            .handle_ipc_request(
                &data_root,
                &json!({
                    "schema_version": 1,
                    "op": SOURCE_REFRESH_REQUEST_OP,
                    "mode": "wait",
                    "operation": "import",
                    "explicit_source_catalog": authority.to_json(),
                }),
            )
            .unwrap()
            .expect("source refresh response")
    };

    let accepted = (1..=SOURCE_REFRESH_ACTIVE_PENDING_LIMIT)
        .map(|revision| request(u64::try_from(revision).unwrap()))
        .collect::<Vec<_>>();
    assert!(accepted.iter().all(|response| response["ok"] == true));
    assert_eq!(
        accepted
            .iter()
            .map(request_id)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        SOURCE_REFRESH_ACTIVE_PENDING_LIMIT
    );

    let busy = request(99);
    assert_eq!(busy["ok"], false);
    assert_eq!(busy["status"], "busy");
    assert_eq!(busy["error_code"], "source_refresh_queue_full");
    assert_eq!(busy["reason"], "queue_full");
    assert_eq!(busy["retryable"], true);
    assert_eq!(
        busy["active_pending_requests"],
        SOURCE_REFRESH_ACTIVE_PENDING_LIMIT
    );
    assert_eq!(
        busy["max_active_pending_requests"],
        SOURCE_REFRESH_ACTIVE_PENDING_LIMIT
    );
    assert!(busy.get("request_id").is_none());
}

#[test]
fn terminal_history_is_trimmed_independently_from_inflight_capacity() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let coordinator = CoreRefreshEngine::new();
    let total = SOURCE_REFRESH_ATTEMPT_HISTORY + 3;
    let mut request_ids = Vec::with_capacity(total);

    for generation in 0..total {
        let previous = format!("generation-{generation}");
        let published = format!("generation-{}", generation.saturating_add(1));
        let request = coordinator.enqueue(Some(previous));
        let queued_request_id = request_id(&request);
        coordinator
            .complete_pending_admission_for_test(&data_root, &queued_request_id, BTreeMap::new())
            .unwrap();
        request_ids.push(queued_request_id);
        let run = coordinator
            .run_next_with(
                |_, _| Ok(test_publication(published.clone())),
                || Ok(Some(published.clone())),
                |_| Ok(()),
                |_| Ok(()),
            )
            .expect("queued refresh");
        assert!(!run.failed);
    }

    assert!(request_ids[..3]
        .iter()
        .all(|request_id| coordinator.status(request_id).is_none()));
    assert!(request_ids[3..]
        .iter()
        .all(|request_id| coordinator.status(request_id).is_some()));

    let next = coordinator.enqueue(Some(format!("generation-{total}")));
    let next_request_id = request_id(&next);
    coordinator
        .complete_pending_admission_for_test(&data_root, &next_request_id, BTreeMap::new())
        .unwrap();
    assert_eq!(
        coordinator.status(&next_request_id).unwrap()["request_state"],
        "queued"
    );
    assert!(coordinator.has_pending_request());
}

#[test]
fn production_run_persists_discovering_before_executor_entry() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let observed = Arc::new(AtomicBool::new(false));
    let observed_from_executor = Arc::clone(&observed);
    let coordinator = CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            let job =
                read_daemon_job_status(&daemon_source_backed_refresh_job_path(execution.data_root))
                    .expect("running source refresh status");
            assert_eq!(job["request_state"], "running");
            assert_eq!(job["progress"]["phase"], "discovering");
            assert_eq!(job["progress"]["total_sources_known"], false);
            assert!(job["progress"]["current_source"].is_null());
            assert!(job["progress"]["completed_records"].is_null());
            assert!(job["progress"]["completed_bytes"].is_null());
            assert!(job["progress"]["current_source_progress"].is_null());
            observed_from_executor.store(true, Ordering::SeqCst);
            Err(anyhow!("stop after observing persisted discovery phase"))
        },
    ));
    let _request = manual_all_request_without_catalog(&coordinator, &data_root);

    let run = coordinator.run_next(&data_root).expect("queued refresh");
    assert!(run.failed);
    assert!(observed.load(Ordering::SeqCst));
}

#[test]
fn default_executor_uses_capture_owned_execution() {
    let coordinator = CoreRefreshEngine::new();
    assert_eq!(
        coordinator.executor.implementation_name(),
        std::any::type_name::<CaptureOwnedSourceBackedRefreshExecutor>()
    );
}
