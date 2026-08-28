use super::*;
use crate::{SEMANTIC_EMBEDDING_TOKEN_ENDPOINT_ENV, SEMANTIC_EMBEDDING_TOKEN_ENV};

#[test]
fn supervisor_reinstalls_rotated_scrubbed_and_reenabled_semantic_credentials() -> Result<()> {
    let _env_lock = crate::test_environment_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let _restore = RestoreTestEnvironment::capture(&[
        SEMANTIC_EMBEDDING_TOKEN_ENV,
        SEMANTIC_EMBEDDING_TOKEN_ENDPOINT_ENV,
    ]);
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("ctx");
    let backend = FakeSupervisorBackend::default();
    let endpoint = "https://embeddings.example.test/";
    let enabled_http = crate::DaemonConfigSnapshot {
        enabled: true,
        mode: crate::DaemonMode::Full,
        semantic_enabled: true,
        semantic_executor: endpoint.to_owned(),
    };

    env::set_var(SEMANTIC_EMBEDDING_TOKEN_ENV, "token-a");
    env::set_var(SEMANTIC_EMBEDDING_TOKEN_ENDPOINT_ENV, endpoint);
    let mut input_a = ManagedSupervisorInput::new(&TestHost, temp.path(), &executable)?;
    input_a.daemon_environment = configured_supervisor_environment_for_config(
        supervisor_environment_snapshot(&TestHost)?,
        temp.path(),
        None,
        &enabled_http,
    )?;
    backend.expect_environment(&input_a.daemon_environment);
    assert_eq!(
        ensure_native_supervisor_with(&TestHost, &input_a, &backend)?,
        DaemonSupervisorStart::Native
    );

    env::set_var(SEMANTIC_EMBEDDING_TOKEN_ENV, "token-b");
    env::set_var(SEMANTIC_EMBEDDING_TOKEN_ENDPOINT_ENV, endpoint);
    let mut input_b = ManagedSupervisorInput::new(&TestHost, temp.path(), &executable)?;
    input_b.daemon_environment = configured_supervisor_environment_for_config(
        supervisor_environment_snapshot(&TestHost)?,
        temp.path(),
        None,
        &enabled_http,
    )?;
    backend.expect_environment(&input_b.daemon_environment);
    assert_eq!(
        ensure_native_supervisor_with(&TestHost, &input_b, &backend)?,
        DaemonSupervisorStart::Native
    );

    let mut disabled = ManagedSupervisorInput::new(&TestHost, temp.path(), &executable)?;
    let disabled_config = crate::DaemonConfigSnapshot {
        semantic_enabled: false,
        ..enabled_http.clone()
    };
    disabled.daemon_environment = configured_supervisor_environment_for_config(
        supervisor_environment_snapshot(&TestHost)?,
        temp.path(),
        None,
        &disabled_config,
    )?;
    backend.expect_environment(&disabled.daemon_environment);
    assert_eq!(
        ensure_native_supervisor_with(&TestHost, &disabled, &backend)?,
        DaemonSupervisorStart::Native
    );

    let builtin_config = crate::DaemonConfigSnapshot {
        semantic_executor: "builtin".to_owned(),
        ..enabled_http.clone()
    };
    let builtin_environment = configured_supervisor_environment_for_config(
        supervisor_environment_snapshot(&TestHost)?,
        temp.path(),
        None,
        &builtin_config,
    )?;
    assert_eq!(
        builtin_environment.identity_sha256(),
        disabled.daemon_environment.identity_sha256()
    );

    let mut reenabled = ManagedSupervisorInput::new(&TestHost, temp.path(), &executable)?;
    reenabled.daemon_environment = configured_supervisor_environment_for_config(
        supervisor_environment_snapshot(&TestHost)?,
        temp.path(),
        None,
        &enabled_http,
    )?;
    backend.expect_environment(&reenabled.daemon_environment);
    assert_eq!(
        ensure_native_supervisor_with(&TestHost, &reenabled, &backend)?,
        DaemonSupervisorStart::Native
    );

    let state = backend.state.lock().unwrap();
    assert_eq!(state.installs, 4);
    assert_eq!(
        state.installed_environment_sha256,
        Some(reenabled.daemon_environment.identity_sha256().to_owned())
    );
    Ok(())
}

