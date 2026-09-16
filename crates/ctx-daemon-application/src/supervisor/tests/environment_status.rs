use super::*;

fn install_environment_fixture(
    data_root: &Path,
    snapshot: &SupervisorEnvironmentSnapshot,
) -> Result<SupervisorSpec> {
    let executable = data_root.join("ctx");
    let identity = environment::supervisor_identity("ctx", data_root.join("native"))?;
    let spec = environment::supervisor_artifact_spec(identity, &executable, data_root, snapshot)?;
    ctx_daemon_runtime::write_supervisor_environment(&spec)?;
    write_installed_receipt(
        data_root,
        &executable,
        Some(spec.identity().artifact_path().to_owned()),
        4_242,
        Some(snapshot.contract_report()),
    )?;
    Ok(spec)
}

fn observe_installed_environment(
    data_root: &Path,
    spec: &SupervisorSpec,
    backend: &FakeSupervisorBackend,
) -> Result<Value> {
    let observed = report::supervisor_environment_snapshot_for_registration(&TestHost, data_root)?;
    let observed_spec = environment::supervisor_artifact_spec(
        spec.identity().clone(),
        spec.launch().program(),
        data_root,
        &observed,
    )?;
    // Exercise the actual native registration's environment verifier, then
    // substitute only the manager's registration and live-owner responses.
    ctx_daemon_runtime::verify_supervisor_environment(&observed_spec)?;
    backend.expect_environment(&observed);
    Ok(revalidated_supervisor_report_with(
        &TestHost, data_root, backend,
    ))
}

#[test]
fn observer_shell_does_not_change_installed_supervisor_health() -> Result<()> {
    const CONTEXT: &[(&str, &str)] = &[
        ("LANG", "C.UTF-8"),
        ("LC_ALL", "C"),
        ("TZ", "America/New_York"),
        ("TMPDIR", "/observer/tmp"),
        ("TMP", "/observer/tmp"),
        ("TEMP", "/observer/tmp"),
        ("DBUS_SESSION_BUS_ADDRESS", "unix:path=/observer/bus"),
        ("XDG_RUNTIME_DIR", "/observer/runtime"),
    ];
    let _env_lock = crate::test_environment_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let temp = tempfile::tempdir()?;
    let _restore =
        RestoreTestEnvironment::capture(&CONTEXT.iter().map(|(name, _)| *name).collect::<Vec<_>>());
    for (name, _) in CONTEXT {
        env::remove_var(name);
    }
    let snapshot = configured_supervisor_environment(&TestHost, temp.path(), None)?;
    let spec = install_environment_fixture(temp.path(), &snapshot)?;
    let receipt_path = ctx_daemon_runtime::daemon_root_path(temp.path()).join("supervisor.json");
    let original_receipt = fs::read(&receipt_path)?;
    let original_environment = fs::read(spec.environment_path())?;
    let backend = FakeSupervisorBackend::with_registration(Some(4_242));
    backend.state.lock().unwrap().installed_environment_sha256 =
        Some(snapshot.identity_sha256().to_owned());

    for (name, value) in CONTEXT {
        for value in [Some(*value), None] {
            match value {
                Some(value) => env::set_var(name, value),
                None => env::remove_var(name),
            }
            let report = observe_installed_environment(temp.path(), &spec, &backend)?;
            assert_eq!(report["status"], "installed", "{name}={value:?}");
            assert_eq!(report["registration_verified"], true);
            assert_eq!(report["live_owner_verified"], true);
            assert_eq!(report["owner_pid"], 4_242);
            assert_eq!(report["environment_snapshot"]["restart_required"], false);
            assert_eq!(report["revalidation_error"], Value::Null);
        }
    }
    assert_eq!(fs::read(&receipt_path)?, original_receipt);
    assert_eq!(fs::read(spec.environment_path())?, original_environment);
    // A temporary manager loss can preserve an already-installed service.
    // Its recorded artifact must still use the installed environment.
    let mut preserved: Value = serde_json::from_slice(&original_receipt)?;
    preserved["status"] = json!("manager_unavailable");
    ctx_daemon_runtime::write_private_json_file(&receipt_path, &preserved)?;
    env::set_var("LANG", "C.UTF-8");
    let recovered = observe_installed_environment(temp.path(), &spec, &backend)?;
    assert_eq!(recovered["status"], "installed");
    assert_eq!(recovered["environment_snapshot"]["restart_required"], false);
    assert_eq!(fs::read(spec.environment_path())?, original_environment);
    let state = backend.state.lock().unwrap();
    assert_eq!(
        (state.installs, state.starts, state.disables, state.handoffs),
        (0, 0, 0, 0)
    );
    Ok(())
}

