use std::{
    fs,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use super::*;
use crate::analytics::{ProviderRefreshCompletedV1, Surface};

fn manual_run() -> DaemonRunFactsV1 {
    DaemonRunFactsV1::new(DaemonStartModeV1::Manual, DaemonSupervisorV1::User, None)
}

fn runtime_names(events: &[PublicEventV1]) -> Vec<&'static str> {
    events
        .iter()
        .filter_map(|event| match event {
            PublicEventV1::RuntimeObservation(event) => Some(event.kind.name()),
            _ => None,
        })
        .collect()
}

#[test]
fn liveness_is_jittered_daily_and_never_a_loop_heartbeat() {
    assert_eq!(daemon_liveness_interval(0), DAEMON_LIVENESS_MIN_INTERVAL);
    assert!(
        daemon_liveness_interval(u64::MAX)
            < DAEMON_LIVENESS_MIN_INTERVAL + DAEMON_LIVENESS_JITTER_WINDOW
    );
    let started = Instant::now();
    let mut telemetry = DaemonTelemetry::new(manual_run(), started, 0);
    assert!(telemetry
        .liveness_events(started + StdDuration::from_secs(5))
        .is_empty());
    let due = started + DAEMON_LIVENESS_MIN_INTERVAL;
    assert_eq!(runtime_names(&telemetry.liveness_events(due)), ["liveness"]);
    assert!(telemetry.liveness_events(due).is_empty());
}

#[test]
fn idle_cycles_emit_first_then_flush_a_coalesced_transition() {
    let started = Instant::now();
    let mut telemetry = DaemonTelemetry::new(manual_run(), started, 0);
    let mut first = DaemonIteration::new(false, false, DaemonCycleStateV1::unknown());
    assert_eq!(
        runtime_names(&telemetry.observe_cycle(&mut first, StdDuration::from_millis(10))),
        ["cycle"]
    );
    for _ in 0..6 {
        let mut idle = DaemonIteration::new(false, false, DaemonCycleStateV1::unknown());
        assert!(telemetry
            .observe_cycle(&mut idle, StdDuration::from_millis(10))
            .is_empty());
    }
    assert_eq!(
        runtime_names(&telemetry.stopped_events(false, started + StdDuration::from_secs(1))),
        ["cycle", "stopped"]
    );
}

#[test]
fn run_once_has_no_runtime_event_but_preserves_provider_handoff() {
    let provider = PublicEventV1::ProviderRefreshCompleted(ProviderRefreshCompletedV1::new(
        Surface::Daemon,
        Outcome::Success,
        StdDuration::from_secs(1),
    ));
    let mut iteration = DaemonIteration::new(true, false, DaemonCycleStateV1::unknown())
        .with_provider_refresh_events(vec![provider]);
    let events = daemon_iteration_events(None, &mut iteration, StdDuration::from_secs(1));
    assert_eq!(events.len(), 1);
    assert!(matches!(
        events[0],
        PublicEventV1::ProviderRefreshCompleted(_)
    ));
    assert!(iteration.provider_refresh_events.is_empty());
}

#[test]
fn only_enabled_long_lived_daemon_uses_upgrade_scheduler() {
    assert!(daemon_should_schedule_auto_upgrade(
        true,
        DaemonMode::Full,
        false
    ));
    assert!(!daemon_should_schedule_auto_upgrade(
        false,
        DaemonMode::Full,
        false
    ));
    assert!(!daemon_should_schedule_auto_upgrade(
        true,
        DaemonMode::Full,
        true
    ));
    assert!(!daemon_should_schedule_auto_upgrade(
        true,
        DaemonMode::SourceRefreshOnly,
        false
    ));
}

fn test_daemon_run_args() -> DaemonRunArgs {
    DaemonRunArgs {
        foreground: false,
        once: true,
        idle_exit_seconds: None,
        loop_interval_seconds: None,
        max_chunks: None,
        max_seconds: None,
        force: false,
        start_mode: None,
        trigger_command: None,
        format: crate::output::JsonOutputFormat::Json,
    }
}