#[test]
fn preserved_registration_hands_same_binary_fallback_to_native_once() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("ctx");
    let backend = FakeSupervisorBackend::with_registration(None);
    backend.state.lock().unwrap().detached_owner_live = true;

    let input = ManagedSupervisorInput::new(&TestHost, temp.path(), &executable)?;
    assert_eq!(
        ensure_native_supervisor_with(&TestHost, &input, &backend)?,
        DaemonSupervisorStart::Native
    );
    assert_eq!(
        ensure_native_supervisor_with(&TestHost, &input, &backend)?,
        DaemonSupervisorStart::Native
    );

    let state = backend.state.lock().unwrap();
    assert!(!state.detached_owner_live);
    assert_eq!(state.handoffs, 1);
    assert_eq!(state.starts, 1);
    assert_eq!(state.installs, 0);
    assert_eq!(state.disables, 0);
    assert_eq!(state.live_owner, Some(4_242));
    drop(state);
    let receipt = stored_supervisor_report(temp.path());
    assert_eq!(receipt["status"], "installed");
    assert_eq!(receipt["owner_pid"], 4_242);
    Ok(())
}

#[test]
fn unavailable_manager_falls_back_before_native_mutation_under_the_installation_lock() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("ctx");
    let backend = FakeSupervisorBackend::default();
    backend.state.lock().unwrap().manager_unavailable = true;

    let result = ensure_native_supervisor_with(
        &TestHost,
        &ManagedSupervisorInput::new(&TestHost, temp.path(), &executable)?,
        &backend,
    )?;
    assert_eq!(result, DaemonSupervisorStart::ManagerUnavailable);
    let state = backend.state.lock().unwrap();
    assert_eq!(state.manager_probes, 2);
    assert_eq!(state.mutation_preparations, 0);
    assert_eq!(state.registration_probes, 0);
    assert_eq!(state.installs, 0);
    assert_eq!(state.disables, 0);
    assert_eq!(state.starts, 0);
    drop(state);
    assert!(ctx_daemon_runtime::daemon_root_path(temp.path())
        .join("supervisor-installation.lock")
        .exists());

    let report = stored_supervisor_report(temp.path());
    assert_eq!(report["status"], "manager_unavailable");
    assert_eq!(report["autostart_supported"], false);
    assert_eq!(report["restart_supported"], false);
    assert_eq!(report["registration_verified"], false);
    assert_eq!(report["live_owner_verified"], false);
    assert!(report["limitation"]
        .as_str()
        .is_some_and(|value| value.contains("persistent detached daemon")));
    assert!(report["limitation"]
        .as_str()
        .is_some_and(|value| value.contains("automatic restart")));
    Ok(())
}

#[test]
fn fallback_repairs_and_native_recovery_reuse_the_persisted_loop_interval() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("ctx");
    let backend = FakeSupervisorBackend::default();
    backend.state.lock().unwrap().manager_unavailable = true;
    let mut input = ManagedSupervisorInput::new(&TestHost, temp.path(), &executable)?;
    input.daemon_environment = input
        .daemon_environment
        .with_loop_interval_seconds(Some(23))?;

    assert_eq!(
        ensure_native_supervisor_with(&TestHost, &input, &backend)?,
        DaemonSupervisorStart::ManagerUnavailable
    );
    assert_eq!(
        persisted_supervisor_loop_interval_seconds(temp.path()),
        Some(23)
    );

    for _repair_attempt in 0..2 {
        let launch = lifecycle::configured_daemon_autostart_command(
            &executable,
            temp.path(),
            crate::DaemonTrigger::Search,
            None,
        )?;
        let args = launch
            .args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--loop-interval-seconds", "23"]));
    }

    let environment_with_different_ambient_override = input
        .daemon_environment
        .clone()
        .with_loop_interval_seconds(Some(11))?;
    let recovered_native_environment = configured_supervisor_environment_from_snapshot(
        environment_with_different_ambient_override,
        temp.path(),
        None,
    )?;
    assert_eq!(
        recovered_native_environment.loop_interval_seconds(),
        Some(23)
    );

    let explicit_upgrade_environment = configured_supervisor_environment_from_snapshot(
        recovered_native_environment,
        temp.path(),
        Some(41),
    )?;
    assert_eq!(
        explicit_upgrade_environment.loop_interval_seconds(),
        Some(41)
    );
    Ok(())
}

