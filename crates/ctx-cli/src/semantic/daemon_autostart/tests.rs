use super::*;
use crate::config::CONFIG_FILE;
use crate::semantic::paths_status::DaemonLock;
use ctx_history_core::database_path;
use std::sync::{Arc, Barrier};

const DAEMON_ENV_PROBE_STAGE: &str = "CTX_DAEMON_ENV_PROBE_STAGE";
const DAEMON_ENV_PROBE_TEST: &str =
    "semantic::daemon_autostart::tests::daemon_child_environment_is_narrow_and_release_sanitized";
const DAEMON_ENV_HOSTILE: &str = "CTX_UNTRUSTED_DAEMON_AMBIENT_SECRET";
const DAEMON_ENV_ALLOWED_SENTINEL: &str = "/ctx-daemon-allowed-home";

#[test]
fn daemon_child_environment_is_narrow_and_release_sanitized() -> Result<()> {
    match env::var(DAEMON_ENV_PROBE_STAGE).as_deref() {
        Ok("final") => {
            assert_eq!(env::var("HOME").as_deref(), Ok(DAEMON_ENV_ALLOWED_SENTINEL));
            assert!(env::var_os(DAEMON_ENV_HOSTILE).is_none());
            assert!(env::var_os("CTX_RELEASE_INHERITED_AUTHORITY").is_none());
            assert!(env::var_os("CTX_RELEASE_CONFIGURED_AUTHORITY").is_none());
            return Ok(());
        }
        Ok("inherited") => {
            assert_eq!(env::var(DAEMON_ENV_HOSTILE).as_deref(), Ok("attacker"));
            assert_eq!(
                env::var("CTX_RELEASE_INHERITED_AUTHORITY").as_deref(),
                Ok("attacker")
            );
            let mut descendant = Command::new(env::current_exe()?);
            configure_narrow_daemon_environment(&mut descendant);
            descendant
                .args(["--exact", DAEMON_ENV_PROBE_TEST, "--nocapture"])
                .env(DAEMON_ENV_PROBE_STAGE, "final")
                .env("CTX_RELEASE_CONFIGURED_AUTHORITY", "attacker");
            assert!(spawn_daemon_child(&mut descendant)?.wait()?.success());
            return Ok(());
        }
        _ => {}
    }

    let mut inherited = Command::new(env::current_exe()?);
    inherited
        .args(["--exact", DAEMON_ENV_PROBE_TEST, "--nocapture"])
        .env(DAEMON_ENV_PROBE_STAGE, "inherited")
        .env(DAEMON_ENV_HOSTILE, "attacker")
        .env("CTX_RELEASE_INHERITED_AUTHORITY", "attacker")
        .env("HOME", DAEMON_ENV_ALLOWED_SENTINEL);
    assert!(inherited.status()?.success());
    Ok(())
}

fn write_installation_registration(
    root: &Path,
    name: &str,
    status: &str,
    attempt_id: Option<&str>,
    pid: u32,
) -> Result<()> {
    write_private_json_file(
        &root.join(format!("{name}.json")),
        &json!({
            "schema_version": 1,
            "registration_id": name,
            "status": status,
            "attempt_id": attempt_id,
            "pid": pid,
            "data_root": root.join(name),
            "trigger_command": "search",
            "idle_exit_seconds": 60,
            "loop_interval_seconds": 30,
            "updated_at_ms": utc_now().timestamp_millis(),
        }),
    )
}

