use super::*;
use std::time::{Duration, Instant};

#[test]
fn all_scope_admission_race_recovers_without_idle_spin() {
    for mode in [DaemonMode::Full, DaemonMode::SourceRefreshOnly] {
        let temp = tempfile::tempdir().unwrap();
        let data_root = temp.path().join("data");
        ctx_history_platform::platform_security::establish_private_data_root(&data_root).unwrap();
        publish_empty_core_generation(&data_root);
        let ready_route = route_identity(0xa7);
        let delayed_route = route_identity(0xa8);
        let observations = BTreeMap::from([
            (ready_route.clone(), Some("a7".repeat(32))),
            (delayed_route.clone(), Some("a8".repeat(32))),
        ]);
        let scopes = Arc::new(Mutex::new(Vec::new()));
        let executed_scopes = Arc::clone(&scopes);
        let coordinator = CoreRefreshEngine::with_runtime_for_test(
            Arc::new(move |execution: SourceBackedRefreshExecution<'_>| {
                executed_scopes
                    .lock()
                    .unwrap()
                    .push(execution.admitted_refresh().publication_scope().clone());
                Ok(publish_empty_authoritative_generation(&execution))
            }),
            Arc::new(move |_, _| Ok(observations.clone())),
            Arc::new(write_daemon_job_status),
        );
        coordinator.initialize_watch_route_authority(BTreeSet::from([
            ready_route.clone(),
            delayed_route.clone(),
        ]));
        let route_now = super::super::super::source_route_ledger_now_ms();
        assert!(coordinator
            .enqueue_provider_root_refresh(&data_root, route_now)
            .unwrap());
        assert!(coordinator
            .prepare_next_pending_admission(&data_root)
            .unwrap());
        // A watcher event lands after the All-scope eligibility check but before
        // execution. Keep its debounce in the future so the race is deterministic.
        coordinator.record_watch_routes(
            [(delayed_route.clone(), EventWatermark::new(1, 1))],
            route_now + 60_000,
        );
        let mut runtime = DaemonRuntime::default();
        runtime.config.daemon.mode = mode;
        let failed = run_source_refresh_cycle(&data_root, &mut runtime, &coordinator);
        assert!(failed.failed);
        assert!(!failed.did_work);
        let terminal = read_daemon_job_status(&daemon_core_refresh_job_path(&data_root)).unwrap();
        assert_eq!(terminal["refresh_scope"]["kind"], "all");
        assert_eq!(
            terminal["last_error"],
            "one or more exact source routes are no longer due for admission"
        );
        assert_eq!(terminal["structured_outcome"]["class"], "internal");
        assert_eq!(terminal["structured_outcome"]["affected_routes"], json!([]));
        assert_eq!(
            terminal["structured_outcome"]["retry_advice"],
            "retry_request"
        );
        assert!(
            scopes.lock().unwrap().is_empty(),
            "failure precedes capture"
        );

        assert_eq!(coordinator.pending_scheduler_retry_root_for_test(), None);
        assert!(!coordinator.has_pending_request());
        let now = Instant::now();
        let wait = crate::daemon::daemon_wait_duration(
            &runtime,
            Some(&coordinator),
            now + Duration::from_secs(30),
            now,
            failed.did_work,
        );
        assert_eq!(wait, Duration::from_secs(10));
        let ready = run_source_refresh_cycle(&data_root, &mut runtime, &coordinator);
        assert!(ready.did_work, "the due route must recover without restart");
        assert!(!ready.failed);
        assert_eq!(runtime.history_retry.consecutive_failures, 0);
        assert_eq!(
            scopes.lock().unwrap().as_slice(),
            &[SourceBackedRefreshScope::Exact(BTreeSet::from([
                ready_route
            ]))]
        );
        assert!(!coordinator.has_pending_request());
        assert_eq!(
            read_daemon_job_status(&daemon_core_refresh_job_path(&data_root)).unwrap()["status"],
            "completed"
        );
        assert_eq!(
            coordinator
                .status_for_test(terminal["request_id"].as_str().unwrap())
                .unwrap()["request_state"],
            "failed"
        );
        let idle = run_source_refresh_cycle(&data_root, &mut runtime, &coordinator);
        assert!(!idle.did_work);
        assert!(!idle.continue_immediately);
        let now = Instant::now();
        assert_eq!(
            crate::daemon::daemon_wait_duration(
                &runtime,
                Some(&coordinator),
                now + Duration::from_secs(30),
                now,
                idle.did_work,
            ),
            Duration::from_secs(30),
        );

        // A later genuine event makes the remaining route admissible. It must
        // publish normally; the earlier failed logical request stays terminal.
        coordinator.record_watch_routes(
            [(delayed_route.clone(), EventWatermark::new(1, 2))],
            super::super::super::source_route_ledger_now_ms().saturating_sub(1_000),
        );
        let recovered = run_source_refresh_cycle(&data_root, &mut runtime, &coordinator);
        assert!(recovered.did_work);
        assert!(!recovered.failed);
        assert_eq!(
            scopes.lock().unwrap().last(),
            Some(&SourceBackedRefreshScope::Exact(BTreeSet::from([
                delayed_route
            ])))
        );
        assert!(coordinator.next_dirty_route_due_in_ms(u64::MAX).is_none());
    }
}

