use std::fs;

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
    assert!(daemon_should_schedule_auto_upgrade(true, false));
    assert!(!daemon_should_schedule_auto_upgrade(false, false));
    assert!(!daemon_should_schedule_auto_upgrade(true, true));
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
            json: true,
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