#[test]
fn source_refresh_only_scheduler_runs_no_unrelated_job() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let _hooks = install_daemon_test_job_hooks(DaemonTestJobHooks {
        calls: calls.clone(),
        history_refresh: Some(json!({"status": "completed"})),
        semantic_index: Some(json!({"status": "completed"})),
    });
    let mut runtime = DaemonRuntime {
        config: AppConfig {
            daemon: crate::config::DaemonConfig {
                enabled: true,
                mode: DaemonMode::SourceRefreshOnly,
            },
            search: crate::config::SearchConfig {
                semantic: Some(true),
            },
            ..AppConfig::default()
        },
        ..DaemonRuntime::default()
    };

    let iteration = run_daemon_once_with_activity(
        &test_daemon_run_args(),
        temp.path(),
        &mut runtime,
        None,
        true,
        None,
        None,
    )?;

    assert!(!iteration.did_work);
    assert!(!iteration.failed);
    assert!(calls.borrow().is_empty());
    assert!(!daemon_history_refresh_job_path(temp.path()).exists());
    assert!(!daemon_semantic_job_path(temp.path()).exists());
    Ok(())
}

#[test]
fn source_refresh_only_and_full_modes_share_the_same_refresh_path() -> Result<()> {
    use super::super::source_backed_refresh_coordinator::{
        SourceBackedRefreshExecution, SourceBackedRefreshPublication, SourceBackedRefreshTimings,
    };

    fn run_mode(daemon_mode: DaemonMode, calls: Arc<AtomicUsize>) -> Result<serde_json::Value> {
        let temp = tempfile::tempdir()?;
        let prepared = crate::upgrade::data_migration::prepare(temp.path(), &[])?;
        let generation_id = prepared
            .marker()
            .lexical_generation_id
            .clone()
            .expect("prepared lexical generation");
        let returned_generation = generation_id.clone();
        let coordinator = SourceBackedRefreshCoordinator::with_executor(Arc::new(
            move |execution: SourceBackedRefreshExecution<'_>| {
                calls.fetch_add(1, Ordering::SeqCst);
                execution.report_progress("refreshing", 0, 1, Some("all-providers".to_owned()))?;
                execution.report_progress("verifying", 1, 1, None)?;
                Ok(SourceBackedRefreshPublication {
                    generation_id: returned_generation.clone(),
                    scanned_routes: 1,
                    unsupported_routes: 0,
                    certified_source_count: 3,
                    certified_source_bytes: 4096,
                    timings: SourceBackedRefreshTimings {
                        discovery_us: 7,
                        scan_stage_us: 11,
                        commit_us: 13,
                    },
                })
            },
        ));
        coordinator.enqueue_for_test(Some(generation_id));
        let mut config = AppConfig::default();
        config.daemon.mode = daemon_mode;
        let mut runtime = DaemonRuntime {
            config,
            ..DaemonRuntime::default()
        };

        let iteration = run_daemon_once_with_activity(
            &test_daemon_run_args(),
            temp.path(),
            &mut runtime,
            None,
            false,
            None,
            Some(&coordinator),
        )?;
        assert!(!iteration.failed);
        read_daemon_job_status(&daemon_source_backed_refresh_job_path(temp.path()))
            .ok_or_else(|| anyhow!("source refresh job was not persisted"))
    }

    let source_only_calls = Arc::new(AtomicUsize::new(0));
    let full_calls = Arc::new(AtomicUsize::new(0));
    let source_only = run_mode(DaemonMode::SourceRefreshOnly, source_only_calls.clone())?;
    let full = run_mode(DaemonMode::Full, full_calls.clone())?;

    assert_eq!(source_only_calls.load(Ordering::SeqCst), 1);
    assert_eq!(full_calls.load(Ordering::SeqCst), 1);
    for key in [
        "status",
        "request_state",
        "owner",
        "kind",
        "source_count",
        "scanned_routes",
        "unsupported_routes",
        "certified_source_count",
        "certified_source_bytes",
        "timings_us",
    ] {
        assert_eq!(source_only[key], full[key], "{key}");
    }
    Ok(())
}