#[test]
fn installation_quiescence_waits_for_every_live_daemon_lease() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let lock_path = temp.path().join("installation-daemons.lock");
    let registrations = temp.path().join("acks");
    let attempt_id = "ua_01890f3e-2c80-7000-8000-000000000010";
    let live_lease = open_installation_daemon_quiescence_lock_at(&lock_path)?;
    fs2::FileExt::lock_shared(&live_lease)?;
    write_installation_registration(
        &registrations,
        "second-root",
        "acknowledged",
        Some(attempt_id),
        process::id(),
    )?;

    let waiter_lock = lock_path.clone();
    let waiter_registrations = registrations.clone();
    let waiter = std::thread::spawn(move || {
        wait_for_installation_daemon_quiescence_at(
            &waiter_lock,
            &waiter_registrations,
            attempt_id,
            StdDuration::from_secs(5),
        )
    });
    std::thread::sleep(StdDuration::from_millis(100));
    assert!(
        !waiter.is_finished(),
        "exclusive installation quiescence cleared while a daemon lease remained live"
    );

    fs2::FileExt::unlock(&live_lease)?;
    waiter
        .join()
        .expect("join installation quiescence waiter")?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn installation_acknowledgements_ignore_stale_attempts_and_stopped_crashes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let registrations = temp.path().join("acks");
    write_installation_registration(
        &registrations,
        "stale-attempt",
        "acknowledged",
        Some("ua_old_attempt"),
        process::id(),
    )?;
    write_installation_registration(&registrations, "crashed-daemon", "live", None, u32::MAX)?;

    assert!(
        read_installation_daemon_restarts_from(&registrations, "ua_current_attempt", true)?
            .is_empty()
    );
    Ok(())
}

#[test]
fn incomplete_or_malformed_current_quiescence_fails_closed() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let registrations = temp.path().join("acks");
    let attempt_id = "ua_01890f3e-2c80-7000-8000-000000000011";
    write_installation_registration(
        &registrations,
        "incomplete",
        "quiescing",
        Some(attempt_id),
        process::id(),
    )?;
    assert!(
        read_installation_daemon_restarts_from(&registrations, attempt_id, true).is_err(),
        "a current attempt may not mutate after a partial acknowledgement"
    );

    fs::write(registrations.join("incomplete.json"), b"{not-json")?;
    assert!(
        read_installation_daemon_restarts_from(&registrations, attempt_id, true).is_err(),
        "malformed acknowledgement state must fail closed"
    );
    Ok(())
}

#[test]
fn failed_restart_intent_write_preserves_partial_acknowledgement() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let lock_path = temp.path().join("installation-daemons.lock");
    let registration_path = temp.path().join("partial.json");
    let blocked_root = temp.path().join("not-a-directory");
    fs::write(&blocked_root, b"blocked")?;
    let lock = open_installation_daemon_quiescence_lock_at(&lock_path)?;
    fs2::FileExt::lock_shared(&lock)?;
    let lease = InstallationDaemonLease {
        lock,
        registration_path: registration_path.clone(),
        registration_id: "partial".to_owned(),
        data_root: blocked_root,
        trigger: DaemonTriggerCommandArg::Search,
        idle_exit_seconds: Some(60),
        loop_interval_seconds: Some(30),
        status: "live",
    };
    lease.write_status("live", None)?;

    assert!(lease
        .acknowledge("ua_01890f3e-2c80-7000-8000-000000000012")
        .is_err());
    let registration: Value = serde_json::from_slice(&fs::read(registration_path)?)?;
    assert_eq!(registration["status"], "quiescing");
    assert_eq!(
        registration["attempt_id"],
        "ua_01890f3e-2c80-7000-8000-000000000012"
    );
    Ok(())
}

#[test]
fn autostart_child_inherits_effective_analytics_policy() {
    let command = daemon_autostart_command(
        Path::new("ctx"),
        Path::new("/tmp/ctx-daemon-telemetry-test"),
        DaemonTriggerCommandArg::Search,
        Some(5),
        Some(5),
        None,
    );
    let env = command
        .get_envs()
        .map(|(key, value)| (key.to_owned(), value.map(ToOwned::to_owned)))
        .collect::<Vec<_>>();
    assert!(env.iter().any(|(key, value)| {
        key == DAEMON_BACKGROUND_CHILD_ENV && value.as_deref() == Some(std::ffi::OsStr::new("1"))
    }));
    assert!(env
        .iter()
        .all(|(key, _)| key != std::ffi::OsStr::new("CTX_ANALYTICS_ENABLED")));
}

