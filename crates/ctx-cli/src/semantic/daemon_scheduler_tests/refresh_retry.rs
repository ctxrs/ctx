use super::*;

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
            crate::commands::import::explicit_source_catalog_authority_for_test(revision),
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
) -> crate::semantic::daemon::DaemonIteration {
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
        ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
        publish_empty_core_generation(&data_root);
        let route = route_identity(byte);
        let scopes = Arc::new(Mutex::new(Vec::new()));
        let observed_scopes = Arc::clone(&scopes);
        let coordinator = CoreRefreshEngine::with_executor(Arc::new(
            move |execution: SourceBackedRefreshExecution<'_>| {
                observed_scopes
                    .lock()
                    .unwrap()
                    .push(execution.scope.clone());
                Err(SourceBackedRouteError::new(kind, "injected route failure").into())
            },
        ));
        coordinator.initialize_watch_route_authority(BTreeSet::from([route.clone()]));
        coordinator.record_watch_routes(
            [(route.clone(), EventWatermark::new(byte.into(), 1))],
            super::super::source_route_ledger_now_ms().saturating_sub(1_000),
        );
        let mut runtime = source_refresh_only_runtime();
        assert!(run_source_refresh_cycle(&data_root, &mut runtime, &coordinator).failed);
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
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    publish_empty_core_generation(&data_root);
    let retryable_route = route_identity(0xc1);
    let blocked_route = route_identity(0xc2);
    let executor_retryable = retryable_route.clone();
    let executor_blocked = blocked_route.clone();
    let coordinator = CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            let mut publication = publish_empty_authoritative_generation(execution.index_root);
            publication.route_results = [
                (&executor_retryable, "unavailable"),
                (&executor_blocked, "incompatible"),
            ]
            .into_iter()
            .map(|(route, class)| {
                let mut result =
                    SourceBackedRefreshRouteResult::succeeded(route.as_str().to_owned(), false);
                result.source_failure_total = 1;
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
            Ok(publication)
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

    let completed = run_source_refresh_cycle(&data_root, &mut runtime, &coordinator);

    assert!(!completed.failed);
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
fn failed_attached_demand_is_terminal_replayable_and_never_executes_a_successor() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let executor_entered = Arc::clone(&entered);
    let executor_release = Arc::clone(&release);
    let executions = Arc::new(AtomicUsize::new(0));
    let observed_executions = Arc::clone(&executions);
    let coordinator = Arc::new(CoreRefreshEngine::with_executor(Arc::new(
        move |_execution: SourceBackedRefreshExecution<'_>| {
            observed_executions.fetch_add(1, Ordering::SeqCst);
            executor_entered.wait();
            executor_release.wait();
            Err(SourceBackedRouteError::new(
                SourceBackedRouteErrorKind::SourceChanged,
                "injected predecessor failure",
            )
            .into())
        },
    )));
    coordinator.enqueue_for_test(None);
    let logical_request_id = uuid::Uuid::from_u128(0x294_0001).to_string();
    let authority = crate::commands::import::explicit_source_catalog_authority_for_test(7);

    let (iteration, attached) = std::thread::scope(|scope| {
        let scheduler_coordinator = Arc::clone(&coordinator);
        let scheduler_root = data_root.clone();
        let scheduler = scope.spawn(move || {
            let mut runtime = source_refresh_only_runtime();
            run_source_refresh_cycle(&scheduler_root, &mut runtime, &scheduler_coordinator)
        });
        entered.wait();
        let attached = coordinator
            .enqueue_fresh_catalog_demand_for_test(
                &data_root,
                None,
                logical_request_id.clone(),
                authority.clone(),
            )
            .expect("attached logical freshness demand");
        assert_eq!(attached["logical_phase"], "attached");
        release.wait();
        (scheduler.join().unwrap(), attached)
    });

    assert!(iteration.failed);
    let physical_attempt_id = attached["physical_attempt_id"].as_str().unwrap().to_owned();
    let terminal = coordinator
        .handle_ipc_request(
            &data_root,
            &json!({
                "op": "source_refresh_status",
                "request_id": logical_request_id,
            }),
        )
        .unwrap()
        .expect("terminal logical demand");
    assert_eq!(terminal["request_state"], "failed");
    assert_eq!(terminal["logical_phase"], "terminal");
    assert_eq!(
        terminal["structured_outcome"]["physical_attempt_id"],
        physical_attempt_id
    );
    assert!(!coordinator.has_pending_request());

    let mut idle_runtime = source_refresh_only_runtime();
    let idle = run_source_refresh_cycle(&data_root, &mut idle_runtime, &coordinator);
    assert!(!idle.did_work);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    drop(coordinator);

    let restarted = CoreRefreshEngine::new();
    let _ = restarted
        .recover_interrupted_publication(&data_root)
        .unwrap();
    assert!(!restarted.has_pending_request());
    assert!(restarted.run_next(&data_root).is_none());
    let replay = restarted
        .enqueue_fresh_catalog_demand_for_test(&data_root, None, logical_request_id, authority)
        .expect("same-ID terminal replay after restart");
    assert_eq!(replay, terminal);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
}