#[test]
fn no_work_timeout_wakeups_are_bounded_while_foreground_queries_block_admission() {
    let temp = tempfile::tempdir().unwrap();
    let data_root = temp.path().join("data");
    let route = route_identity(0xb7);
    let coordinator =
        CoreRefreshEngine::with_executor(Arc::new(|_: SourceBackedRefreshExecution<'_>| {
            panic!("foreground query blocks capture")
        }));
    coordinator.reconcile_watch_routes([route], EventWatermark::new(1, 0), 0);
    let activity = Arc::new(crate::query_service::DaemonQueryActivity::new());
    let query = activity.begin_request().unwrap();
    let mut runtime = DaemonRuntime::default();
    let started = Instant::now();
    let safety_deadline = started + Duration::from_secs(30);
    let mut now = started;
    let mut no_work_cycles = 0;
    let mut zero_timeouts = 0;
    // Advance a deterministic clock by the real scheduler's selected wait.
    // A zero-timeout loop hits the iteration cap without advancing any time.
    while now < safety_deadline && no_work_cycles < 32 {
        let idle = run_daemon_scheduler_cycle_with_activity(
            &daemon_args(),
            &data_root,
            &mut runtime,
            None,
            false,
            Some(&activity),
            Some(&coordinator),
        )
        .unwrap();
        assert!(!idle.did_work);
        assert!(!idle.continue_immediately);
        no_work_cycles += 1;
        let wait = crate::daemon::daemon_wait_duration(
            &runtime,
            Some(&coordinator),
            safety_deadline,
            now,
            idle.did_work,
        );
        zero_timeouts += usize::from(wait.is_zero());
        now += wait;
    }
    eprintln!(
        "30s no-admission window: no_work_cycles={no_work_cycles}, zero_timeouts={zero_timeouts}"
    );
    assert_eq!(no_work_cycles, 3);
    assert_eq!(zero_timeouts, 0);
    assert_eq!(now, safety_deadline);
    drop(query);

    // Real signals interrupt the bounded wait instead of waiting for its timer.
    let wakeup = crate::daemon_wakeup::DaemonWakeup::default();
    wakeup.signal_ipc();
    let wake = wakeup.wait(Duration::from_secs(10));
    assert!(!wake.timed_out);
    assert_eq!(
        crate::daemon::daemon_wait_duration(
            &runtime,
            Some(&coordinator),
            safety_deadline,
            safety_deadline,
            false,
        ),
        Duration::ZERO,
        "the safety deadline must still be serviced"
    );
}
