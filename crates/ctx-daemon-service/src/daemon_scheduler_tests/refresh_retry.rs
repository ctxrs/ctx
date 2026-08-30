use super::*;
use ctx_client_observability::analytics::PublicEventV1;

#[path = "refresh_retry/durable_retry.rs"]
mod durable_retry;

fn enqueue_synthetic_refresh_successor(
    coordinator: &CoreRefreshEngine,
    data_root: &Path,
    revision: u64,
) -> Value {
    coordinator
        .enqueue_fresh_catalog_demand_for_test(
            data_root,
            None,
            uuid::Uuid::now_v7().to_string(),
            ctx_history_refresh::explicit_source_catalog_authority_for_test(revision),
        )
        .expect("synthetic refresh successor")
}

fn make_history_retry_due(runtime: &mut DaemonRuntime) {
    runtime.history_retry.retry_not_before = Some(std::time::Instant::now());
    runtime.history_retry.retry_not_before_at_ms =
        Some(ctx_history_core::utc_now().timestamp_millis());
}

fn route_identity(byte: u8) -> SourceRouteIdentity {
    SourceRouteIdentity::from_sha256(format!("{byte:02x}").repeat(32)).unwrap()
}

fn source_refresh_only_runtime() -> DaemonRuntime {
    let mut runtime = DaemonRuntime::default();
    runtime.config.daemon.mode = DaemonMode::SourceRefreshOnly;
    runtime
}

fn run_source_refresh_cycle(
    data_root: &Path,
    runtime: &mut DaemonRuntime,
    coordinator: &CoreRefreshEngine,
) -> crate::daemon::DaemonIteration {
    run_daemon_scheduler_cycle_with_activity(
        &daemon_args(),
        data_root,
        runtime,
        None,
        false,
        None,
        Some(coordinator),
    )
    .unwrap()
}

#[test]
fn hot_route_failure_retries_exact_after_cooldown_while_blocked_route_stays_idle() {
    for (byte, kind, retryable) in [
        (0xa1, SourceBackedRouteErrorKind::SourceChanged, true),
        (0xb1, SourceBackedRouteErrorKind::InvalidSource, false),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
        publish_empty_core_generation(&data_root);
        let route = route_identity(byte);
        let scopes = Arc::new(Mutex::new(Vec::new()));
        let observed_scopes = Arc::clone(&scopes);
        let coordinator = CoreRefreshEngine::with_executor(Arc::new(
            move |execution: SourceBackedRefreshExecution<'_>| {
                observed_scopes
                    .lock()
                    .unwrap()
                    .push(execution.admitted_refresh().publication_scope().clone());
                Err(SourceBackedRouteError::new(kind, "injected route failure").into())
            },
        ));
        coordinator.initialize_watch_route_authority(BTreeSet::from([route.clone()]));
        coordinator.record_watch_routes(
            [(route.clone(), EventWatermark::new(byte.into(), 1))],
            super::super::source_route_ledger_now_ms().saturating_sub(1_000),
        );
        let mut runtime = source_refresh_only_runtime();
        let mut failed_iteration = run_source_refresh_cycle(&data_root, &mut runtime, &coordinator);
        assert!(failed_iteration.failed);
        let events = crate::daemon::daemon_iteration_events_without_telemetry(
            &mut failed_iteration,
            std::time::Duration::from_millis(1),
        );
        assert!(matches!(
            events.as_slice(),
            [PublicEventV1::ProviderRefreshCompleted(_)]
        ));
        assert_eq!(runtime.history_retry.consecutive_failures, 0);
        assert_eq!(
            scopes.lock().unwrap().as_slice(),
            &[SourceBackedRefreshScope::Exact(BTreeSet::from([
                route.clone()
            ]))]
        );
        let terminal = read_daemon_job_status(&daemon_core_refresh_job_path(&data_root)).unwrap();
        assert_eq!(terminal["structured_outcome"]["retryable"], retryable);
        let field = if retryable {
            "retryable_routes"
        } else {
            "blocked_routes"
        };
        assert_eq!(
            terminal["structured_outcome"][field],
            json!([route.as_str()])
        );
        assert!(terminal.get("retry_after_ms").is_none());

        if retryable {
            let now = super::super::source_route_ledger_now_ms();
            let delay = coordinator.next_dirty_route_due_in_ms(now).unwrap();
            assert!(delay > 0);
            assert!(!coordinator
                .enqueue_next_scheduled_refresh(&data_root, now + delay - 1)
                .unwrap());
            assert!(coordinator
                .enqueue_next_scheduled_refresh(&data_root, now + delay)
                .unwrap());
            let queued = read_daemon_job_status(&daemon_core_refresh_job_path(&data_root)).unwrap();
            assert_eq!(queued["refresh_scope"]["kind"], "exact");
            assert_eq!(queued["refresh_scope"]["routes"], json!([route.as_str()]));
        } else {
            assert!(coordinator.next_dirty_route_due_in_ms(u64::MAX).is_none());
            assert!(!coordinator
                .enqueue_next_scheduled_refresh(&data_root, u64::MAX)
                .unwrap());
            assert!(!coordinator.has_pending_request());
        }
    }
}