#[test]
fn unavailable_manager_receipt_waits_for_the_installation_lock() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let data_root = temp.path().to_path_buf();
    let executable = data_root.join("ctx");
    let held_lock = SupervisorInstallationLock::acquire(&data_root)?;
    let backend = Arc::new(FakeSupervisorBackend::default());
    backend.state.lock().unwrap().manager_unavailable = true;

    let worker_backend = Arc::clone(&backend);
    let worker_root = data_root.clone();
    let worker_executable = executable.clone();
    let worker = std::thread::spawn(move || {
        ensure_native_supervisor_with(
            &TestHost,
            &ManagedSupervisorInput::new(&TestHost, &worker_root, &worker_executable)?,
            worker_backend.as_ref(),
        )
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while backend.state.lock().unwrap().manager_probes == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "manager preflight did not reach the held installation lock"
        );
        std::thread::yield_now();
    }
    assert!(
        !ctx_daemon_runtime::daemon_root_path(&data_root)
            .join("supervisor.json")
            .exists(),
        "manager-unavailable receipt must not race ahead of the installation lock"
    );

    drop(held_lock);
    assert_eq!(
        worker.join().expect("join manager-unavailable setup")?,
        DaemonSupervisorStart::ManagerUnavailable
    );
    assert_eq!(
        stored_supervisor_report(&data_root)["status"],
        "manager_unavailable"
    );
    Ok(())
}

#[test]
fn unavailable_manager_artifact_inspection_errors_remain_fatal() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let blocked_parent = temp.path().join("artifact-parent-is-a-file");
    fs::write(&blocked_parent, b"not a directory")?;
    let backend = FakeSupervisorBackend {
        artifact_path_override: Some(blocked_parent.join("ctx.service")),
        ..FakeSupervisorBackend::default()
    };
    backend.state.lock().unwrap().manager_unavailable = true;

    let error = ensure_native_supervisor_with(
        &TestHost,
        &ManagedSupervisorInput::new(&TestHost, temp.path(), &temp.path().join("ctx"))?,
        &backend,
    )
    .expect_err("artifact metadata errors must not be treated as absence");
    assert!(
        error
            .to_string()
            .contains("inspect native supervisor artifact"),
        "{error:#}"
    );
    assert!(!ctx_daemon_runtime::daemon_root_path(temp.path())
        .join("supervisor.json")
        .exists());
    Ok(())
}

#[test]
fn manager_loss_after_partial_registration_preserves_state_and_falls_back() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("ctx");
    let backend = FakeSupervisorBackend {
        fail_install_after_registration: true,
        manager_unavailable_after_install: true,
        ..FakeSupervisorBackend::default()
    };

    let result = ensure_native_supervisor_with(
        &TestHost,
        &ManagedSupervisorInput::new(&TestHost, temp.path(), &executable)?,
        &backend,
    )?;
    assert_eq!(result, DaemonSupervisorStart::ManagerUnavailable);
    let state = backend.state.lock().unwrap();
    assert!(state.registered);
    assert_eq!(state.installs, 1);
    assert_eq!(state.disables, 0);
    drop(state);

    let report = stored_supervisor_report(temp.path());
    assert_eq!(report["status"], "manager_unavailable");
    assert!(report["limitation"]
        .as_str()
        .is_some_and(|value| value.contains("state was preserved")));
    Ok(())
}

