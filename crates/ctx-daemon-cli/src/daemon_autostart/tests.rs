use super::*;
use crate::config::CONFIG_FILE;
use ctx_daemon_runtime::DaemonLock;
use std::sync::{Arc, Barrier};

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
fn installation_root_discovery_is_all_root_sorted_and_deduplicated() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let registrations = temp.path().join("acks");
    write_installation_registration(&registrations, "z-root", "live", None, process::id())?;
    write_installation_registration(&registrations, "a-root", "live", None, process::id())?;
    let duplicate = fs::read(registrations.join("a-root.json"))?;
    fs::write(registrations.join("a-root-duplicate.json"), duplicate)?;

    assert_eq!(
        registered_installation_daemon_roots_from(&registrations)?,
        vec![registrations.join("a-root"), registrations.join("z-root")]
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
        loop_interval_seconds: None,
        persistent: true,
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
        None,
    )
    .expect("normalized daemon launch");
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
    )
    .expect("normalized daemon launch");
    let args = command
        .get_args()
        .filter_map(std::ffi::OsStr::to_str)
        .collect::<Vec<_>>();
    assert!(!args.contains(&"--idle-exit-seconds"), "{args:?}");
    assert!(!args.contains(&"--loop-interval-seconds"), "{args:?}");
}

#[test]
fn detached_daemon_launch_dto_keeps_only_the_explicit_loop_interval() {
    let launch = daemon_autostart_command(
        Path::new("/managed/ctx"),
        Path::new("/managed/data"),
        DaemonTriggerCommandArg::Import,
        Some(23),
        Some("handoff-token"),
    )
    .expect("normalized daemon launch");
    assert_eq!(launch.program(), Path::new("/managed/ctx"));
    assert_eq!(
        launch
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        [
            "--data-root",
            "/managed/data",
            "daemon",
            "run",
            "--start-mode",
            "auto",
            "--trigger-command",
            "import",
            "--format=json",
            "--loop-interval-seconds",
            "23",
        ]
    );
    let environment = launch
        .get_envs()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.map(|value| value.to_string_lossy().into_owned()),
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        environment.get(DAEMON_BACKGROUND_CHILD_ENV),
        Some(&Some("1".to_owned()))
    );
    assert_eq!(
        environment.get(DAEMON_UPGRADE_HANDOFF_TOKEN_ENV),
        Some(&Some("handoff-token".to_owned()))
    );
}

#[test]
fn detached_daemon_launch_freezes_the_normalized_environment() {
    struct RestoreHome(Option<OsString>);
    impl Drop for RestoreHome {
        fn drop(&mut self) {
            match self.0.take() {
                Some(value) => env::set_var("HOME", value),
                None => env::remove_var("HOME"),
            }
        }
    }

    let _env_lock = crate::config::TEST_LOCAL_USAGE_ENV_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let _restore = RestoreHome(env::var_os("HOME"));
    env::set_var("HOME", "/before-normalization");
    let launch = daemon_autostart_command(
        Path::new("ctx"),
        Path::new("/data"),
        DaemonTriggerCommandArg::Search,
        None,
        None,
    )
    .expect("normalized daemon launch");
    env::set_var("HOME", "/after-normalization");

    let home = launch
        .get_envs()
        .find(|(name, _)| *name == OsStr::new("HOME"))
        .and_then(|(_, value)| value);
    assert_eq!(home, Some(OsStr::new("/before-normalization")));
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

    assert!(!temp.path().join("work.sqlite").exists());
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
    )
    .expect("normalized configured daemon launch");
    let mode = command
        .get_envs()
        .find(|(key, _)| *key == std::ffi::OsStr::new(DAEMON_MODE_ENV))
        .and_then(|(_, value)| value)
        .and_then(std::ffi::OsStr::to_str);

    assert_eq!(mode, Some("source-refresh-only"));
}

#[test]
fn persistent_fallback_autostart_child_has_no_idle_exit() {
    let temp = tempfile::tempdir().unwrap();
    let launch = configured_daemon_autostart_command(
        Path::new("ctx"),
        temp.path(),
        DaemonTriggerCommandArg::Import,
        None,
    )
    .expect("normalized persistent fallback daemon launch");
    let args = launch
        .get_args()
        .filter_map(std::ffi::OsStr::to_str)
        .collect::<Vec<_>>();
    assert!(
        !args.contains(&"--idle-exit-seconds"),
        "persistent fallback launch must not acquire the bounded manager-unavailable lifetime: {args:?}"
    );
}

#[test]
fn mismatched_live_binary_never_joins_and_returns_one_actionable_handoff_command() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let _lock = DaemonLock::acquire(temp.path())?
        .ok_or_else(|| anyhow!("test daemon could not acquire its process lock"))?;
    let replacement = temp.path().join("replacement-ctx");
    fs::write(&replacement, b"different ctx image")?;

    let error = handoff_mismatched_daemon_owner(temp.path(), &replacement)
        .expect_err("a different binary image must not join the live owner");
    let message = error.to_string();
    assert!(message.contains("different binary image"), "{message}");
    assert_eq!(
        message
            .matches("ctx daemon disable --prepare-uninstall")
            .count(),
        1,
        "{message}"
    );
    Ok(())
}