#[test]
fn mixed_route_dispositions_schedule_only_retryable_routes() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    publish_empty_core_generation(&data_root);
    let retryable_route = route_identity(0xc1);
    let blocked_route = route_identity(0xc2);
    let executor_retryable = retryable_route.clone();
    let executor_blocked = blocked_route.clone();
    let coordinator = CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            let route_results = [
                (&executor_retryable, "unavailable"),
                (&executor_blocked, "incompatible"),
            ]
            .into_iter()
            .map(|(route, class)| {
                let mut result =
                    SourceBackedRefreshRouteResult::succeeded(route.as_str().to_owned(), false);
                result.source_failure_total = 1;
                result.source_retryable_failure_total = usize::from(class == "unavailable");
                result.source_failures = vec![SourceBackedRefreshSourceFailure {
                    route_identity: route.as_str().to_owned(),
                    source_identity: "dd".repeat(32),
                    provider: "fixture".to_owned(),
                    class: class.to_owned(),
                    carried_forward: true,
                    source_selector: "fixture source".to_owned(),
                    detail: "injected route disposition".to_owned(),
                }];
                result
            })
            .collect();
            Ok(publish_empty_authoritative_generation_with_route_results(
                &execution,
                Some(route_results),
            ))
        },
    ));
    coordinator.initialize_watch_route_authority(BTreeSet::from([
        retryable_route.clone(),
        blocked_route.clone(),
    ]));
    coordinator.record_watch_routes(
        [
            (retryable_route.clone(), EventWatermark::new(3, 1)),
            (blocked_route.clone(), EventWatermark::new(3, 1)),
        ],
        super::super::source_route_ledger_now_ms().saturating_sub(1_000),
    );
    let mut runtime = source_refresh_only_runtime();

    let mut completed = run_source_refresh_cycle(&data_root, &mut runtime, &coordinator);

    assert!(!completed.failed);
    let events = crate::daemon::daemon_iteration_events_without_telemetry(
        &mut completed,
        std::time::Duration::from_millis(1),
    );
    assert!(matches!(
        events.as_slice(),
        [PublicEventV1::ProviderRefreshCompleted(_)]
    ));
    assert_eq!(runtime.history_retry.consecutive_failures, 0);
    let terminal = read_daemon_job_status(&daemon_core_refresh_job_path(&data_root)).unwrap();
    assert_eq!(
        terminal["structured_outcome"]["retryable_routes"],
        json!([retryable_route.as_str()])
    );
    assert_eq!(
        terminal["structured_outcome"]["blocked_routes"],
        json!([blocked_route.as_str()])
    );
    let now = super::super::source_route_ledger_now_ms();
    let delay = coordinator
        .next_dirty_route_due_in_ms(now)
        .expect("mixed outcome retains retryable route");
    assert!(coordinator
        .enqueue_next_scheduled_refresh(&data_root, now.saturating_add(delay))
        .unwrap());
    let queued = read_daemon_job_status(&daemon_core_refresh_job_path(&data_root)).unwrap();
    assert_eq!(
        queued["refresh_scope"]["routes"],
        json!([retryable_route.as_str()])
    );
}