#[test]
fn manager_loss_during_partial_cleanup_is_a_degraded_fallback() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("ctx");
    let backend = FakeSupervisorBackend {
        fail_install_without_registration: true,
        fail_disable: true,
        manager_unavailable_on_disable_failure: true,
        ..FakeSupervisorBackend::default()
    };

    let result = ensure_native_supervisor_with(
        &TestHost,
        &ManagedSupervisorInput::new(&TestHost, temp.path(), &executable)?,
        &backend,
    )?;
    assert_eq!(result, DaemonSupervisorStart::ManagerUnavailable);
    let state = backend.state.lock().unwrap();
    assert_eq!(state.installs, 1);
    assert_eq!(state.disables, 1);
    assert!(state.manager_unavailable);
    drop(state);
    assert_eq!(
        stored_supervisor_report(temp.path())["status"],
        "manager_unavailable"
    );
    Ok(())
}

#[test]
fn manager_unavailable_upgrade_receipt_waits_for_lock_and_preserves_fence() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let data_root = temp.path().to_path_buf();
    let executable = data_root.join("ctx");
    let held_lock = SupervisorInstallationLock::acquire(&data_root)?;
    let backend = Arc::new(FakeSupervisorBackend::with_registration(Some(4_242)));
    backend.state.lock().unwrap().manager_unavailable = true;
    let fence_released = Arc::new(AtomicBool::new(false));

    let worker_backend = Arc::clone(&backend);
    let worker_root = data_root.clone();
    let worker_executable = executable.clone();
    let worker_released = Arc::clone(&fence_released);
    let worker = std::thread::spawn(move || {
        let mut fence = TestSupervisorUpgradeFence(Some(move || {
            worker_released.store(true, Ordering::SeqCst);
            Ok(())
        }));
        resume_daemon_supervisor_after_upgrade_with(
            &TestHost,
            &worker_root,
            &worker_executable,
            worker_backend.as_ref(),
            None,
            &mut fence,
        )
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while backend.state.lock().unwrap().manager_probes == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "upgrade manager preflight did not reach the held installation lock"
        );
        std::thread::yield_now();
    }
    assert!(!fence_released.load(Ordering::SeqCst));
    assert!(!ctx_daemon_runtime::daemon_root_path(&data_root)
        .join("supervisor.json")
        .exists());

    drop(held_lock);
    assert_eq!(
        worker.join().expect("join manager-unavailable upgrade")?,
        DaemonSupervisorUpgradeResume::ManagerUnavailable
    );
    assert!(!fence_released.load(Ordering::SeqCst));
    assert_eq!(
        stored_supervisor_report(&data_root)["status"],
        "manager_unavailable"
    );
    Ok(())
}

#[test]
fn operational_manager_cleanup_and_probe_integrity_failures_remain_fatal() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("ctx");
    let cleanup_failure = FakeSupervisorBackend {
        fail_install_without_registration: true,
        fail_disable: true,
        ..FakeSupervisorBackend::default()
    };
    let error = ensure_native_supervisor_with(
        &TestHost,
        &ManagedSupervisorInput::new(&TestHost, temp.path(), &executable)?,
        &cleanup_failure,
    )
    .expect_err("an operational manager cleanup failure must remain fatal");
    assert!(format!("{error:#}").contains("fake installer failed"));
    assert_eq!(cleanup_failure.state.lock().unwrap().disables, 1);

    let probe_failure = FakeSupervisorBackend {
        manager_probe_error: true,
        ..FakeSupervisorBackend::default()
    };
    let error = ensure_native_supervisor_with(
        &TestHost,
        &ManagedSupervisorInput::new(&TestHost, temp.path(), &executable)?,
        &probe_failure,
    )
    .expect_err("manager identity/probe errors must remain fatal");
    assert!(error.to_string().contains("identity probe failed"));
    let state = probe_failure.state.lock().unwrap();
    assert_eq!(state.installs, 0);
    assert_eq!(state.disables, 0);
    drop(state);

    let ownership_failure = FakeSupervisorBackend {
        mutation_preparation_error: true,
        ..FakeSupervisorBackend::default()
    };
    let error = ensure_native_supervisor_with(
        &TestHost,
        &ManagedSupervisorInput::new(&TestHost, temp.path(), &executable)?,
        &ownership_failure,
    )
    .expect_err("daemon ownership preparation failures must remain fatal");
    assert!(error.to_string().contains("ownership preparation failed"));
    let state = ownership_failure.state.lock().unwrap();
    assert_eq!(state.mutation_preparations, 1);
    assert_eq!(state.installs, 0);
    assert_eq!(state.disables, 0);
    Ok(())
}