#[test]
fn persistent_autostart_child_has_no_implicit_exit_or_poll_interval() {
    let command = daemon_autostart_command(
        Path::new("ctx"),
        Path::new("/tmp/ctx-daemon-persistent-test"),
        DaemonTriggerCommandArg::Search,
        None,
        None,
        None,
    );
    let args = command
        .get_args()
        .filter_map(std::ffi::OsStr::to_str)
        .collect::<Vec<_>>();
    assert!(!args.contains(&"--idle-exit-seconds"), "{args:?}");
    assert!(!args.contains(&"--loop-interval-seconds"), "{args:?}");
}

#[test]
fn fresh_v026_root_allows_daemon_autostart_without_legacy_store() {
    struct RestoreAutostartEnv(Option<std::ffi::OsString>);
    impl Drop for RestoreAutostartEnv {
        fn drop(&mut self) {
            match self.0.take() {
                Some(value) => env::set_var(DAEMON_AUTOSTART_OFF_ENV, value),
                None => env::remove_var(DAEMON_AUTOSTART_OFF_ENV),
            }
        }
    }

    let _env_lock = crate::config::TEST_LOCAL_USAGE_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _restore = RestoreAutostartEnv(env::var_os(DAEMON_AUTOSTART_OFF_ENV));
    env::remove_var(DAEMON_AUTOSTART_OFF_ENV);
    let temp = tempfile::tempdir().unwrap();
    let config = AppConfig::default();

    assert!(!database_path(temp.path().to_path_buf()).exists());
    assert!(daemon_autostart_allowed(temp.path(), &config));
    assert!(
        fs::read_dir(temp.path()).unwrap().next().is_none(),
        "eligibility inspection must not create v0.26 state"
    );
}

#[test]
fn configured_autostart_child_inherits_source_refresh_only_mode() {
    struct RestoreModeEnv(Option<std::ffi::OsString>);
    impl Drop for RestoreModeEnv {
        fn drop(&mut self) {
            match self.0.take() {
                Some(value) => env::set_var(DAEMON_MODE_ENV, value),
                None => env::remove_var(DAEMON_MODE_ENV),
            }
        }
    }

    let _env_lock = crate::config::TEST_LOCAL_USAGE_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _restore = RestoreModeEnv(env::var_os(DAEMON_MODE_ENV));
    env::set_var(DAEMON_MODE_ENV, "source-refresh-only");
    let temp = tempfile::tempdir().unwrap();

    let command = configured_daemon_autostart_command(
        Path::new("ctx"),
        temp.path(),
        DaemonTriggerCommandArg::Search,
        None,
    );
    let mode = command
        .get_envs()
        .find(|(key, _)| *key == std::ffi::OsStr::new(DAEMON_MODE_ENV))
        .and_then(|(_, value)| value)
        .and_then(std::ffi::OsStr::to_str);

    assert_eq!(mode, Some("source-refresh-only"));
}

#[cfg(unix)]
#[test]
fn autostart_child_detaches_from_the_invoking_terminal_session() -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("record-session.sh");
    let receipt = temp.path().join("session.txt");
    fs::write(
        &executable,
        "#!/bin/sh\nprintf '%s ' \"$$\" >\"$CTX_DAEMON_TEST_RECEIPT\"\nps -o sid= -p \"$$\" >>\"$CTX_DAEMON_TEST_RECEIPT\"\nexec sleep 30\n",
    )?;
    let mut permissions = fs::metadata(&executable)?.permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&executable, permissions)?;

    let mut command = daemon_autostart_command(
        &executable,
        temp.path(),
        DaemonTriggerCommandArg::Setup,
        Some(5),
        Some(5),
        None,
    );
    command.env("CTX_DAEMON_TEST_RECEIPT", &receipt);
    let mut child = command.spawn()?;
    for _ in 0..100 {
        if fs::read_to_string(&receipt)
            .is_ok_and(|recorded| recorded.split_whitespace().count() == 2)
        {
            break;
        }
        std::thread::sleep(StdDuration::from_millis(10));
    }
    let recorded = fs::read_to_string(&receipt);
    child.kill()?;
    child.wait()?;
    let recorded = recorded?;
    let values = recorded
        .split_whitespace()
        .map(str::parse::<u32>)
        .collect::<std::result::Result<Vec<_>, _>>()?;

    assert_eq!(values, vec![child.id(), child.id()]);
    Ok(())
}