#[test]
fn unclaimed_source_blocks_only_its_culprit_and_retries_the_peer_route() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    publish_empty_core_generation(&data_root);
    let culprit = route_identity(0xd1);
    let peer = route_identity(0xd2);
    let executor_culprit = culprit.clone();
    let coordinator = CoreRefreshEngine::with_executor(Arc::new(
        move |_execution: SourceBackedRefreshExecution<'_>| {
            Err(SourceBackedCoordinatorError::UnclaimedBaseSource {
                source_id: "fixture-source".to_owned(),
                route_identity: executor_culprit.clone(),
                route_failures: Vec::new(),
                logical_source_failures: Default::default(),
            }
            .into())
        },
    ));
    coordinator.initialize_watch_route_authority(BTreeSet::from([culprit.clone(), peer.clone()]));
    coordinator.record_watch_routes(
        [
            (culprit.clone(), EventWatermark::new(4, 1)),
            (peer.clone(), EventWatermark::new(4, 1)),
        ],
        super::super::source_route_ledger_now_ms().saturating_sub(1_000),
    );
    let mut runtime = source_refresh_only_runtime();

    let mut failed = run_source_refresh_cycle(&data_root, &mut runtime, &coordinator);

    assert!(failed.failed);
    let events = crate::daemon::daemon_iteration_events_without_telemetry(
        &mut failed,
        std::time::Duration::from_millis(1),
    );
    assert!(matches!(
        events.as_slice(),
        [PublicEventV1::ProviderRefreshCompleted(_)]
    ));
    assert_eq!(runtime.history_retry.consecutive_failures, 0);
    let terminal = read_daemon_job_status(&daemon_core_refresh_job_path(&data_root)).unwrap();
    assert_eq!(terminal["structured_outcome"]["code"], "source_unclaimed");
    assert_eq!(terminal["structured_outcome"]["class"], "coverage");
    assert_eq!(terminal["structured_outcome"]["retryable"], true);
    assert_eq!(
        terminal["structured_outcome"]["retry_advice"],
        "retry_retryable_routes_and_inspect_blocked"
    );
    assert_eq!(
        terminal["structured_outcome"]["retryable_routes"],
        json!([peer.as_str()])
    );
    assert_eq!(
        terminal["structured_outcome"]["blocked_routes"],
        json!([culprit.as_str()])
    );
    assert!(ctx_history_refresh::RefreshStatus::classify_schema_v1(&terminal).is_ok());

    let now = super::super::source_route_ledger_now_ms();
    let delay = coordinator
        .next_dirty_route_due_in_ms(now)
        .expect("unpublished peer remains retryable");
    assert!(coordinator
        .enqueue_next_scheduled_refresh(&data_root, now.saturating_add(delay))
        .unwrap());
    let queued = read_daemon_job_status(&daemon_core_refresh_job_path(&data_root)).unwrap();
    assert_eq!(queued["refresh_scope"]["kind"], "exact");
    assert_eq!(queued["refresh_scope"]["routes"], json!([peer.as_str()]));
}