#[test]
fn operational_manager_without_an_identity_verified_owner_remains_fatal() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("ctx");
    let backend = FakeSupervisorBackend {
        fail_start: true,
        ..FakeSupervisorBackend::with_registration(None)
    };

    let error = ensure_native_supervisor_with(
        &TestHost,
        &ManagedSupervisorInput::new(&TestHost, temp.path(), &executable)?,
        &backend,
    )
    .expect_err("operational manager ownership failures must not degrade to fallback");
    assert!(
        format!("{error:#}").contains("identity-verified daemon ownership"),
        "{error:#}"
    );
    let state = backend.state.lock().unwrap();
    assert_eq!(state.starts, 1);
    assert_eq!(state.installs, 0);
    drop(state);
    assert_eq!(
        stored_supervisor_report(temp.path())["status"],
        "registered_not_running"
    );
    Ok(())
}

#[test]
fn unavailable_manager_prevents_native_disable_mutation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let backend = FakeSupervisorBackend::with_registration(Some(4_242));
    backend.state.lock().unwrap().manager_unavailable = true;

    let error = disable_native_supervisor_candidate_with(
        temp.path(),
        Some(temp.path().join("ctx")),
        &backend,
    )
    .expect_err("disable must preserve native state while its manager is unavailable");
    assert!(
        error
            .to_string()
            .contains("no registration state was changed"),
        "{error:#}"
    );
    let state = backend.state.lock().unwrap();
    assert_eq!(state.manager_probes, 1);
    assert_eq!(state.disables, 0);
    assert!(state.registered);
    Ok(())
}

#[test]
fn upgrade_handoff_releases_fence_before_native_manager_start() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("ctx");
    let backend = FakeSupervisorBackend::with_registration(None);
    let mut fence = TestSupervisorUpgradeFence(Some(|| {
        backend.state.lock().unwrap().upgrade_fence_released = true;
        Ok(())
    }));
    let result = resume_daemon_supervisor_after_upgrade_with(
        &TestHost,
        temp.path(),
        &executable,
        &backend,
        None,
        &mut fence,
    )?;
    assert_eq!(result, DaemonSupervisorUpgradeResume::Native);
    let state = backend.state.lock().unwrap();
    assert_eq!(state.starts, 1);
    assert!(state.start_observed_released_fence);
    assert_eq!(state.live_owner, Some(4_242));
    drop(state);
    let report = stored_supervisor_report(temp.path());
    assert_eq!(report["status"], "installed");
    assert_eq!(report["owner_pid"], 4_242);
    Ok(())
}

#[test]
fn upgrade_handoff_keeps_fence_for_detached_fallback_without_native_registration() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("ctx");
    let backend = FakeSupervisorBackend::default();
    let fence_released = AtomicBool::new(false);
    let mut fence = TestSupervisorUpgradeFence(Some(|| {
        fence_released.store(true, Ordering::SeqCst);
        Ok(())
    }));
    let environment_snapshot = supervisor_environment_snapshot(&TestHost)?
        .with_loop_interval_seconds(Some(23))?
        .contract_report();
    let result = resume_daemon_supervisor_after_upgrade_with(
        &TestHost,
        temp.path(),
        &executable,
        &backend,
        Some(environment_snapshot),
        &mut fence,
    )?;
    assert_eq!(result, DaemonSupervisorUpgradeResume::Fallback);
    assert!(!fence_released.load(Ordering::SeqCst));
    assert_eq!(backend.state.lock().unwrap().starts, 0);
    let report = stored_supervisor_report(temp.path());
    assert_eq!(report["status"], "fallback");
    assert_eq!(report["environment_snapshot"]["loop_interval_seconds"], 23);
    let launch = lifecycle::configured_daemon_autostart_command(
        &executable,
        temp.path(),
        crate::DaemonTrigger::Search,
        None,
    )?;
    let args = launch
        .args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert!(args
        .windows(2)
        .any(|pair| pair == ["--loop-interval-seconds", "23"]));
    Ok(())
}