#[test]
fn upgrade_handoff_fences_daemon_starts_and_aborts_on_drop() -> Result<()> {
    let temp = tempfile::tempdir()?;
    {
        let handoff =
            begin_daemon_upgrade_handoff(temp.path(), "ua_01890f3e-2c80-7000-8000-000000000001")?;
        assert!(daemon_upgrade_handoff_is_active(temp.path()));
        assert!(daemon_upgrade_handoff_blocks_current_process(temp.path()));
        assert_eq!(
            read_daemon_upgrade_handoff(temp.path())
                .and_then(|value| value["phase"].as_str().map(ToOwned::to_owned))
                .as_deref(),
            Some("ready")
        );
        drop(handoff);
    }
    assert!(!daemon_upgrade_handoff_is_active(temp.path()));
    assert_eq!(
        read_daemon_upgrade_handoff(temp.path())
            .and_then(|value| value["phase"].as_str().map(ToOwned::to_owned))
            .as_deref(),
        Some("aborted")
    );
    Ok(())
}

#[test]
fn current_daemon_fences_starts_before_transferring_to_helper() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let daemon_lock = DaemonLock::acquire(temp.path())?
        .ok_or_else(|| anyhow!("test daemon could not acquire its process lock"))?;
    let attempt_id = "ua_01890f3e-2c80-7000-8000-00000000000c";
    let handoff = begin_current_daemon_upgrade_handoff(
        temp.path(),
        attempt_id,
        DaemonTriggerCommandArg::Search,
    )?;

    assert!(daemon_upgrade_handoff_blocks_current_process(temp.path()));
    assert_eq!(
        read_daemon_upgrade_handoff(temp.path())
            .and_then(|value| value["phase"].as_str().map(ToOwned::to_owned))
            .as_deref(),
        Some("ready")
    );

    mark_replacement_helper_handoff(temp.path(), attempt_id, process::id())?;
    handoff.transfer_to_replacement_helper(process::id())?;
    drop(daemon_lock);

    assert!(daemon_upgrade_handoff_is_active(temp.path()));
    assert_eq!(
        read_daemon_upgrade_handoff(temp.path())
            .and_then(|value| value["phase"].as_str().map(ToOwned::to_owned))
            .as_deref(),
        Some("scheduled")
    );
    Ok(())
}

#[test]
fn running_daemon_cooperatively_releases_its_lock_for_upgrade() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().to_path_buf();
    let ready = Arc::new(Barrier::new(2));
    let daemon_ready = Arc::clone(&ready);
    let daemon = std::thread::spawn(move || -> Result<()> {
        let lock = DaemonLock::acquire(&root)?
            .ok_or_else(|| anyhow!("test daemon could not acquire its process lock"))?;
        daemon_ready.wait();
        let deadline = Instant::now() + StdDuration::from_secs(5);
        while !daemon_upgrade_handoff_blocks_current_process(&root) {
            if Instant::now() >= deadline {
                return Err(anyhow!("test daemon did not observe upgrade handoff"));
            }
            std::thread::sleep(DAEMON_UPGRADE_POLL_INTERVAL);
        }
        drop(lock);
        Ok(())
    });
    ready.wait();

    let handoff =
        begin_daemon_upgrade_handoff(temp.path(), "ua_01890f3e-2c80-7000-8000-000000000006")?;
    assert!(!daemon_lock_is_active(temp.path()));
    assert_eq!(
        handoff.replacement_restart().map(|(trigger, _, _)| trigger),
        Some("search")
    );
    daemon.join().expect("join test daemon")?;
    drop(handoff);
    Ok(())
}

