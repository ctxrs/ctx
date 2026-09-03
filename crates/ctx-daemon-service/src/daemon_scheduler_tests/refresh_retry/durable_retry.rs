use super::*;

#[test]
fn blocked_retry_writer_serializes_concurrent_admission_and_restart() {
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
    let periodic = coordinator.enqueue_periodic(&data_root).unwrap();
    coordinator
        .complete_pending_admission_for_test(
            &data_root,
            periodic["request_id"].as_str().unwrap(),
            BTreeMap::new(),
        )
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
        "admission_pending"
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
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
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
    let periodic = coordinator.enqueue_periodic(&data_root).unwrap();
    coordinator
        .complete_pending_admission_for_test(
            &data_root,
            periodic["request_id"].as_str().unwrap(),
            BTreeMap::new(),
        )
        .unwrap();
    let mut runtime = DaemonRuntime::default();
    assert!(
        run_pending_core_refresh(
            &data_root,
            &mut runtime,
            Some(&coordinator),
            true,
            &crate::test_support::GENERATION_PUBLISHED,
            &crate::test_support::OBSERVATION,
        )
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
    record_source_refresh_retry(
        &data_root,
        &mut runtime.history_retry,
        &coordinator,
        retry.job,
        false,
    )
    .unwrap();
    let persisted = read_daemon_job_status(&daemon_core_refresh_job_path(&data_root)).unwrap();
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
        assert_eq!(status["request_state"], "admission_pending");
    }
}

#[test]
fn stale_global_backoff_cannot_overwrite_durable_same_id_admission() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
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
        "refresh_intent": {"kind": "automatic_maintenance"},
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
    assert_eq!(durable["request_state"], "admission_pending");
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
    assert_eq!(recovered["request_state"], "admission_pending");
    assert_eq!(recovered, admitted);
}