#[test]
fn terminal_admission_fence_failure_releases_root_before_exact_dirty_route_runs() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    publish_empty_core_generation(&data_root);
    let dirty_route = route_identity(0xd1);
    let executor_route = dirty_route.clone();
    let scopes = Arc::new(Mutex::new(Vec::new()));
    let observed_scopes = Arc::clone(&scopes);
    let fence_calls = Arc::new(AtomicUsize::new(0));
    let observed_fence_calls = Arc::clone(&fence_calls);
    let coordinator = CoreRefreshEngine::with_runtime_for_test(
        Arc::new(move |execution: SourceBackedRefreshExecution<'_>| {
            observed_scopes
                .lock()
                .unwrap()
                .push(execution.admitted_refresh().publication_scope().clone());
            assert_eq!(
                execution.admitted_refresh().publication_scope(),
                SourceBackedRefreshScope::Exact(BTreeSet::from([executor_route.clone()]))
            );
            let mut publication = publish_empty_authoritative_generation(&execution);
            publication.route_results = vec![SourceBackedRefreshRouteResult::succeeded(
                executor_route.as_str().to_owned(),
                true,
            )];
            Ok(publication)
        }),
        Arc::new(move |_data_root, _catalog| {
            if observed_fence_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                anyhow::bail!("injected admission-fence failure");
            }
            Ok(BTreeMap::new())
        }),
        Arc::new(write_daemon_job_status),
    );
    coordinator.initialize_watch_route_authority(BTreeSet::from([dirty_route.clone()]));
    let request_id = "019fcaaa-0000-7000-8000-0000000002b1";
    let admitted = coordinator
        .handle_ipc_request(
            &data_root,
            &json!({
                "schema_version": 1,
                "op": "source_refresh_request",
                "request_id": request_id,
                "mode": "wait",
                "operation": "refresh",
                "fresh_after_admitted_snapshot": true,
            }),
        )
        .unwrap()
        .expect("durable pending admission");
    assert_eq!(admitted["request_state"], "admission_pending");
    let mut runtime = source_refresh_only_runtime();

    let failed = run_source_refresh_cycle(&data_root, &mut runtime, &coordinator);

    assert!(failed.failed);
    assert_eq!(runtime.history_retry.consecutive_failures, 0);
    assert_eq!(fence_calls.load(Ordering::SeqCst), 1);
    assert!(scopes.lock().unwrap().is_empty());
    assert_eq!(coordinator.pending_scheduler_retry_root_for_test(), None);
    let terminal = coordinator
        .handle_ipc_request(
            &data_root,
            &json!({
                "op": "source_refresh_status",
                "request_id": request_id,
            }),
        )
        .unwrap()
        .expect("terminal failed admission");
    assert_eq!(terminal["request_state"], "failed");
    assert_eq!(terminal["logical_phase"], "terminal");
    assert_eq!(
        terminal["structured_outcome"]["code"],
        "source_refresh_admission_failed"
    );
    assert_eq!(
        terminal["structured_outcome"]["retry_advice"],
        "retry_admission"
    );
    coordinator.record_watch_routes(
        [(dirty_route.clone(), EventWatermark::new(7, 1))],
        super::super::source_route_ledger_now_ms().saturating_sub(1_000),
    );
    assert_eq!(
        coordinator.scheduled_route_ids_for_test(),
        BTreeSet::from([dirty_route.clone()])
    );
    assert!(coordinator
        .enqueue_next_scheduled_refresh(&data_root, u64::MAX)
        .unwrap());
    let queued = read_daemon_job_status(&daemon_core_refresh_job_path(&data_root)).unwrap();
    assert_eq!(queued["refresh_scope"]["kind"], "exact");
    assert_eq!(
        queued["refresh_scope"]["routes"],
        json!([dirty_route.as_str()])
    );

    let exact = run_source_refresh_cycle(&data_root, &mut runtime, &coordinator);

    assert!(!exact.failed);
    assert_eq!(
        scopes.lock().unwrap().as_slice(),
        &[SourceBackedRefreshScope::Exact(BTreeSet::from([
            dirty_route
        ]))]
    );
    let replay = coordinator
        .handle_ipc_request(
            &data_root,
            &json!({
                "op": "source_refresh_status",
                "request_id": request_id,
            }),
        )
        .unwrap()
        .expect("failed admission remains replayable");
    assert_eq!(replay, terminal);
}