#[test]
fn scheduled_helper_owns_fence_until_its_process_exits() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let handoff =
        begin_daemon_upgrade_handoff(temp.path(), "ua_01890f3e-2c80-7000-8000-000000000002")?;
    mark_replacement_helper_handoff(
        temp.path(),
        "ua_01890f3e-2c80-7000-8000-000000000002",
        process::id(),
    )?;
    handoff.transfer_to_replacement_helper(process::id())?;
    assert!(daemon_upgrade_handoff_is_active(temp.path()));
    assert_eq!(
        read_daemon_upgrade_handoff(temp.path())
            .and_then(|value| value["phase"].as_str().map(ToOwned::to_owned))
            .as_deref(),
        Some("scheduled")
    );
    assert!(
        begin_daemon_upgrade_handoff(temp.path(), "ua_01890f3e-2c80-7000-8000-000000000003")
            .is_err()
    );
    Ok(())
}

#[test]
fn scheduled_fence_does_not_remain_owned_by_old_parent() -> Result<()> {
    let temp = tempfile::tempdir()?;
    write_daemon_upgrade_handoff(temp.path(), "handoff", "scheduled", None)?;
    assert!(!daemon_upgrade_handoff_is_active(temp.path()));
    Ok(())
}

#[test]
fn rollback_reexec_preserves_daemon_restart_intent() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut handoff =
        begin_daemon_upgrade_handoff(temp.path(), "ua_01890f3e-2c80-7000-8000-000000000007")?;
    handoff.restart_trigger = Some(DaemonTriggerCommandArg::Search);
    handoff.prepare_reexec()?;

    assert_eq!(
        read_daemon_restart_request(temp.path()).map(|(_, trigger)| trigger.as_str()),
        Some("search")
    );
    assert!(!daemon_upgrade_handoff_is_active(temp.path()));
    Ok(())
}

#[test]
fn legacy_rollback_does_not_leave_v026_restart_request() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join(CONFIG_FILE), "[daemon]\nenabled = false\n")?;
    fs::write(database_path(temp.path().to_path_buf()), b"")?;
    write_daemon_restart_request(
        temp.path(),
        DaemonTriggerCommandArg::Setup,
        "ua_01890f3e-2c80-7000-8000-000000000009",
    )?;

    begin_daemon_upgrade_handoff(temp.path(), "ua_01890f3e-2c80-7000-8000-000000000009")?
        .resume_legacy_reexec_with(Path::new("unused-while-daemon-is-disabled"))?;

    assert!(read_daemon_restart_request(temp.path()).is_none());
    assert_eq!(
        read_daemon_upgrade_handoff(temp.path())
            .and_then(|value| value["phase"].as_str().map(ToOwned::to_owned))
            .as_deref(),
        Some("completed")
    );
    Ok(())
}

#[test]
fn failed_legacy_daemon_restart_does_not_block_reexec_or_restore_v026_request() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join(CONFIG_FILE), "[daemon]\nenabled = true\n")?;
    fs::write(database_path(temp.path().to_path_buf()), b"")?;
    write_daemon_restart_request(
        temp.path(),
        DaemonTriggerCommandArg::Setup,
        "ua_01890f3e-2c80-7000-8000-00000000000a",
    )?;

    let warning =
        begin_daemon_upgrade_handoff(temp.path(), "ua_01890f3e-2c80-7000-8000-00000000000a")?
            .resume_legacy_reexec_with(&temp.path().join("missing-v025-ctx"))?;

    assert!(warning
        .as_deref()
        .is_some_and(|warning| warning.contains("continuing rollback recovery")));
    assert!(read_daemon_restart_request(temp.path()).is_none());
    assert_eq!(
        read_daemon_upgrade_handoff(temp.path())
            .and_then(|value| value["phase"].as_str().map(ToOwned::to_owned))
            .as_deref(),
        Some("aborted")
    );
    Ok(())
}