#[test]
fn persistent_admission_status_failure_uses_scheduler_backoff() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
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
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let executions = Arc::new(AtomicUsize::new(0));
    let observed_executions = Arc::clone(&executions);
    let executor = Arc::new(move |execution: SourceBackedRefreshExecution<'_>| {
        observed_executions.fetch_add(1, Ordering::SeqCst);
        Ok(publish_empty_authoritative_generation(execution.index_root))
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
    coordinator.enqueue_periodic(&data_root).unwrap();
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
fn scheduler_retries_terminal_status_without_republishing_core() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let executions = Arc::new(AtomicUsize::new(0));
    let execution_count = Arc::clone(&executions);
    let executor = Arc::new(move |execution: SourceBackedRefreshExecution<'_>| {
        execution_count.fetch_add(1, Ordering::SeqCst);
        let commit = GenerationWriter::open(execution.index_root, WriterOptions::default())?
            .into_writer()
            .map_err(crate::semantic::committed_generation_recovery_error)?
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
            catalog_route_bindings: Vec::new(),
            verified_index: None,
        })
    });
    let terminal_failures = Arc::new(AtomicUsize::new(0));
    let failures = Arc::clone(&terminal_failures);
    let writer = Arc::new(move |path: &Path, job: &Value| {
        if job.get("request_state").and_then(Value::as_str) == Some("published")
            && failures.fetch_add(1, Ordering::SeqCst) == 0
        {
            anyhow::bail!("injected scheduler terminal persistence failure");
        }
        write_daemon_job_status(path, job)
    });
    let coordinator = CoreRefreshEngine::with_status_writer_for_test(executor, writer);
    coordinator.enqueue_periodic(&data_root).unwrap();
    let mut runtime = DaemonRuntime::default();

    let first = run_pending_core_refresh(&data_root, &mut runtime, Some(&coordinator))
        .unwrap()
        .expect("first scheduler refresh");
    assert!(first.failed);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert_eq!(
        read_daemon_job_status(&daemon_core_refresh_job_path(&data_root)).unwrap()["request_state"],
        "running"
    );

    let retry = run_pending_core_refresh(&data_root, &mut runtime, Some(&coordinator))
        .unwrap()
        .expect("terminal persistence retry");
    assert!(!retry.failed);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    let terminal = read_daemon_job_status(&daemon_core_refresh_job_path(&data_root)).unwrap();
    assert_eq!(terminal["request_state"], "published");
    assert!(terminal.get("last_error").is_none());
    assert!(terminal.get("failure_type").is_none());
}

#[test]
fn scheduler_failed_terminal_retry_preserves_successor_across_restart() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
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
    let mut runtime = DaemonRuntime::default();
    let first = run_pending_core_refresh(&data_root, &mut runtime, Some(&coordinator))
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
    let recorded = record_source_refresh_retry(
        &data_root,
        &mut runtime.history_retry,
        &coordinator,
        retry.job,
        false,
    )
    .unwrap();
    assert_eq!(recorded["queued_successors"].as_array().unwrap().len(), 1);
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
    assert_eq!(successor_status["request_state"], "queued");
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
    assert_eq!(later_status["request_state"], "queued");
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