#[test]
fn persistent_admission_status_failure_uses_scheduler_backoff() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let fence_calls = Arc::new(AtomicUsize::new(0));
    let observed_fence_calls = Arc::clone(&fence_calls);
    let queued_writes = Arc::new(AtomicUsize::new(0));
    let observed_queued_writes = Arc::clone(&queued_writes);
    let coordinator = CoreRefreshEngine::with_runtime_for_test(
        Arc::new(|_execution: SourceBackedRefreshExecution<'_>| {
            panic!("admission persistence failure must stop before execution")
        }),
        Arc::new(move |_data_root, _catalog| {
            observed_fence_calls.fetch_add(1, Ordering::SeqCst);
            Ok(BTreeMap::new())
        }),
        Arc::new(move |path, job| {
            if job.get("request_state").and_then(Value::as_str) == Some("queued") {
                observed_queued_writes.fetch_add(1, Ordering::SeqCst);
                anyhow::bail!("injected persistent admission status failure");
            }
            write_daemon_job_status(path, job)
        }),
    );
    let request_id = "019fcaaa-0000-7000-8000-0000000002b0";
    coordinator
        .handle_ipc_request(
            &data_root,
            &json!({
                "schema_version": 1,
                "op": "source_refresh_request",
                "request_id": request_id,
                "mode": "wait",
                "operation": "refresh",
                "fresh_after_admitted_snapshot": true,
            }),
        )
        .unwrap()
        .expect("durable pending admission");
    let mut runtime = DaemonRuntime::default();
    runtime.config.daemon.mode = DaemonMode::SourceRefreshOnly;

    let first = run_daemon_scheduler_cycle_with_activity(
        &daemon_args(),
        &data_root,
        &mut runtime,
        None,
        false,
        None,
        Some(&coordinator),
    )
    .unwrap();
    assert!(first.failed);
    assert_eq!(runtime.history_retry.consecutive_failures, 1);
    assert_eq!(fence_calls.load(Ordering::SeqCst), 1);
    assert_eq!(queued_writes.load(Ordering::SeqCst), 1);

    let deferred = run_daemon_scheduler_cycle_with_activity(
        &daemon_args(),
        &data_root,
        &mut runtime,
        None,
        false,
        None,
        Some(&coordinator),
    )
    .unwrap();
    assert!(!deferred.did_work);
    assert_eq!(fence_calls.load(Ordering::SeqCst), 1);
    assert_eq!(queued_writes.load(Ordering::SeqCst), 1);

    make_history_retry_due(&mut runtime);
    let retry = run_daemon_scheduler_cycle_with_activity(
        &daemon_args(),
        &data_root,
        &mut runtime,
        None,
        false,
        None,
        Some(&coordinator),
    )
    .unwrap();
    assert!(retry.failed);
    assert_eq!(runtime.history_retry.consecutive_failures, 2);
    assert_eq!(fence_calls.load(Ordering::SeqCst), 2);
    assert_eq!(queued_writes.load(Ordering::SeqCst), 2);
}

#[test]
fn persistent_terminal_status_failure_retries_without_reexecution_or_hot_spin() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let executions = Arc::new(AtomicUsize::new(0));
    let observed_executions = Arc::clone(&executions);
    let executor = Arc::new(move |execution: SourceBackedRefreshExecution<'_>| {
        observed_executions.fetch_add(1, Ordering::SeqCst);
        Ok(publish_empty_authoritative_generation(&execution))
    });
    let terminal_writes = Arc::new(AtomicUsize::new(0));
    let observed_terminal_writes = Arc::clone(&terminal_writes);
    let writer = Arc::new(move |path: &Path, job: &Value| {
        if job.get("request_state").and_then(Value::as_str) == Some("published") {
            observed_terminal_writes.fetch_add(1, Ordering::SeqCst);
            anyhow::bail!("injected persistent terminal status failure");
        }
        write_daemon_job_status(path, job)
    });
    let coordinator = CoreRefreshEngine::with_status_writer_for_test(executor, writer);
    let periodic = coordinator.enqueue_periodic(&data_root).unwrap();
    coordinator
        .complete_pending_admission_for_test(
            &data_root,
            periodic["request_id"].as_str().unwrap(),
            BTreeMap::new(),
        )
        .unwrap();
    let mut runtime = DaemonRuntime::default();
    runtime.config.daemon.mode = DaemonMode::SourceRefreshOnly;

    let first = run_daemon_scheduler_cycle_with_activity(
        &daemon_args(),
        &data_root,
        &mut runtime,
        None,
        false,
        None,
        Some(&coordinator),
    )
    .unwrap();
    assert!(first.failed);
    assert_eq!(runtime.history_retry.consecutive_failures, 1);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert_eq!(terminal_writes.load(Ordering::SeqCst), 1);

    let deferred = run_daemon_scheduler_cycle_with_activity(
        &daemon_args(),
        &data_root,
        &mut runtime,
        None,
        false,
        None,
        Some(&coordinator),
    )
    .unwrap();
    assert!(!deferred.did_work);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert_eq!(terminal_writes.load(Ordering::SeqCst), 1);

    make_history_retry_due(&mut runtime);
    let retry = run_daemon_scheduler_cycle_with_activity(
        &daemon_args(),
        &data_root,
        &mut runtime,
        None,
        false,
        None,
        Some(&coordinator),
    )
    .unwrap();
    assert!(retry.failed);
    assert_eq!(runtime.history_retry.consecutive_failures, 2);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert_eq!(terminal_writes.load(Ordering::SeqCst), 2);
}