#[test]
fn provider_policy_and_network_changes_require_explicit_recapture() -> Result<()> {
    const CHANGES: &[(&str, &str)] = &[
        ("CODEX_HOME", "/another/profile"),
        ("CLAUDE_CONFIG_DIR", "/another/claude"),
        ("HOME", "/another/home"),
        ("XDG_CONFIG_HOME", "/another/config"),
        ("XDG_DATA_HOME", "/another/data"),
        ("CTX_ANALYTICS_ENABLED", "false"),
        ("CTX_UPGRADE_AUTO", "false"),
        ("HTTPS_PROXY", "http://proxy.example.test:8080"),
        ("SSL_CERT_FILE", "/another/ca.pem"),
    ];
    let _env_lock = crate::test_environment_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let temp = tempfile::tempdir()?;
    for (name, value) in CHANGES {
        let _restore = RestoreTestEnvironment::capture(&[*name]);
        env::remove_var(name);
        let snapshot = configured_supervisor_environment(&TestHost, temp.path(), None)?;
        let spec = install_environment_fixture(temp.path(), &snapshot)?;
        let backend = FakeSupervisorBackend::with_registration(Some(4_242));
        backend.state.lock().unwrap().installed_environment_sha256 =
            Some(snapshot.identity_sha256().to_owned());
        assert_eq!(
            observe_installed_environment(temp.path(), &spec, &backend)?["environment_snapshot"]
                ["restart_required"],
            false,
        );

        env::set_var(name, value);
        let report = observe_installed_environment(temp.path(), &spec, &backend)?;
        assert_eq!(report["status"], "installed", "{name}");
        assert_eq!(
            report["environment_snapshot"]["restart_required"], true,
            "{name}"
        );
        assert_eq!(report["owner_pid"], 4_242);
        assert_eq!(backend.state.lock().unwrap().installs, 0);

        // Explicit setup still installs a changed provider/policy snapshot.
        let input = ManagedSupervisorInput::new(&TestHost, temp.path(), spec.launch().program())?;
        backend.expect_environment(&input.daemon_environment);
        ensure_native_supervisor_with(&TestHost, &input, &backend)?;
        assert_eq!(backend.state.lock().unwrap().installs, 1, "{name}");
        install_environment_fixture(temp.path(), &input.daemon_environment)?;
        assert_eq!(
            observe_installed_environment(temp.path(), &spec, &backend)?["environment_snapshot"]
                ["restart_required"],
            false,
        );
        env::remove_var(name);
        assert_eq!(
            observe_installed_environment(temp.path(), &spec, &backend)?["environment_snapshot"]
                ["restart_required"],
            true,
            "removing {name} also changes the requested snapshot",
        );
    }
    Ok(())
}

#[test]
fn installed_environment_damage_is_not_hidden_by_observer_independence() -> Result<()> {
    let _env_lock = crate::test_environment_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let temp = tempfile::tempdir()?;
    let snapshot = configured_supervisor_environment(&TestHost, temp.path(), None)?;
    let spec = install_environment_fixture(temp.path(), &snapshot)?;
    let mut changed: Value = serde_json::from_slice(&fs::read(spec.environment_path())?)?;
    changed["environment"][0]["value"] = json!("changed after installation");
    ctx_daemon_runtime::write_private_json_file(spec.environment_path(), &changed)?;
    let error = report::supervisor_environment_snapshot_for_registration(&TestHost, temp.path())
        .expect_err("changed environment must not validate itself");
    assert!(error.to_string().contains("does not match its receipt"));
    let report = daemon_supervisor_report(&TestHost, temp.path());
    assert_eq!(report["status"], "environment_invalid");
    assert_eq!(report["registration_verified"], false);
    assert_eq!(report["live_owner_verified"], false);

    fs::remove_file(spec.environment_path())?;
    assert!(
        report::supervisor_environment_snapshot_for_registration(&TestHost, temp.path()).is_err()
    );
    install_environment_fixture(temp.path(), &snapshot)?;
    let backend = FakeSupervisorBackend::with_registration(Some(4_242));
    backend.state.lock().unwrap().installed_environment_sha256 =
        Some(snapshot.identity_sha256().to_owned());
    backend.state.lock().unwrap().registered = false;
    let report = observe_installed_environment(temp.path(), &spec, &backend)?;
    assert_eq!(report["status"], "stale_registration");
    assert_eq!(report["registration_verified"], false);
    assert_eq!(report["live_owner_verified"], false);
    let input = ManagedSupervisorInput::new(&TestHost, temp.path(), spec.launch().program())?;
    backend.expect_environment(&input.daemon_environment);
    ensure_native_supervisor_with(&TestHost, &input, &backend)?;
    assert_eq!(backend.state.lock().unwrap().installs, 1);
    assert_eq!(
        observe_installed_environment(temp.path(), &spec, &backend)?["status"],
        "installed"
    );
    Ok(())
}
