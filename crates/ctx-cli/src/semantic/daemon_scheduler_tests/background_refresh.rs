use super::*;

#[test]
fn continuous_background_writer_obeys_duration_aware_rest_without_tight_loop() {
    let origin = std::time::Instant::now();
    let mut cadence = DaemonBackgroundRefreshCadence::default();

    let first_finished = origin + std::time::Duration::from_secs(30);
    cadence.record_completion(origin, first_finished);
    assert_eq!(
        cadence.remaining(first_finished),
        Some(std::time::Duration::from_secs(30))
    );
    assert!(!cadence.ready(first_finished + std::time::Duration::from_secs(29)));
    assert!(cadence.ready(first_finished + std::time::Duration::from_secs(30)));

    let second_started = first_finished + std::time::Duration::from_secs(30);
    let second_finished = second_started + std::time::Duration::from_millis(100);
    cadence.record_completion(second_started, second_finished);
    assert_eq!(
        cadence.remaining(second_finished),
        Some(DAEMON_BACKGROUND_REFRESH_MIN_REST)
    );
    assert!(!cadence.ready(second_finished));
    assert!(cadence.ready(second_finished + DAEMON_BACKGROUND_REFRESH_MIN_REST));
}

#[test]
fn background_rest_is_bounded_and_restores_from_periodic_terminal_status() {
    assert_eq!(
        background_refresh_rest(std::time::Duration::from_secs(60 * 60)),
        DAEMON_BACKGROUND_REFRESH_MAX_REST
    );
    let now = std::time::Instant::now();
    let mut cadence = DaemonBackgroundRefreshCadence::default();
    cadence.restore(
        Some(&json!({
            "operation": "refresh",
            "trigger": "periodic",
            "trigger_provenance": "daemon_scheduler",
            "started_at_ms": 1_000,
            "finished_at_ms": 31_000,
        })),
        None,
        40_000,
        now,
    );
    assert_eq!(
        cadence.remaining(now),
        Some(std::time::Duration::from_secs(21))
    );

    let mut skewed = DaemonBackgroundRefreshCadence::default();
    skewed.restore(
        Some(&json!({
            "operation": "refresh",
            "trigger": "periodic",
            "trigger_provenance": "daemon_scheduler",
            "started_at_ms": 1,
            "finished_at_ms": 10_000_000,
        })),
        None,
        0,
        now,
    );
    assert_eq!(
        skewed.remaining(now),
        Some(DAEMON_BACKGROUND_REFRESH_MAX_REST)
    );

    let mut explicit = DaemonBackgroundRefreshCadence::default();
    explicit.restore(
        Some(&json!({
            "operation": "import",
            "trigger": "import",
            "trigger_provenance": "explicit_source_catalog",
            "started_at_ms": 1_000,
            "finished_at_ms": 31_000,
        })),
        None,
        40_000,
        now,
    );
    assert!(explicit.ready(now));
}

#[test]
fn unrelated_manual_recovery_does_not_restore_background_cooldown() {
    let temp = tempfile::tempdir().unwrap();
    ctx_history_core::platform_security::establish_private_data_root(temp.path()).unwrap();
    write_daemon_job_status(
        &daemon_core_refresh_job_path(temp.path()),
        &json!({
            "request_id": "manual-recovery",
            "operation": "refresh",
            "trigger": "search",
            "trigger_provenance": "manual",
        }),
    )
    .unwrap();
    preserve_daemon_background_refresh_recovery_provenance(temp.path()).unwrap();
    write_daemon_job_status(
        &daemon_core_refresh_job_path(temp.path()),
        &json!({
            "request_id": "manual-recovery",
            "request_state": "published",
            "status": "completed",
            "trigger": "recovery",
            "trigger_provenance": "commit_payload",
            "started_at_ms": 1_000,
            "finished_at_ms": 2_000,
            "published_generation": "manual-generation",
            "receipt": {},
        }),
    )
    .unwrap();

    let mut runtime = DaemonRuntime::default();
    restore_daemon_background_refresh_cadence(&mut runtime, temp.path());
    assert!(runtime
        .background_refresh_cadence
        .ready(std::time::Instant::now()));
}

#[test]
fn explicit_freshness_bypasses_background_rest_without_second_publisher() {
    let temp = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let executor_calls = Arc::clone(&calls);
    let coordinator = CoreRefreshEngine::with_executor(Arc::new(
        move |execution: SourceBackedRefreshExecution<'_>| {
            executor_calls.fetch_add(1, Ordering::SeqCst);
            Ok(publish_empty_authoritative_generation(execution.index_root))
        },
    ));
    let response = coordinator
        .handle_ipc_request(
            temp.path(),
            &json!({
                "schema_version": 1,
                "op": "source_refresh_request",
                "mode": "wait",
                "operation": "refresh",
            }),
        )
        .unwrap()
        .expect("explicit freshness response");
    assert_eq!(response["request_state"], "queued");
    let mut runtime = DaemonRuntime::default();
    let now = std::time::Instant::now();
    runtime
        .background_refresh_cadence
        .record_completion(now, now);
    assert!(!runtime.background_refresh_cadence.ready(now));

    let iteration = run_daemon_scheduler_cycle_with_activity(
        &daemon_args(),
        temp.path(),
        &mut runtime,
        None,
        false,
        None,
        Some(&coordinator),
    )
    .unwrap();

    assert!(!iteration.failed);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(!coordinator.has_pending_request());
}