#[test]
fn scheduler_retries_post_route_finalization_once_and_notifies_generation_once() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let executions = Arc::new(AtomicUsize::new(0));
    let execution_count = Arc::clone(&executions);
    let executor = Arc::new(move |execution: SourceBackedRefreshExecution<'_>| {
        execution_count.fetch_add(1, Ordering::SeqCst);
        let commit = GenerationWriter::open(execution.index_root, WriterOptions::default())?
            .into_writer()
            .map_err(crate::committed_generation_recovery_error)?
            .commit(|_| true)?;
        Ok(SourceBackedRefreshPublication {
            generation_id: commit.generation_id,
            published_explicit_source_catalog: None,
            unsupported_routes: 0,
            certified_source_count: 0,
            certified_source_bytes: 0,
            current: SourceBackedRefreshCurrent::default(),
            timings: SourceBackedRefreshTimings::default(),
            route_results: Vec::new(),
            zero_source_authority: Vec::new(),
            catalog_route_bindings: Vec::new(),
            verified_index: None,
        })
    });
    let terminal_failures = Arc::new(AtomicUsize::new(0));
    let failures = Arc::clone(&terminal_failures);
    let writer = Arc::new(move |path: &Path, job: &Value| {
        if job.get("request_state").and_then(Value::as_str) == Some("published")
            && failures.fetch_add(1, Ordering::SeqCst) == 1
        {
            anyhow::bail!("injected scheduler route-finalization persistence failure");
        }
        write_daemon_job_status(path, job)
    });
    let coordinator = CoreRefreshEngine::with_status_writer_for_test(executor, writer);
    let periodic = coordinator.enqueue_periodic(&data_root).unwrap();
    coordinator
        .complete_pending_admission_for_test(
            &data_root,
            periodic["request_id"].as_str().unwrap(),
            BTreeMap::new(),
        )
        .unwrap();
    let mut runtime = DaemonRuntime::default();
    let notification = FailingGenerationPublished::default();

    let first = run_pending_core_refresh(
        &data_root,
        &mut runtime,
        Some(&coordinator),
        true,
        &notification,
        &crate::test_support::OBSERVATION,
    )
    .unwrap()
    .expect("first scheduler refresh");
    assert!(first.failed);
    assert!(first.provider_refresh_events.is_empty());
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert_eq!(notification.calls.load(Ordering::SeqCst), 0);
    let pending = read_daemon_job_status(&daemon_core_refresh_job_path(&data_root)).unwrap();
    assert_eq!(pending["request_state"], "published");
    assert_eq!(pending["route_finalization_pending"], true);

    let mut retry = run_pending_core_refresh(
        &data_root,
        &mut runtime,
        Some(&coordinator),
        true,
        &notification,
        &crate::test_support::OBSERVATION,
    )
    .unwrap()
    .expect("terminal persistence retry");
    assert!(!retry.failed);
    let events = crate::daemon::daemon_iteration_events_without_telemetry(
        &mut retry,
        std::time::Duration::from_millis(1),
    );
    assert!(matches!(
        events.as_slice(),
        [PublicEventV1::ProviderRefreshCompleted(_)]
    ));
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert_eq!(terminal_failures.load(Ordering::SeqCst), 3);
    assert_eq!(notification.calls.load(Ordering::SeqCst), 1);
    let terminal = read_daemon_job_status(&daemon_core_refresh_job_path(&data_root)).unwrap();
    assert_eq!(terminal["request_state"], "published");
    assert!(terminal.get("route_finalization_pending").is_none());
    assert!(terminal.get("last_error").is_none());
    assert!(terminal.get("failure_type").is_none());
}