#[test]
fn legacy_readiness_is_bound_to_child_status_and_optional_query_endpoint() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let child_pid = process::id();
    write_daemon_status(
        temp.path(),
        &json!({
            "schema_version": 1,
            "status": "running",
            "pid": child_pid,
            "start_mode": "auto",
            "trigger_command": "setup",
        }),
    )?;

    assert!(legacy_daemon_status_is_ready(
        temp.path(),
        child_pid,
        DaemonTriggerCommandArg::Setup
    ));
    assert!(!legacy_daemon_status_is_ready(
        temp.path(),
        child_pid.wrapping_add(1),
        DaemonTriggerCommandArg::Setup
    ));
    assert!(!legacy_daemon_query_endpoint_is_ready(
        temp.path(),
        child_pid
    ));

    write_private_json_file(
        &daemon_query_endpoint_path(temp.path()),
        &json!({
            "schema_version": 1,
            "transport": "unix",
            "path": "/tmp/ctx-legacy-ready.sock",
            "token": "0123456789abcdef0123456789abcdef",
            "pid": child_pid.wrapping_add(1),
        }),
    )?;
    assert!(!legacy_daemon_query_endpoint_is_ready(
        temp.path(),
        child_pid
    ));

    write_private_json_file(
        &daemon_query_endpoint_path(temp.path()),
        &json!({
            "schema_version": 1,
            "transport": "unix",
            "path": "/tmp/ctx-legacy-ready.sock",
            "token": "0123456789abcdef0123456789abcdef",
            "pid": child_pid,
        }),
    )?;
    assert!(legacy_daemon_query_endpoint_is_ready(
        temp.path(),
        child_pid
    ));
    assert!(
        !legacy_daemon_query_service_is_ready(temp.path(), child_pid),
        "endpoint metadata without a live protocol response is not readiness"
    );

    clear_legacy_daemon_readiness(temp.path())?;
    assert!(read_daemon_status(temp.path()).is_none());
    assert!(!daemon_query_endpoint_path(temp.path()).exists());
    Ok(())
}

#[test]
fn durable_daemon_disable_wins_over_handoff_restart() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join(CONFIG_FILE), "[daemon]\nenabled = false\n")?;
    fs::write(database_path(temp.path().to_path_buf()), b"")?;
    write_daemon_restart_request(
        temp.path(),
        DaemonTriggerCommandArg::Setup,
        "ua_01890f3e-2c80-7000-8000-000000000005",
    )?;
    begin_daemon_upgrade_handoff(temp.path(), "ua_01890f3e-2c80-7000-8000-000000000005")?
        .resume_with(Path::new("definitely-not-a-real-ctx-executable"))?;
    assert_eq!(
        read_daemon_upgrade_handoff(temp.path())
            .and_then(|value| value["phase"].as_str().map(ToOwned::to_owned))
            .as_deref(),
        Some("completed")
    );
    assert!(read_daemon_restart_request(temp.path()).is_none());
    Ok(())
}

#[test]
fn replacement_handoff_waits_for_daemon_ready_ack() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path().to_path_buf();
    let handoff_id = "ua_01890f3e-2c80-7000-8000-000000000008";
    let _lock = DaemonLock::acquire(&root)?
        .ok_or_else(|| anyhow!("test daemon could not acquire its process lock"))?;
    write_daemon_upgrade_handoff(&root, handoff_id, "scheduled", Some(process::id()))?;
    write_daemon_restart_request(&root, DaemonTriggerCommandArg::Search, handoff_id)?;

    let worker_root = root.clone();
    let worker = std::thread::spawn(move || {
        complete_replacement_daemon_handoff(
            &worker_root,
            Path::new("unused-while-daemon-lock-is-active"),
            handoff_id,
            Some(("search", 5, 5)),
        )
    });
    std::thread::sleep(StdDuration::from_millis(100));
    assert!(
        !worker.is_finished(),
        "handoff completed before the daemon acknowledged readiness"
    );

    acknowledge_daemon_restart_requests(&root);
    worker.join().expect("join replacement handoff")?;
    assert_eq!(
        read_daemon_upgrade_handoff(&root)
            .and_then(|value| value["phase"].as_str().map(ToOwned::to_owned))
            .as_deref(),
        Some("scheduled")
    );
    finish_replacement_daemon_handoff(&root, handoff_id)?;
    assert_eq!(
        read_daemon_upgrade_handoff(&root)
            .and_then(|value| value["phase"].as_str().map(ToOwned::to_owned))
            .as_deref(),
        Some("completed")
    );
    Ok(())
}