#[test]
fn status_revalidates_registration_and_live_owner_instead_of_replaying_receipt() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("ctx");
    let backend = FakeSupervisorBackend::with_registration(Some(4_242));
    write_installed_receipt(
        temp.path(),
        &executable,
        backend.artifact_path(temp.path())?,
        4_242,
        Some(supervisor_environment_snapshot(&TestHost)?.contract_report()),
    )?;

    backend.state.lock().unwrap().live_owner = Some(7_331);
    let restarted = revalidated_supervisor_report_with(&TestHost, temp.path(), &backend);
    assert_eq!(restarted["status"], "installed");
    assert_eq!(restarted["registration_verified"], true);
    assert_eq!(restarted["live_owner_verified"], true);
    assert_eq!(restarted["owner_pid"], 7_331);

    backend.state.lock().unwrap().live_owner = None;
    let stopped = revalidated_supervisor_report_with(&TestHost, temp.path(), &backend);
    assert_eq!(stopped["status"], "registered_not_running");
    assert_eq!(stopped["registration_verified"], true);
    assert_eq!(stopped["live_owner_verified"], false);
    assert_eq!(stopped["owner_pid"], Value::Null);

    backend.state.lock().unwrap().registered = false;
    let stale = revalidated_supervisor_report_with(&TestHost, temp.path(), &backend);
    assert_eq!(stale["status"], "stale_registration");
    assert_eq!(stale["registration_verified"], false);
    assert_eq!(stale["live_owner_verified"], false);
    Ok(())
}

#[test]
fn status_reports_manager_unavailability_without_registration_or_lock_mutation() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("ctx");
    let backend = FakeSupervisorBackend::with_registration(Some(4_242));
    write_installed_receipt(
        temp.path(),
        &executable,
        backend.artifact_path(temp.path())?,
        4_242,
        Some(supervisor_environment_snapshot(&TestHost)?.contract_report()),
    )?;
    let receipt_path = ctx_daemon_runtime::daemon_root_path(temp.path()).join("supervisor.json");
    let mut receipt: Value = serde_json::from_slice(&fs::read(&receipt_path)?)?;
    receipt["environment_snapshot"]["sha256"] = json!("0".repeat(64));
    ctx_daemon_runtime::write_private_json_file(&receipt_path, &receipt)?;
    backend.state.lock().unwrap().manager_unavailable = true;

    let report = revalidated_supervisor_report_with(&TestHost, temp.path(), &backend);
    assert_eq!(report["status"], "manager_unavailable");
    assert_eq!(report["registration_verified"], false);
    assert_eq!(report["live_owner_verified"], false);
    assert_eq!(report["owner_pid"], Value::Null);
    assert_eq!(report["autostart_supported"], false);
    assert_eq!(report["restart_supported"], false);
    assert_eq!(
        report["environment_snapshot"]["restart_required"], false,
        "manager unavailability must remain the actionable persistence limitation"
    );
    let state = backend.state.lock().unwrap();
    assert_eq!(state.manager_probes, 1);
    assert_eq!(state.registration_probes, 0);
    drop(state);
    assert!(!ctx_daemon_runtime::daemon_root_path(temp.path())
        .join("supervisor-installation.lock")
        .exists());
    Ok(())
}

