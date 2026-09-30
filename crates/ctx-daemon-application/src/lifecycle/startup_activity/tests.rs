use super::*;

#[test]
fn unreaped_child_without_cpu_observations_waits_past_the_grace_until_ready() -> Result<()> {
    let root = tempfile::tempdir()?;
    let config = crate::TestHost.daemon_config(root.path())?;
    let elapsed = Cell::new(Duration::ZERO);
    let ready = DaemonHandoff {
        pid: 42,
        heartbeat_at_ms: 1,
    };
    let mut child_checks = 0;
    let handoff = wait_for_daemon_handoff_with(
        DAEMON_SETUP_HANDOFF_STALL_POLL_ATTEMPTS,
        || {
            observe(
                if elapsed.get() >= Duration::from_secs(600) {
                    DaemonHandoffObservation::Running(ready)
                } else {
                    DaemonHandoffObservation::Pending
                },
                root.path(),
                true,
                &config,
            )
        },
        || {
            child_checks += 1;
            Ok(None)
        },
        || {},
        || elapsed.set(elapsed.get() + Duration::from_secs(1)),
    )?;
    assert_eq!(handoff, ready);
    assert_eq!(elapsed.get(), Duration::from_secs(600));
    assert_eq!(child_checks, 600);
    // No lock, heartbeat or CPU sample was needed to observe this exact child.
    assert!(!daemon_lock_path(root.path()).exists());
    Ok(())
}

#[test]
fn live_startup_remains_cancellable_and_exit_or_terminal_failure_wins() -> Result<()> {
    let root = tempfile::tempdir()?;
    let config = crate::TestHost.daemon_config(root.path())?;
    let elapsed = Cell::new(Duration::ZERO);
    let error = wait_for_daemon_handoff_with_cancellation(
        DAEMON_SETUP_HANDOFF_STALL_POLL_ATTEMPTS,
        || {
            observe(
                DaemonHandoffObservation::Pending,
                root.path(),
                true,
                &config,
            )
        },
        || Ok(None),
        || {},
        || elapsed.set(elapsed.get() + Duration::from_secs(1)),
        &mut || {
            if elapsed.get() >= Duration::from_secs(20) {
                Err(anyhow!("cancelled slow startup"))
            } else {
                Ok(())
            }
        },
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "cancelled slow startup");
    assert_eq!(elapsed.get(), Duration::from_secs(20));

    let exited = wait_for_daemon_handoff_with(
        DAEMON_SETUP_HANDOFF_STALL_POLL_ATTEMPTS,
        || {
            observe(
                DaemonHandoffObservation::Pending,
                root.path(),
                true,
                &config,
            )
        },
        || Ok(Some("child exited".to_owned())),
        || {},
        || panic!("child exit must win immediately"),
    )
    .unwrap_err();
    assert_eq!(exited.to_string(), "child exited");
    let failed = DaemonHandoffObservation::Failed("startup failed".to_owned());
    assert_eq!(observe(failed.clone(), root.path(), true, &config), failed);
    Ok(())
}

#[test]
fn losing_the_child_without_a_joined_owner_restores_the_stall_bound() -> Result<()> {
    let root = tempfile::tempdir()?;
    let config = crate::TestHost.daemon_config(root.path())?;
    let child_unreaped = Cell::new(true);
    let mut pauses = 0;
    let error = wait_for_daemon_handoff_with(
        3,
        || {
            observe(
                DaemonHandoffObservation::Pending,
                root.path(),
                child_unreaped.get(),
                &config,
            )
        },
        || {
            child_unreaped.set(false);
            Ok(None)
        },
        || {},
        || pauses += 1,
    )
    .unwrap_err();
    assert!(error.is::<DaemonHandoffTimeout>());
    assert_eq!(pauses, 3);
    Ok(())
}

#[test]
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
fn joined_startup_requires_the_live_owner_and_pending_requested_config() -> Result<()> {
    let root = tempfile::tempdir()?;
    let lock = ctx_daemon_runtime::DaemonLock::acquire(root.path())?.unwrap();
    let owner = read_daemon_owner_identity(root.path())?.unwrap();
    let config = crate::TestHost.daemon_config(root.path())?;
    let status = json!({
        "status": "running", "pid": owner.pid, "started_at_ms": owner.started_at_ms,
        "config_reload": {
            "status": "pending",
            "requested": {
                "daemon_enabled": true, "daemon_mode": "full", "semantic_enabled": true,
                "semantic_executor": "builtin",
                "semantic_contract_fingerprint": "sha256:test-builtin-contract",
                "semantic_builtin_throttling_configured": true,
                "semantic_builtin_throttling_effective": true,
            },
        },
    });
    write_daemon_status(root.path(), &status)?;
    assert_eq!(
        pending_owner_identity(root.path(), &config),
        Some(owner.clone())
    );
    // A native identity match extends observation without fabricating readiness.
    assert_eq!(
        observe(
            DaemonHandoffObservation::Pending,
            root.path(),
            false,
            &config
        ),
        DaemonHandoffObservation::Starting
    );
    assert_eq!(
        daemon_handoff_observation(
            &crate::TestHost,
            root.path(),
            None,
            &config,
            DaemonReadinessRequirement::Full,
            DAEMON_HEALTH_TIMEOUT,
        ),
        DaemonHandoffObservation::Pending
    );
    let mut failed = status.clone();
    failed["status"] = json!("failed");
    write_daemon_status(root.path(), &failed)?;
    assert!(pending_owner_identity(root.path(), &config).is_none());
    let mut wrong_config = config.clone();
    wrong_config.semantic_enabled = false;
    write_daemon_status(root.path(), &status)?;
    assert!(pending_owner_identity(root.path(), &wrong_config).is_none());
    let mut wrong_owner = status.clone();
    wrong_owner["started_at_ms"] = json!(owner.started_at_ms + 1);
    write_daemon_status(root.path(), &wrong_owner)?;
    assert!(pending_owner_identity(root.path(), &config).is_none());
    write_daemon_status(root.path(), &status)?;
    let lock_path = daemon_lock_path(root.path());
    let original = fs::read(&lock_path)?;
    let mut wrong_token: Value = serde_json::from_slice(&original)?;
    wrong_token["process_creation_token"] = Value::Null;
    fs::write(&lock_path, serde_json::to_vec(&wrong_token)?)?;
    assert!(pending_owner_identity(root.path(), &config).is_none());
    fs::write(&lock_path, original)?;
    drop(lock);
    assert_eq!(
        observe(
            DaemonHandoffObservation::Pending,
            root.path(),
            false,
            &config
        ),
        DaemonHandoffObservation::Pending
    );
    Ok(())
}