#[test]
fn scheduler_failed_terminal_retry_preserves_successor_across_restart() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
    let executions = Arc::new(AtomicUsize::new(0));
    let execution_count = Arc::clone(&executions);
    let executor = Arc::new(move |_execution: SourceBackedRefreshExecution<'_>| {
        execution_count.fetch_add(1, Ordering::SeqCst);
        Err(anyhow::anyhow!("injected provider failure"))
    });
    let terminal_failures = Arc::new(AtomicUsize::new(0));
    let failures = Arc::clone(&terminal_failures);
    let writer = Arc::new(move |path: &Path, job: &Value| {
        if job.get("request_state").and_then(Value::as_str) == Some("failed")
            && failures.fetch_add(1, Ordering::SeqCst) == 0
        {
            anyhow::bail!("injected failed-terminal persistence failure");
        }
        write_daemon_job_status(path, job)
    });
    let coordinator = CoreRefreshEngine::with_status_writer_for_test(executor, writer);
    let root = coordinator.enqueue_periodic(&data_root).unwrap();
    let root_id = root["request_id"].as_str().unwrap().to_owned();
    coordinator
        .complete_pending_admission_for_test(&data_root, &root_id, BTreeMap::new())
        .unwrap();
    let mut runtime = DaemonRuntime::default();
    let first = run_pending_core_refresh(
        &data_root,
        &mut runtime,
        Some(&coordinator),
        true,
        &crate::test_support::GENERATION_PUBLISHED,
        &crate::test_support::OBSERVATION,
    )
    .unwrap()
    .expect("failed terminal persistence");
    assert!(first.failed);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    let successor = enqueue_synthetic_refresh_successor(&coordinator, &data_root, 0);
    let successor_id = successor["request_id"].as_str().unwrap().to_owned();
    let retry = coordinator
        .run_next(&data_root)
        .expect("terminal retry snapshot");
    assert!(retry.failed);
    let later = enqueue_synthetic_refresh_successor(&coordinator, &data_root, 1);
    let later_id = later["request_id"].as_str().unwrap().to_owned();
    record_source_refresh_retry(
        &data_root,
        &mut runtime.history_retry,
        &coordinator,
        retry.job,
        false,
    )
    .unwrap();
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    let exact_failed_root = coordinator
        .handle_ipc_request(
            &data_root,
            &json!({
                "op": "source_refresh_status",
                "request_id": root_id,
            }),
        )
        .unwrap()
        .expect("exact failed root status");
    drop(coordinator);

    let restarted = CoreRefreshEngine::new();
    assert!(restarted
        .recover_interrupted_publication(&data_root)
        .unwrap());
    let successor_status = restarted
        .handle_ipc_request(
            &data_root,
            &json!({
                "op": "source_refresh_status",
                "request_id": successor_id,
            }),
        )
        .unwrap()
        .expect("recovered successor status");
    assert_eq!(successor_status["request_state"], "admission_pending");
    let later_status = restarted
        .handle_ipc_request(
            &data_root,
            &json!({
                "op": "source_refresh_status",
                "request_id": later_id,
            }),
        )
        .unwrap()
        .expect("later recovered successor status");
    assert_eq!(later_status["request_state"], "admission_pending");
    let recovered_root = restarted
        .handle_ipc_request(
            &data_root,
            &json!({
                "op": "source_refresh_status",
                "request_id": root_id,
            }),
        )
        .unwrap()
        .expect("recovered failed root status");
    assert_eq!(recovered_root, exact_failed_root);
    assert!(restarted.has_pending_request());
}