#[test]
fn source_refresh_only_status_exposes_runtime_and_certified_refresh_identity() -> Result<()> {
    let _env_lock = crate::config::TEST_LOCAL_USAGE_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::tempdir()?;
    fs::write(
        temp.path().join(CONFIG_FILE),
        "[daemon]\nmode = \"source-refresh-only\"\n",
    )?;
    let lock = DaemonLock::acquire(temp.path())?.expect("daemon lock");
    let now = utc_now().timestamp_millis();
    write_daemon_status(
        temp.path(),
        &json!({
            "schema_version": 1,
            "status": "running",
            "pid": process::id(),
            "started_at_ms": now,
            "heartbeat_at_ms": now,
            "start_mode": "auto",
            "trigger_command": "search",
            "semantic_runtime_active": false,
            "config_reload": {
                "status": "applied",
                "requested": {
                    "daemon_enabled": true,
                    "daemon_mode": "source-refresh-only",
                    "semantic_enabled": false,
                },
                "applied": {
                    "daemon_enabled": true,
                    "daemon_mode": "source-refresh-only",
                    "semantic_enabled": false,
                },
            },
        }),
    )?;
    super::super::paths_status::write_private_json_file(
        &super::super::paths_status::daemon_root_path(temp.path())
            .join("source-refresh-endpoint.json"),
        &json!({
            "schema_version": 1,
            "transport": "unix",
            "path": temp.path().join("daemon/source-refresh.sock"),
            "token": "must-not-appear-in-status",
            "pid": process::id(),
        }),
    )?;
    super::super::paths_status::write_daemon_job_status(
        &daemon_source_backed_refresh_job_path(temp.path()),
        &json!({
            "status": "completed",
            "daemon_mode": "source-refresh-only",
            "trigger": "search",
            "trigger_provenance": "autostart",
            "certified_source_count": 4,
            "certified_source_bytes": 8192,
            "timings_us": {
                "discovery": 5,
                "scan_stage": 7,
                "commit": 11,
            },
        }),
    )?;

    let report = daemon_report(temp.path(), &semantic_worker_report_for_daemon(temp.path()));

    assert_eq!(report["mode"], "source-refresh-only");
    assert_eq!(report["live_pid"], process::id());
    assert_eq!(report["trigger_command"], "search");
    assert_eq!(report["trigger_provenance"], "autostart");
    assert_eq!(report["lock_identity"]["active"], true);
    assert!(report["lock_identity"]["owner_id"]
        .as_str()
        .is_some_and(|owner| !owner.is_empty()));
    assert_eq!(report["source_refresh_endpoint"]["available"], true);
    assert_eq!(
        report["source_refresh_endpoint"]["owner_pid"],
        process::id()
    );
    assert!(!report.to_string().contains("must-not-appear-in-status"));
    assert_eq!(
        report["jobs"]["history_refresh"]["reason"],
        "daemon_mode_source_refresh_only"
    );
    assert_eq!(
        report["jobs"]["semantic_index"]["reason"],
        "daemon_mode_source_refresh_only"
    );
    assert_eq!(
        report["jobs"]["source_backed_refresh"]["certified_source_count"],
        4
    );
    assert_eq!(
        report["jobs"]["source_backed_refresh"]["certified_source_bytes"],
        8192
    );
    for stage in ["discovery", "scan_stage", "commit"] {
        assert!(
            report["jobs"]["source_backed_refresh"]["timings_us"][stage]
                .as_u64()
                .is_some_and(|duration| duration > 0),
            "{stage}"
        );
    }
    drop(lock);
    Ok(())
}

#[test]
fn post_lock_initialization_failure_retains_restart_intent() -> Result<()> {
    let root = tempfile::tempdir()?;
    super::super::daemon_autostart::write_daemon_restart_request(
        root.path(),
        DaemonTriggerCommandArg::Search,
        "ua_01890f3e-2c80-7000-8000-00000000000b",
    )?;
    let failure_marker = root.path().join(".fail-daemon-before-ready-for-test");
    fs::write(&failure_marker, b"fail")?;

    let error = run_daemon_inner(
        DaemonRunArgs {
            foreground: false,
            once: true,
            idle_exit_seconds: None,
            loop_interval_seconds: None,
            max_chunks: None,
            max_seconds: None,
            force: false,
            start_mode: Some(DaemonStartModeArg::Auto),
            trigger_command: Some(DaemonTriggerCommandArg::Search),
            format: crate::output::JsonOutputFormat::Json,
        },
        root.path(),
        &AppConfig::default(),
    )
    .expect_err("the injected post-lock initialization failure must surface");

    assert!(error
        .to_string()
        .contains("injected daemon failure before readiness"));
    assert!(super::super::daemon_autostart::read_daemon_restart_request(root.path()).is_some());
    Ok(())
}

#[test]
fn telemetry_policy_is_reloaded_and_failures_fail_closed() {
    let root = tempfile::tempdir().unwrap();
    let config_path = root.path().join(CONFIG_FILE);

    fs::write(&config_path, "analytics.enabled = true\n").unwrap();
    assert!(reload_daemon_analytics_config(root.path()).is_some());

    fs::write(&config_path, "analytics.enabled = false\n").unwrap();
    assert!(reload_daemon_analytics_config(root.path()).is_none());

    fs::write(&config_path, "not valid config\n").unwrap();
    assert!(reload_daemon_analytics_config(root.path()).is_none());

    let event = runtime_event(
        DaemonRuntimeObservationV1::ready(manual_run()),
        Outcome::Success,
        StdDuration::ZERO,
    );
    send_daemon_events(root.path(), &[event]);
    assert!(!crate::identity::install_path(root.path()).exists());
}