#[test]
fn autostart_surfaces_only_the_binary_handoff_recovery_command() -> Result<()> {
    struct RestoreEnvironment {
        ci: Option<std::ffi::OsString>,
        executable: Option<std::ffi::OsString>,
    }
    impl Drop for RestoreEnvironment {
        fn drop(&mut self) {
            match self.ci.take() {
                Some(value) => env::set_var("CI", value),
                None => env::remove_var("CI"),
            }
            match self.executable.take() {
                Some(value) => env::set_var("CTX_DAEMON_AUTOSTART_EXE", value),
                None => env::remove_var("CTX_DAEMON_AUTOSTART_EXE"),
            }
        }
    }

    let _environment_lock = crate::config::TEST_LOCAL_USAGE_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _restore = RestoreEnvironment {
        ci: env::var_os("CI"),
        executable: env::var_os("CTX_DAEMON_AUTOSTART_EXE"),
    };
    let temp = tempfile::tempdir()?;
    let _lock = DaemonLock::acquire(temp.path())?
        .ok_or_else(|| anyhow!("test daemon could not acquire its process lock"))?;
    let replacement = temp.path().join("replacement-ctx");
    fs::write(&replacement, b"different ctx image")?;
    env::set_var("CI", "1");
    env::set_var("CTX_DAEMON_AUTOSTART_EXE", replacement);

    let error = autostart_daemon_and_wait(
        temp.path(),
        &AppConfig::default(),
        DaemonTriggerCommandArg::Search,
    )
    .expect_err("a mismatched live image must fail through the public autostart path");
    let message = error.to_string();
    assert_eq!(
        message
            .matches("ctx daemon disable --prepare-uninstall")
            .count(),
        1,
        "{message}"
    );
    assert!(!message.contains("ctx daemon status"), "{message}");
    assert!(!message.contains("ctx daemon run"), "{message}");
    Ok(())
}

#[cfg(unix)]
#[test]
fn stale_binary_owner_uses_cooperative_supervisor_handoff_before_releasing_lock() -> Result<()> {
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::net::UnixListener,
        sync::mpsc,
    };

    let temp = tempfile::Builder::new()
        .prefix("ctx-handoff-")
        .tempdir_in("/tmp")?;
    let owner_lock = DaemonLock::acquire(temp.path())?
        .ok_or_else(|| anyhow!("test daemon could not acquire its process lock"))?;
    let replacement = temp.path().join("replacement-ctx");
    fs::write(&replacement, b"different ctx image")?;

    let socket_path = temp.path().join("source-refresh.sock");
    let listener = UnixListener::bind(&socket_path)?;
    let token = "0123456789abcdef0123456789abcdef";
    write_private_json_file(
        &daemon_root_path(temp.path()).join("source-refresh-endpoint.json"),
        &json!({
            "schema_version": 1,
            "transport": "unix",
            "path": socket_path,
            "token": token,
            "pid": process::id(),
        }),
    )?;
    let (request_tx, request_rx) = mpsc::channel();
    let owner = std::thread::spawn(move || -> Result<()> {
        let (mut stream, _) = listener.accept()?;
        let mut request = String::new();
        BufReader::new(stream.try_clone()?).read_line(&mut request)?;
        let request: Value = serde_json::from_str(&request)?;
        request_tx.send(request.clone()).ok();
        assert_eq!(request["op"], "supervisor_handoff");
        assert_eq!(request["token"], token);
        writeln!(
            stream,
            "{}",
            serde_json::to_string(&json!({
                "ok": true,
                "schema_version": 1,
                "supervisor_handoff": "accepted",
                "pid": process::id(),
            }))?
        )?;
        stream.flush()?;
        drop(stream);
        drop(owner_lock);
        Ok(())
    });

    handoff_mismatched_daemon_owner(temp.path(), &replacement)?;
    let request = request_rx.recv_timeout(StdDuration::from_secs(1))?;
    assert_eq!(request["op"], "supervisor_handoff");
    assert!(!daemon_lock_is_active(temp.path()));
    owner.join().expect("join cooperative handoff owner")?;
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
        Some(23),
    )?;

    assert_eq!(handoff.replacement_restart(), Some(("search", Some(23))));

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
    assert_eq!(handoff.replacement_restart(), Some(("search", None)));
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
fn current_format_recovery_reexec_preserves_daemon_restart_intent() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let attempt_id = "ua_01890f3e-2c80-7000-8000-000000000007";
    write_daemon_restart_request(temp.path(), DaemonTriggerCommandArg::Search, attempt_id)?;
    begin_daemon_upgrade_handoff(temp.path(), attempt_id)?.release_for_current_format_reexec()?;

    assert_eq!(
        read_daemon_restart_request(temp.path()).map(|(_, trigger)| trigger.as_str()),
        Some("search")
    );
    assert!(!daemon_upgrade_handoff_is_active(temp.path()));
    Ok(())
}

#[test]
fn durable_daemon_disable_wins_over_handoff_restart() -> Result<()> {
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join(CONFIG_FILE), "[daemon]\nenabled = false\n")?;
    fs::write(temp.path().join("work.sqlite"), b"")?;
    write_daemon_restart_request(
        temp.path(),
        DaemonTriggerCommandArg::Setup,
        "ua_01890f3e-2c80-7000-8000-000000000005",
    )?;
    let replacement = temp.path().join("replacement-ctx");
    fs::write(&replacement, b"replacement ctx image")?;
    begin_daemon_upgrade_handoff(temp.path(), "ua_01890f3e-2c80-7000-8000-000000000005")?
        .resume_with(&replacement)?;
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
            Some(("search", Some(23))),
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
        Some("finalizing")
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
        Some("handoff-token"),
    )
    .expect("normalized daemon launch");
    let env = command
        .get_envs()
        .map(|(key, value)| (key.to_owned(), value.map(ToOwned::to_owned)))
        .collect::<Vec<_>>();
    assert!(env.iter().any(|(key, value)| {
        key == DAEMON_UPGRADE_HANDOFF_TOKEN_ENV
            && value.as_deref() == Some(std::ffi::OsStr::new("handoff-token"))
    }));
}
