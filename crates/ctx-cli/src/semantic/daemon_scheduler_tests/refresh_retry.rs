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
    )
    .unwrap();
    assert_eq!(recorded["queued_successors"].as_array().unwrap().len(), 2);
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
        record_source_refresh_retry(&data_root, &mut backoff, &coordinator, retry.job).unwrap();
    assert_eq!(recorded["queued_successors"].as_array().unwrap().len(), 2);
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
fn generic_backoff_write_serializes_concurrent_admission_and_restart() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_core::platform_security::establish_private_data_root(&data_root).unwrap();
    let status_entered = Arc::new(Barrier::new(2));
    let status_release = Arc::new(Barrier::new(2));
    let coordinator = Arc::new(CoreRefreshEngine::with_executor(Arc::new(
        |_execution: SourceBackedRefreshExecution<'_>| {
            panic!("backoff race must not execute refresh work")
        },
    )));
    let mut runtime = DaemonRuntime::default();
    runtime.history_retry.record_failure();

    let request_id = std::thread::scope(|scope| {
        let scheduler_coordinator = Arc::clone(&coordinator);
        let scheduler_root = data_root.clone();
        let entered = Arc::clone(&status_entered);
        let release = Arc::clone(&status_release);
        let scheduler = scope.spawn(move || {
            install_before_core_scheduler_status_hook_for_test(move || {
                entered.wait();
                release.wait();
            });
            run_daemon_scheduler_cycle_with_activity(
                &daemon_args(),
                &scheduler_root,
                &mut runtime,
                None,
                false,
                None,
                Some(&scheduler_coordinator),
            )
            .unwrap()
        });
        status_entered.wait();
        let response = enqueue_synthetic_refresh_successor(&coordinator, &data_root, 0);
        let request_id = response["request_id"].as_str().unwrap().to_owned();
        let admitted = read_daemon_job_status(&daemon_core_refresh_job_path(&data_root)).unwrap();
        assert_eq!(admitted["request_id"], request_id);
        assert_eq!(admitted["request_state"], "queued");
        status_release.wait();
        let iteration = scheduler.join().unwrap();
        assert!(!iteration.did_work);
        request_id
    });
    let durable = read_daemon_job_status(&daemon_core_refresh_job_path(&data_root)).unwrap();
    assert_eq!(durable["request_id"], request_id);
    assert_eq!(durable["request_state"], "queued");
    assert_eq!(durable["consecutive_failures"], 1);
    assert!(durable["retry_after_ms"].as_u64().is_some());
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
    let durable = read_daemon_job_status(&daemon_core_refresh_job_path(&data_root)).unwrap();
    assert_eq!(durable["request_id"], request_id);
    assert_eq!(durable["request_state"], "queued");
}