#[test]
fn status_invalidates_healthy_receipt_when_current_launch_environment_is_unreadable() -> Result<()>
{
    let temp = tempfile::tempdir()?;
    write_supervisor_receipt(
        temp.path(),
        &SupervisorReceipt {
            kind: native_supervisor_kind().to_owned(),
            status: "installed",
            autostart_supported: true,
            restart_supported: true,
            registration_verified: true,
            live_owner_verified: true,
            owner_pid: Some(4_242),
            artifact_path: Some(temp.path().join("native")),
            executable_path: Some(temp.path().join("ctx")),
            limitation: None,
            last_error: None,
        },
    )?;
    let report = daemon_supervisor_report_with_normalized_environment(
        &TestHost,
        temp.path(),
        Err(anyhow!("HOME is not Unicode")),
    );
    assert_eq!(report["status"], "environment_invalid");
    assert_eq!(report["registration_verified"], false);
    assert_eq!(report["live_owner_verified"], false);
    assert_eq!(report["owner_pid"], Value::Null);
    assert!(report["revalidation_error"]
        .as_str()
        .is_some_and(|error| error.contains("not trusted")));
    Ok(())
}

#[cfg(unix)]
#[test]
fn native_control_context_accepts_nonunicode_manager_values_without_launch_snapshot() -> Result<()>
{
    use std::os::unix::ffi::OsStringExt as _;

    let manager_environment = normalized_supervisor_manager_environment(BTreeMap::from([(
        OsString::from("HOME"),
        OsString::from_vec(vec![b'/', 0xff]),
    )]))?;
    let backend = PlatformNativeSupervisor::new(
        &TestHost,
        Path::new("/tmp/ctx-control-test"),
        None,
        &manager_environment,
    )?;
    assert!(backend.launch_environment().is_err());
    // Removal/control mechanics consume only the manager context. This keeps
    // uninstall available when the launch-only environment cannot be Unicode.
    assert!(backend
        .artifact_path(Path::new("/tmp/ctx-control-test"))?
        .is_some());
    Ok(())
}

#[test]
fn status_flags_environment_mismatch_without_exposing_credential_derived_hashes() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("ctx");
    let backend = FakeSupervisorBackend::with_registration(Some(4_242));
    write_installed_receipt(
        temp.path(),
        &executable,
        backend.artifact_path(temp.path())?,
        4_242,
        Some(supervisor_environment_snapshot(&TestHost)?.contract_report()),
    )?;
    let receipt_path = ctx_daemon_runtime::daemon_root_path(temp.path()).join("supervisor.json");
    let mut installed: Value = serde_json::from_slice(&fs::read(&receipt_path)?)?;
    installed["environment_snapshot"]["sha256"] = json!("0".repeat(64));
    installed["environment_snapshot"]["captured_at_ms"] = json!(1234);
    ctx_daemon_runtime::write_private_json_file(&receipt_path, &installed)?;

    let report = revalidated_supervisor_report_with(&TestHost, temp.path(), &backend);
    assert!(report["environment_snapshot"].get("sha256").is_none());
    assert!(report["environment_snapshot"]
        .get("current_sha256")
        .is_none());
    assert_eq!(report["environment_snapshot"]["captured_at_ms"], 1234);
    assert_eq!(report["environment_snapshot"]["restart_required"], true);
    assert_eq!(report["environment_snapshot"]["values_exposed"], false);
    Ok(())
}