#[test]
fn replacement_daemon_receives_only_its_handoff_bypass_token() {
    let command = daemon_autostart_command(
        Path::new("ctx"),
        Path::new("/tmp/ctx-daemon-upgrade-test"),
        DaemonTriggerCommandArg::Search,
        Some(5),
        Some(5),
        Some("handoff-token"),
    );
    let env = command
        .get_envs()
        .map(|(key, value)| (key.to_owned(), value.map(ToOwned::to_owned)))
        .collect::<Vec<_>>();
    assert!(env.iter().any(|(key, value)| {
        key == DAEMON_UPGRADE_HANDOFF_TOKEN_ENV
            && value.as_deref() == Some(std::ffi::OsStr::new("handoff-token"))
    }));
}

#[test]
fn setup_handoff_wait_accepts_authoritative_running_observation_without_sleep() -> Result<()> {
    let status = json!({
        "status": "running",
        "pid": 41,
        "heartbeat_at_ms": 1234,
        "config_reload": {"status": "applied"},
    });
    let mut observations = std::collections::VecDeque::from([
        DaemonHandoffObservation::Pending,
        daemon_handoff_observation_from(Some(&status), Some(41), true, Some(41), None, 1234),
    ]);
    let pauses = std::cell::Cell::new(0);

    let handoff = wait_for_daemon_handoff_with(
        3,
        || {
            observations
                .pop_front()
                .unwrap_or(DaemonHandoffObservation::Pending)
        },
        || Ok(None),
        || pauses.set(pauses.get() + 1),
    )?;

    assert_eq!(
        handoff,
        DaemonHandoff {
            pid: 41,
            heartbeat_at_ms: 1234,
        }
    );
    assert_eq!(pauses.get(), 1);
    Ok(())
}

#[test]
fn setup_handoff_waits_for_requested_config_instead_of_previous_applied_mode() {
    let expected = AppConfig::default();
    let previous = json!({
        "status": "running",
        "pid": 41,
        "heartbeat_at_ms": 1234,
        "config_reload": {
            "status": "applied",
            "applied": {
                "daemon_enabled": expected.daemon.enabled,
                "daemon_mode": "source-refresh-only",
                "semantic_enabled": expected.semantic_search_enabled(),
            },
        },
    });
    assert_eq!(
        daemon_handoff_observation_from(
            Some(&previous),
            Some(41),
            true,
            None,
            Some(&expected),
            1234,
        ),
        DaemonHandoffObservation::Pending
    );

    let current = json!({
        "status": "running",
        "pid": 42,
        "heartbeat_at_ms": 1235,
        "config_reload": {
            "status": "applied",
            "applied": {
                "daemon_enabled": expected.daemon.enabled,
                "daemon_mode": expected.daemon.mode.as_str(),
                "semantic_enabled": expected.semantic_search_enabled(),
            },
        },
    });
    assert_eq!(
        daemon_handoff_observation_from(
            Some(&current),
            Some(42),
            true,
            None,
            Some(&expected),
            1235,
        ),
        DaemonHandoffObservation::Running(DaemonHandoff {
            pid: 42,
            heartbeat_at_ms: 1235,
        })
    );
}

#[test]
fn setup_handoff_wait_surfaces_daemon_failure_without_sleep() {
    let status = json!({
        "status": "failed",
        "pid": 42,
        "heartbeat_at_ms": 1235,
        "last_error": "query service failed",
    });
    let pauses = std::cell::Cell::new(0);

    let error = wait_for_daemon_handoff_with(
        3,
        || daemon_handoff_observation_from(Some(&status), None, false, Some(42), None, 1235),
        || Ok(None),
        || pauses.set(pauses.get() + 1),
    )
    .expect_err("failed status must reject the handoff");

    assert_eq!(error.to_string(), "query service failed");
    assert_eq!(pauses.get(), 0);
}