#[test]
fn blocked_retry_writer_serializes_concurrent_admission_and_restart() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let executions = Arc::new(AtomicUsize::new(0));
    let execution_count = Arc::clone(&executions);
    let executor = Arc::new(move |_execution: SourceBackedRefreshExecution<'_>| {
        execution_count.fetch_add(1, Ordering::SeqCst);
        Err(anyhow::anyhow!("injected provider failure"))
    });
    let terminal_failures = Arc::new(AtomicUsize::new(0));
    let block_retry = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let retry_entered = Arc::new(Barrier::new(2));
    let retry_release = Arc::new(Barrier::new(2));
    let failures = Arc::clone(&terminal_failures);
    let block = Arc::clone(&block_retry);
    let entered = Arc::clone(&retry_entered);
    let release = Arc::clone(&retry_release);
    let writer = Arc::new(move |path: &Path, job: &Value| {
        if job.get("request_state").and_then(Value::as_str) == Some("failed")
            && job.get("retry_after_ms").is_none()
        {
            if failures.fetch_add(1, Ordering::SeqCst) == 0 {
                anyhow::bail!("injected failed-terminal persistence failure");
            }
            if block.swap(false, Ordering::SeqCst) {
                entered.wait();
                release.wait();
            }
        }
        write_daemon_job_status(path, job)
    });
    let coordinator = Arc::new(CoreRefreshEngine::with_status_writer_for_test(
        executor, writer,
    ));
    coordinator.enqueue_periodic(&data_root).unwrap();
    let mut runtime = DaemonRuntime::default();
    let first = run_pending_core_refresh(&data_root, &mut runtime, Some(&coordinator))
        .unwrap()
        .expect("failed terminal persistence");
    assert!(first.failed);
    let successor = enqueue_synthetic_refresh_successor(&coordinator, &data_root, 0);
    let successor_id = successor["request_id"].as_str().unwrap().to_owned();
    block_retry.store(true, Ordering::SeqCst);

    let (retry, later_id) = std::thread::scope(|scope| {
        let retry_coordinator = Arc::clone(&coordinator);
        let retry_root = data_root.clone();
        let retry_writer = scope.spawn(move || {
            retry_coordinator
                .run_next(&retry_root)
                .expect("terminal retry snapshot")
        });
        retry_entered.wait();

        let (admission_started_tx, admission_started_rx) = std::sync::mpsc::sync_channel(0);
        let (admission_done_tx, admission_done_rx) = std::sync::mpsc::channel();
        let admission_coordinator = Arc::clone(&coordinator);
        let admission_root = data_root.clone();
        let admission = scope.spawn(move || {
            admission_started_tx.send(()).unwrap();
            let response =
                enqueue_synthetic_refresh_successor(&admission_coordinator, &admission_root, 1);
            admission_done_tx.send(()).unwrap();
            response["request_id"].as_str().unwrap().to_owned()
        });
        admission_started_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("concurrent admission started");
        assert!(
            admission_done_rx
                .recv_timeout(std::time::Duration::from_millis(100))
                .is_err(),
            "admission acknowledgement must wait behind durable retry publication"
        );
        retry_release.wait();
        let retry = retry_writer.join().unwrap();
        assert!(retry.failed);
        (retry, admission.join().unwrap())
    });
    let mut backoff = DaemonRetryBackoff::default();
    let recorded =
        record_source_refresh_retry(&data_root, &mut backoff, &coordinator, retry.job, false)
            .unwrap();
    assert_eq!(recorded["queued_successors"].as_array().unwrap().len(), 1);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    let before_restart = read_daemon_job_status(&daemon_core_refresh_job_path(&data_root)).unwrap();
    assert_eq!(before_restart["request_id"], recorded["request_id"]);
    assert_eq!(
        before_restart["queued_successors"][0]["request_id"],
        successor_id
    );
    assert_eq!(
        before_restart["queued_successors"][1]["request_id"],
        later_id
    );
    drop(coordinator);

    let restarted = CoreRefreshEngine::new();
    assert!(restarted
        .recover_interrupted_publication(&data_root)
        .unwrap());
    assert_eq!(
        restarted
            .handle_ipc_request(
                &data_root,
                &json!({
                    "op": "source_refresh_status",
                    "request_id": later_id,
                }),
            )
            .unwrap()
            .expect("concurrent successor survives restart")["request_state"],
        "queued"
    );
    let durable = read_daemon_job_status(&daemon_core_refresh_job_path(&data_root)).unwrap();
    assert_eq!(durable["request_state"], "failed");
    assert_eq!(durable["queued_successors"].as_array().unwrap().len(), 2);
    assert_eq!(durable["queued_successors"][0]["request_id"], successor_id);
    assert_eq!(durable["queued_successors"][1]["request_id"], later_id);
}