#[test]
fn supervisor_report_states_forced_termination_identity_limitations() {
    let temp = tempfile::tempdir().unwrap();
    let report = daemon_supervisor_report(&TestHost, temp.path());
    if cfg!(target_os = "linux") {
        assert_eq!(
            report["forced_termination_identity"]["strategy"],
            "pidfd_when_available"
        );
        assert!(report["forced_termination_identity"]["limitation"]
            .as_str()
            .is_some_and(|value| value.contains("PID reuse")));
    } else if cfg!(unix) {
        assert_eq!(
            report["forced_termination_identity"]["strategy"],
            "reverified_pid"
        );
        assert!(report["forced_termination_identity"]["limitation"]
            .as_str()
            .is_some_and(|value| value.contains("cannot eliminate")));
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[test]
fn supervisor_live_ownership_requires_exact_manager_pid_and_executable() {
    let temp = tempfile::tempdir().unwrap();
    let _lock = ctx_daemon_runtime::DaemonLock::acquire(temp.path())
        .unwrap()
        .expect("daemon lock");
    let executable = env::current_exe().unwrap();
    assert_eq!(
        verify_daemon_owner_identity(temp.path(), &executable, Some(std::process::id())).unwrap(),
        std::process::id()
    );
    assert!(verify_daemon_owner_identity(
        temp.path(),
        &executable,
        Some(std::process::id().saturating_add(1)),
    )
    .is_err());
    assert!(verify_daemon_owner_identity(
        temp.path(),
        &temp.path().join("not-the-owner"),
        Some(std::process::id()),
    )
    .is_err());
}

#[test]
fn fallback_disable_status_is_retry_safe_without_claiming_registration() {
    let temp = tempfile::tempdir().unwrap();
    write_supervisor_receipt(
        temp.path(),
        &SupervisorReceipt {
            kind: "cli_self_heal".to_owned(),
            status: "fallback",
            autostart_supported: false,
            restart_supported: false,
            registration_verified: false,
            live_owner_verified: false,
            owner_pid: None,
            artifact_path: None,
            executable_path: None,
            limitation: Some("test limitation".to_owned()),
            last_error: None,
        },
    )
    .unwrap();
    disable_daemon_supervisor(&TestHost, temp.path()).unwrap();
    disable_daemon_supervisor(&TestHost, temp.path()).unwrap();
    let status = daemon_supervisor_report(&TestHost, temp.path());
    assert_eq!(status["status"], "disabled");
    assert_eq!(status["registration_verified"], false);
    assert_eq!(status["live_owner_verified"], false);
    assert_eq!(status["autostart_supported"], false);
    assert_eq!(status["restart_supported"], false);
}

#[test]
fn native_disable_attempts_surviving_registration_without_artifact_or_launch_probe() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let backend = FakeSupervisorBackend::with_registration(Some(4_242));

    disable_native_supervisor_candidate_with(temp.path(), Some(temp.path().join("ctx")), &backend)?;

    let state = backend.state.lock().unwrap();
    assert_eq!(state.disables, 1);
    assert_eq!(state.registration_probes, 0);
    assert!(!state.registered);
    drop(state);
    let receipt = stored_supervisor_report(temp.path());
    assert_eq!(receipt["status"], "disabled");
    assert_eq!(receipt["registration_verified"], false);
    Ok(())
}

#[test]
fn native_disable_failure_does_not_claim_an_unavailable_launch_probe_is_healthy() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let mut backend = FakeSupervisorBackend::with_registration(Some(4_242));
    backend.fail_disable = true;

    assert!(disable_native_supervisor_candidate_with(
        temp.path(),
        Some(temp.path().join("ctx")),
        &backend,
    )
    .is_err());

    let state = backend.state.lock().unwrap();
    assert_eq!(state.disables, 1);
    assert_eq!(state.registration_probes, 0);
    assert!(state.registered);
    drop(state);
    let receipt = stored_supervisor_report(temp.path());
    assert_eq!(receipt["status"], "disable_failed");
    assert_eq!(receipt["registration_verified"], false);
    assert!(receipt["last_error"]
        .as_str()
        .is_some_and(|error| error.contains("failed")));
    Ok(())
}

#[test]
fn canonical_supervisor_root_is_independent_of_ctx_data_root_override() {
    static ENV_LOCK: Mutex<()> = Mutex::new(());
    let _guard = ENV_LOCK.lock().unwrap();
    let previous = env::var_os("CTX_DATA_ROOT");
    let canonical = ctx_history_platform::managed_data_root().unwrap();
    let custom = canonical.with_file_name("ctx-custom-supervisor-test");
    env::set_var("CTX_DATA_ROOT", &custom);

    assert!(is_canonical_managed_data_root(&canonical).unwrap());
    assert!(!is_canonical_managed_data_root(&custom).unwrap());

    if let Some(previous) = previous {
        env::set_var("CTX_DATA_ROOT", previous);
    } else {
        env::remove_var("CTX_DATA_ROOT");
    }
}

#[test]
fn explicit_root_is_noncanonical_when_managed_home_is_unavailable() {
    let custom = Path::new("/explicit/ctx-root");

    assert!(!is_canonical_managed_data_root_with(
        custom,
        Err(ctx_history_platform::PlatformError::MissingHome)
    )
    .unwrap());
}