#[test]
fn setup_handoff_wait_ignores_stale_or_unowned_existing_failure_without_sleep() {
    let stale = json!({
        "status": "failed",
        "pid": 42,
        "heartbeat_at_ms": 1_000,
        "last_error": "old failure",
    });
    let unowned = json!({
        "status": "failed",
        "pid": 42,
        "heartbeat_at_ms": 35_000,
        "last_error": "unowned failure",
    });

    for (status, lock_pid, lock_active) in [
        (&stale, Some(42), true),
        (&unowned, Some(43), true),
        (&unowned, Some(42), false),
    ] {
        let pauses = std::cell::Cell::new(0);
        let error = wait_for_daemon_handoff_with(
            2,
            || {
                daemon_handoff_observation_from(
                    Some(status),
                    lock_pid,
                    lock_active,
                    None,
                    None,
                    35_000,
                )
            },
            || Ok(None),
            || pauses.set(pauses.get() + 1),
        )
        .expect_err("stale or unowned existing failure must remain pending");

        assert!(error.to_string().contains("timed out"), "{error:#}");
        assert_eq!(pauses.get(), 1);
    }
}

#[test]
fn setup_handoff_wait_surfaces_fresh_owned_existing_failure_without_sleep() {
    let status = json!({
        "status": "failed",
        "pid": 42,
        "heartbeat_at_ms": 35_000,
        "last_error": "current failure",
    });
    let pauses = std::cell::Cell::new(0);

    let error = wait_for_daemon_handoff_with(
        2,
        || daemon_handoff_observation_from(Some(&status), Some(42), true, None, None, 35_000),
        || Ok(None),
        || pauses.set(pauses.get() + 1),
    )
    .expect_err("fresh failure owned by the active daemon must reject the handoff");

    assert_eq!(error.to_string(), "current failure");
    assert_eq!(pauses.get(), 0);
}

#[test]
fn setup_handoff_wait_times_out_on_status_lock_identity_race_without_sleep() {
    let status = json!({
        "status": "running",
        "pid": 43,
        "heartbeat_at_ms": 1236,
        "config_reload": {"status": "applied"},
    });
    let pauses = std::cell::Cell::new(0);

    let error = wait_for_daemon_handoff_with(
        3,
        || daemon_handoff_observation_from(Some(&status), Some(44), true, Some(43), None, 1236),
        || Ok(None),
        || pauses.set(pauses.get() + 1),
    )
    .expect_err("mismatched status and lock identities must not become ready");

    assert!(error.to_string().contains("timed out"), "{error:#}");
    assert_eq!(pauses.get(), 2);
}

#[test]
fn setup_handoff_wait_rejects_stale_or_future_heartbeat_without_sleep() {
    for heartbeat_at_ms in [1_000, 40_001] {
        let status = json!({
            "status": "running",
            "pid": 45,
            "heartbeat_at_ms": heartbeat_at_ms,
            "config_reload": {"status": "applied"},
        });
        let pauses = std::cell::Cell::new(0);

        let error = wait_for_daemon_handoff_with(
            2,
            || {
                daemon_handoff_observation_from(
                    Some(&status),
                    Some(45),
                    true,
                    Some(45),
                    None,
                    35_000,
                )
            },
            || Ok(None),
            || pauses.set(pauses.get() + 1),
        )
        .expect_err("an implausible heartbeat must not verify daemon readiness");

        assert!(error.to_string().contains("timed out"), "{error:#}");
        assert_eq!(pauses.get(), 1);
    }
}

#[test]
fn setup_handoff_wait_ignores_stale_nested_config_failure_without_sleep() {
    let status = json!({
        "status": "running",
        "pid": 45,
        "heartbeat_at_ms": 1_000,
        "last_error": "old daemon failure",
        "config_reload": {
            "status": "activation_failed",
            "last_error": "old config failure",
        },
    });
    let pauses = std::cell::Cell::new(0);

    let error = wait_for_daemon_handoff_with(
        2,
        || daemon_handoff_observation_from(Some(&status), Some(45), true, None, None, 35_000),
        || Ok(None),
        || pauses.set(pauses.get() + 1),
    )
    .expect_err("stale nested config failure must remain pending");

    assert!(error.to_string().contains("timed out"), "{error:#}");
    assert_eq!(pauses.get(), 1);
}