#[test]
fn failed_terminal_root_keeps_capacity_through_successful_retry_and_restart() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let executor = Arc::new(move |_execution: SourceBackedRefreshExecution<'_>| {
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
    coordinator.enqueue_periodic(&data_root).unwrap();
    let mut runtime = DaemonRuntime::default();
    assert!(
        run_pending_core_refresh(&data_root, &mut runtime, Some(&coordinator))
            .unwrap()
            .expect("failed terminal persistence")
            .failed
    );

    let mut successor_ids = Vec::new();
    let mut busy = None;
    for revision in 1..=16 {
        let response = enqueue_synthetic_refresh_successor(
            &coordinator,
            &data_root,
            u64::try_from(revision).unwrap(),
        );
        if response["ok"] == false {
            busy = Some(response);
            break;
        }
        successor_ids.push(response["request_id"].as_str().unwrap().to_owned());
    }
    let busy = busy.expect("typed queue-full response");
    assert_eq!(busy["error_code"], "source_refresh_queue_full");
    assert_eq!(
        busy["active_pending_requests"].as_u64().unwrap(),
        u64::try_from(successor_ids.len().saturating_add(1)).unwrap()
    );
    assert_eq!(
        busy["active_pending_requests"],
        busy["max_active_pending_requests"]
    );
    let retry = coordinator
        .run_next(&data_root)
        .expect("successful failed-terminal status retry");
    assert!(retry.failed);
    assert!(!retry.terminal_persistence_pending);
    let still_full = enqueue_synthetic_refresh_successor(&coordinator, &data_root, 99);
    assert_eq!(still_full["error_code"], "source_refresh_queue_full");
    let persisted = record_source_refresh_retry(
        &data_root,
        &mut runtime.history_retry,
        &coordinator,
        retry.job,
        false,
    )
    .unwrap();
    assert_eq!(
        persisted["queued_successors"].as_array().unwrap().len(),
        successor_ids.len()
    );
    drop(coordinator);

    let restarted = CoreRefreshEngine::new();
    assert!(restarted
        .recover_interrupted_publication(&data_root)
        .unwrap());
    for request_id in successor_ids {
        let status = restarted
            .handle_ipc_request(
                &data_root,
                &json!({
                    "op": "source_refresh_status",
                    "request_id": request_id,
                }),
            )
            .unwrap()
            .expect("every saturated successor recovers");
        assert_eq!(status["request_state"], "queued");
    }
}

#[test]
fn stale_global_backoff_cannot_overwrite_durable_same_id_admission() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let coordinator = CoreRefreshEngine::with_executor(Arc::new(
        |_execution: SourceBackedRefreshExecution<'_>| {
            panic!("stale global backoff must not execute refresh work")
        },
    ));
    let mut runtime = DaemonRuntime::default();
    runtime.history_retry.record_failure();
    make_history_retry_due(&mut runtime);

    let idle = run_daemon_scheduler_cycle_with_activity(
        &daemon_args(),
        &data_root,
        &mut runtime,
        None,
        false,
        None,
        Some(&coordinator),
    )
    .unwrap();
    assert!(!idle.did_work);
    assert_eq!(runtime.history_retry.consecutive_failures, 0);

    let request_id = uuid::Uuid::from_u128(0x294_0002).to_string();
    let request = json!({
        "schema_version": 1,
        "op": "source_refresh_request",
        "request_id": request_id,
        "mode": "wait",
        "operation": "refresh",
    });
    let admitted = coordinator
        .handle_ipc_request(&data_root, &request)
        .unwrap()
        .expect("durable admission");
    let replayed = coordinator
        .handle_ipc_request(&data_root, &request)
        .unwrap()
        .expect("same-ID replay");
    assert_eq!(admitted, replayed);
    let durable = read_daemon_job_status(&daemon_core_refresh_job_path(&data_root)).unwrap();
    assert_eq!(durable["request_id"], request_id);
    assert_eq!(durable["request_state"], "queued");
    assert!(durable.get("consecutive_failures").is_none());
    assert!(durable.get("retry_after_ms").is_none());
    drop(coordinator);

    let restarted = CoreRefreshEngine::new();
    assert!(restarted
        .recover_interrupted_publication(&data_root)
        .unwrap());
    assert!(restarted.has_pending_request());
    let recovered = restarted
        .handle_ipc_request(
            &data_root,
            &json!({
                "op": "source_refresh_status",
                "request_id": request_id,
            }),
        )
        .unwrap()
        .expect("exact acknowledged request survives restart");
    assert_eq!(recovered["request_id"], request_id);
    assert_eq!(recovered["request_state"], "queued");
    assert_eq!(recovered, admitted);
}
