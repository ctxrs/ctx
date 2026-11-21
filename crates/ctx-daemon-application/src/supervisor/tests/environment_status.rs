use super::*;

#[test]
fn upgrade_resume_preserves_installed_contract_despite_observer_drift() -> Result<()> {
    let _env_lock = crate::test_environment_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let temp = tempfile::tempdir()?;
    let _restore = RestoreTestEnvironment::capture(&["LANG", "CTX_SEARCH_SEMANTIC"]);
    env::set_var("LANG", "C");
    env::set_var("CTX_SEARCH_SEMANTIC", "false");
    let snapshot = configured_supervisor_environment(&TestHost, temp.path(), Some(23))?;
    let spec = install_environment_fixture(temp.path(), &snapshot)?;
    let system_root = Path::new(r"C:\Windows");
    let sid = "S-1-0-0";
    let xml = ctx_daemon_runtime::windows_task_xml(&spec, system_root, sid)?;
    let manager = supervisor_manager_environment(&TestHost)?;

    env::set_var("LANG", "C.UTF-8");
    env::set_var("CTX_SEARCH_SEMANTIC", "true");
    let recaptured = configured_supervisor_environment(&TestHost, temp.path(), None)?;
    assert!(snapshot.requires_restart(&recaptured));
    let recaptured_spec = environment::supervisor_artifact_spec(
        spec.identity().clone(),
        spec.launch().program(),
        temp.path(),
        &recaptured,
    )?;
    assert!(ctx_daemon_runtime::verify_supervisor_environment(&recaptured_spec).is_err());

    for requested_interval in [None, Some(23)] {
        let resumed = resumed_supervisor_environment(&TestHost, temp.path(), requested_interval)?;
        assert_eq!(resumed, snapshot);
        let resumed_spec = environment::supervisor_artifact_spec(
            spec.identity().clone(),
            spec.launch().program(),
            temp.path(),
            &resumed,
        )?;
        ctx_daemon_runtime::verify_supervisor_environment(&resumed_spec)?;
        assert!(ctx_daemon_runtime::windows_task_registration_matches(
            &xml,
            &resumed_spec,
            system_root,
            sid,
            &manager,
        )?);
    }
    assert_eq!(
        resumed_supervisor_environment(&TestHost, temp.path(), Some(41))?.loop_interval_seconds(),
        Some(41),
    );

    let receipt_path = ctx_daemon_runtime::daemon_root_path(temp.path()).join("supervisor.json");
    let mut receipt = stored_supervisor_report(temp.path());
    receipt["environment_snapshot"]["sha256"] = json!("0".repeat(64));
    ctx_daemon_runtime::write_private_json_file(&receipt_path, &receipt)?;
    assert!(resumed_supervisor_environment(&TestHost, temp.path(), None).is_err());
    install_environment_fixture(temp.path(), &snapshot)?;
    let mut changed: Value = serde_json::from_slice(&fs::read(spec.environment_path())?)?;
    changed["environment"][0]["value"] = json!("changed after installation");
    ctx_daemon_runtime::write_private_json_file(spec.environment_path(), &changed)?;
    assert!(resumed_supervisor_environment(&TestHost, temp.path(), None).is_err());
    fs::write(spec.environment_path(), b"not JSON")?;
    assert!(resumed_supervisor_environment(&TestHost, temp.path(), None).is_err());
    Ok(())
}

#[test]
fn explicit_setup_refreshes_a_copied_managed_root_snapshot() -> Result<()> {
    let _env_lock = crate::test_environment_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let _restore = RestoreTestEnvironment::capture(&["CTX_DATA_ROOT"]);
    let temp = tempfile::tempdir()?;
    let old_root = temp.path().join("old");
    let new_root = temp.path().join("new");
    env::set_var("CTX_DATA_ROOT", &old_root);
    let old_snapshot = configured_supervisor_environment(&TestHost, &old_root, None)?;
    assert!(old_snapshot.values.contains(&(
        "CTX_DATA_ROOT".to_owned(),
        old_root.to_str().unwrap().to_owned(),
    )));
    // A manual copy retains the old environment and its receipt. Status and
    // upgrade resume preserve that contract until explicit index mode auto.
    let copied = install_environment_fixture(&new_root, &old_snapshot)?;
    env::set_var("CTX_DATA_ROOT", &new_root);
    assert_eq!(
        resumed_supervisor_environment(&TestHost, &new_root, None)?,
        old_snapshot
    );
    let backend = FakeSupervisorBackend::with_registration(Some(4_242));
    backend.state.lock().unwrap().installed_environment_sha256 =
        Some(old_snapshot.identity_sha256().to_owned());
    let input = ManagedSupervisorInput::new(&TestHost, &new_root, copied.launch().program())?;
    assert!(old_snapshot.requires_restart(&input.daemon_environment));
    assert!(input.daemon_environment.values.contains(&(
        "CTX_DATA_ROOT".to_owned(),
        new_root.to_str().unwrap().to_owned(),
    )));
    backend.expect_environment(&input.daemon_environment);
    ensure_native_supervisor_with(&TestHost, &input, &backend)?;
    assert_eq!(backend.state.lock().unwrap().installs, 1);
    let refreshed = install_environment_fixture(&new_root, &input.daemon_environment)?;
    let installed = ctx_daemon_runtime::read_supervisor_environment(refreshed.environment_path())?;
    assert_eq!(
        installed.get(OsStr::new("CTX_DATA_ROOT")),
        Some(&new_root.clone().into_os_string())
    );
    assert_eq!(
        resumed_supervisor_environment(&TestHost, &new_root, None)?,
        input.daemon_environment
    );

    env::remove_var("CTX_DATA_ROOT");
    let default = ctx_history_platform::managed_data_root()?;
    let snapshot = configured_supervisor_environment(&TestHost, &new_root, None)?;
    assert!(
        snapshot.values.contains(&(
            "CTX_DATA_ROOT".to_owned(),
            default.to_str().unwrap().to_owned(),
        )),
        "a command override must not become the managed selection"
    );
    Ok(())
}

#[cfg(windows)]
#[test]
fn upgrade_resume_restores_only_the_canonical_managed_root_alias() -> Result<()> {
    let _env_lock = crate::test_environment_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let temp = tempfile::tempdir()?;
    let managed = temp.path().join(".ctx");
    let resume_root = |root: &Path| resume_windows_supervisor_data_root(root, Ok(managed.clone()));
    fs::create_dir_all(&managed)?;
    let canonical = fs::canonicalize(&managed)?;
    assert_ne!(canonical, managed);
    assert_eq!(resume_root(&canonical), managed);
    assert_eq!(resume_root(&managed), managed);
    let custom = temp.path().join("custom");
    fs::create_dir_all(&custom)?;
    assert_eq!(resume_root(&custom), custom);
    let custom = fs::canonicalize(custom)?;
    assert_eq!(resume_root(&custom), custom);

    let snapshot = configured_supervisor_environment(&TestHost, &managed, Some(23))?;
    let spec = install_environment_fixture(&managed, &snapshot)?;
    let system_root = Path::new(r"C:\Windows");
    let sid = "S-1-0-0";
    let xml = ctx_daemon_runtime::windows_task_xml(&spec, system_root, sid)?;
    let manager = supervisor_manager_environment(&TestHost)?;
    for (root, matches) in [(&canonical, false), (&resume_root(&canonical), true)] {
        let identity = environment::supervisor_identity("ctx", root.join("native"))?;
        let resumed_spec = environment::supervisor_artifact_spec(
            identity,
            spec.launch().program(),
            root,
            &snapshot,
        )?;
        ctx_daemon_runtime::verify_supervisor_environment(&resumed_spec)?;
        assert_eq!(
            ctx_daemon_runtime::windows_task_registration_matches(
                &xml,
                &resumed_spec,
                system_root,
                sid,
                &manager,
            )?,
            matches
        );
    }
    for root in [&custom, &canonical] {
        assert_eq!(
            resume_windows_supervisor_data_root(root, Err(PlatformError::MissingHome)),
            *root,
        );
        assert_eq!(
            resume_windows_supervisor_data_root(root, Ok(temp.path().join("missing-home"))),
            *root,
        );
    }
    Ok(())
}

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

#[test]
fn ensure_preserves_installed_environment_across_caller_process_context_drift() -> Result<()> {
    const CONTEXT: &[(&str, &str)] = &[
        ("LANG", "C.UTF-8"),
        ("LC_ALL", "C"),
        ("TZ", "America/Los_Angeles"),
        ("TMPDIR", "/caller/subshell/tmp"),
        ("TMP", "/caller/subshell/tmp"),
        ("TEMP", "/caller/subshell/tmp"),
        ("DBUS_SESSION_BUS_ADDRESS", "unix:path=/caller/bus"),
        ("XDG_RUNTIME_DIR", "/caller/runtime"),
    ];
    let _env_lock = crate::test_environment_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let _restore =
        RestoreTestEnvironment::capture(&CONTEXT.iter().map(|(name, _)| *name).collect::<Vec<_>>());
    for (name, _) in CONTEXT {
        env::remove_var(name);
    }
    let temp = tempfile::tempdir()?;
    let installed_snapshot = configured_supervisor_environment(&TestHost, temp.path(), None)?;
    let spec = install_environment_fixture(temp.path(), &installed_snapshot)?;
    let original_environment = fs::read(spec.environment_path())?;
    let backend = FakeSupervisorBackend::with_registration(Some(37_687));
    backend.state.lock().unwrap().installed_environment_sha256 =
        Some(installed_snapshot.identity_sha256().to_owned());

    for (name, value) in CONTEXT {
        env::set_var(name, value);
    }
    let input = ManagedSupervisorInput::new(&TestHost, temp.path(), spec.launch().program())?;
    assert_eq!(
        input.daemon_environment.identity_sha256(),
        installed_snapshot.identity_sha256(),
        "process-context-only shell drift must preserve the installed environment snapshot"
    );
    let verified_spec = environment::supervisor_artifact_spec(
        spec.identity().clone(),
        spec.launch().program(),
        temp.path(),
        &input.daemon_environment,
    )?;
    ctx_daemon_runtime::verify_supervisor_environment(&verified_spec)?;
    backend.expect_environment(&input.daemon_environment);
    assert_eq!(
        ensure_native_supervisor_with(&TestHost, &input, &backend)?,
        DaemonSupervisorStart::Native
    );
    let state = backend.state.lock().unwrap();
    assert_eq!((state.installs, state.starts, state.handoffs), (0, 0, 0));
    drop(state);
    assert_eq!(fs::read(spec.environment_path())?, original_environment);
    Ok(())
}

#[test]
fn preserve_live_owner_records_recovery_reason_without_reinstalling_or_replacing_environment(
) -> Result<()> {
    let _env_lock = crate::test_environment_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let _restore = RestoreTestEnvironment::capture(&["CODEX_HOME"]);
    env::remove_var("CODEX_HOME");
    let temp = tempfile::tempdir()?;
    let installed_snapshot = configured_supervisor_environment(&TestHost, temp.path(), None)?;
    let spec = install_environment_fixture(temp.path(), &installed_snapshot)?;
    let original_environment = fs::read(spec.environment_path())?;
    let backend = FakeSupervisorBackend::with_registration(Some(37_687));
    backend.state.lock().unwrap().installed_environment_sha256 =
        Some(installed_snapshot.identity_sha256().to_owned());

    env::set_var("CODEX_HOME", "/caller/different-codex-home");
    let input = ManagedSupervisorInput::new(&TestHost, temp.path(), spec.launch().program())?;
    backend.expect_environment(&input.daemon_environment);
    assert_eq!(
        ensure_native_supervisor_with_policy(
            &TestHost,
            &input,
            &backend,
            SupervisorMutationPolicy::PreserveLiveOwner,
        )?,
        DaemonSupervisorStart::Fallback
    );
    let state = backend.state.lock().unwrap();
    assert_eq!(
        (
            state.installs,
            state.mutation_preparations,
            state.starts,
            state.handoffs
        ),
        (0, 0, 0, 0)
    );
    drop(state);
    assert_eq!(fs::read(spec.environment_path())?, original_environment);
    let receipt = stored_supervisor_report(temp.path());
    assert_eq!(receipt["status"], "stale_registration");
    assert_eq!(receipt["owner_pid"], 37_687);
    assert!(receipt["recovery_reason"]
        .as_str()
        .is_some_and(|reason| reason.contains("preserved_live_owner")));

    // Explicit AllowReinstall still reinstalls and records why recovery ran.
    assert_eq!(
        ensure_native_supervisor_with(&TestHost, &input, &backend)?,
        DaemonSupervisorStart::Native
    );
    assert_eq!(backend.state.lock().unwrap().installs, 1);
    let reinstalled = stored_supervisor_report(temp.path());
    assert_eq!(reinstalled["status"], "installed");
    assert!(reinstalled["recovery_reason"]
        .as_str()
        .is_some_and(|reason| reason.contains("reinstalled_registration")));
    Ok(())
}

#[test]
fn preserve_live_owner_records_detached_and_inspection_denied_receipts_without_mutation(
) -> Result<()> {
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("ctx");
    let input = ManagedSupervisorInput::new(&TestHost, temp.path(), &executable)?;

    // Valid registration + detached live owner -> preserved without handoff/start.
    let detached_backend = FakeSupervisorBackend::with_registration(None);
    detached_backend.state.lock().unwrap().detached_owner_live = true;
    detached_backend.state.lock().unwrap().existing_live_owner = Some(42_101);
    assert_eq!(
        ensure_native_supervisor_with_policy(
            &TestHost,
            &input,
            &detached_backend,
            SupervisorMutationPolicy::PreserveLiveOwner,
        )?,
        DaemonSupervisorStart::Fallback
    );
    {
        let state = detached_backend.state.lock().unwrap();
        assert!(state.detached_owner_live);
        assert_eq!((state.handoffs, state.starts, state.installs), (0, 0, 0));
    }
    let receipt = stored_supervisor_report(temp.path());
    assert_eq!(receipt["status"], "registered_not_running");
    assert_eq!(receipt["registration_verified"], true);
    assert_eq!(receipt["live_owner_verified"], false);
    assert_eq!(receipt["owner_pid"], 42_101);
    assert!(receipt["recovery_reason"]
        .as_str()
        .is_some_and(|reason| reason.contains("preserved_live_owner")));

    // Valid registration + denied inspection -> registered_unverified without handoff/start or PID claim.
    let denied_registered = FakeSupervisorBackend::with_registration(None);
    denied_registered
        .state
        .lock()
        .unwrap()
        .live_owner_inspection_denied = Some(42_102);
    assert_eq!(
        ensure_native_supervisor_with_policy(
            &TestHost,
            &input,
            &denied_registered,
            SupervisorMutationPolicy::PreserveLiveOwner,
        )?,
        DaemonSupervisorStart::Fallback
    );
    {
        let state = denied_registered.state.lock().unwrap();
        assert_eq!((state.handoffs, state.starts, state.installs), (0, 0, 0));
    }
    let receipt = stored_supervisor_report(temp.path());
    assert_eq!(receipt["status"], "registered_unverified");
    assert_eq!(receipt["registration_verified"], true);
    assert_eq!(receipt["live_owner_verified"], false);
    assert!(receipt["owner_pid"].is_null());
    assert!(receipt["recovery_reason"]
        .as_str()
        .is_some_and(|reason| reason.contains("preserved_unverified_live_owner")));

    // Stale/missing registration + denied inspection -> stale_registration without mutation/install.
    let denied_stale = FakeSupervisorBackend::default();
    denied_stale
        .state
        .lock()
        .unwrap()
        .existing_owner_inspection_denied = Some(42_103);
    assert_eq!(
        ensure_native_supervisor_with_policy(
            &TestHost,
            &input,
            &denied_stale,
            SupervisorMutationPolicy::PreserveLiveOwner,
        )?,
        DaemonSupervisorStart::Fallback
    );
    {
        let state = denied_stale.state.lock().unwrap();
        assert_eq!(
            (
                state.mutation_preparations,
                state.installs,
                state.handoffs,
                state.starts
            ),
            (0, 0, 0, 0)
        );
    }
    let receipt = stored_supervisor_report(temp.path());
    assert_eq!(receipt["status"], "stale_registration");
    assert_eq!(receipt["registration_verified"], false);
    assert_eq!(receipt["live_owner_verified"], false);
    assert!(receipt["owner_pid"].is_null());
    assert!(receipt["recovery_reason"]
        .as_str()
        .is_some_and(|reason| reason.contains("preserved_unverified_live_owner")));
    Ok(())
}

#[test]
fn local_steady_state_check_detects_drift_without_spawning_manager_subprocesses() -> Result<()> {
    struct ManagedTestHost(PathBuf);

    impl DaemonApplicationHost for ManagedTestHost {
        fn hosted_uninstall_active(&self) -> Result<bool> {
            TestHost.hosted_uninstall_active()
        }
        fn hosted_uninstall_active_for_executable(&self, executable: &Path) -> Result<bool> {
            TestHost.hosted_uninstall_active_for_executable(executable)
        }
        fn managed_install_executable(&self) -> Result<Option<PathBuf>> {
            Ok(Some(self.0.clone()))
        }
        fn installation_upgrade_active(&self) -> Result<bool> {
            TestHost.installation_upgrade_active()
        }
        fn automatic_upgrade_recovery_allowed(&self, data_root: &Path) -> Result<bool> {
            TestHost.automatic_upgrade_recovery_allowed(data_root)
        }
        fn daemon_config(&self, data_root: &Path) -> Result<crate::DaemonConfigSnapshot> {
            TestHost.daemon_config(data_root)
        }
        fn observe_daemon_config(&self, data_root: &Path) -> Result<crate::DaemonConfigSnapshot> {
            TestHost.observe_daemon_config(data_root)
        }
        fn persisted_daemon_enabled(&self, data_root: &Path) -> Result<bool> {
            TestHost.persisted_daemon_enabled(data_root)
        }
        fn defer_restart_for_upgrade_handoff(
            &self,
            data_root: &Path,
            trigger: crate::DaemonTrigger,
        ) -> Result<Option<ctx_daemon_runtime::DaemonHandoffRestartDeferral>> {
            TestHost.defer_restart_for_upgrade_handoff(data_root, trigger)
        }
        fn request_lifecycle_wakeup(
            &self,
            data_root: &Path,
            request: Value,
            timeout: Duration,
            response_limit: u64,
        ) -> Result<Option<Value>> {
            TestHost.request_lifecycle_wakeup(data_root, request, timeout, response_limit)
        }
        fn home_dir(&self) -> Option<PathBuf> {
            TestHost.home_dir()
        }
        fn run_daemon_service(
            &self,
            data_root: &Path,
            request: crate::DaemonHostRunRequest,
        ) -> Result<()> {
            TestHost.run_daemon_service(data_root, request)
        }
        fn set_daemon_enabled(&self, data_root: &Path, enabled: bool) -> Result<()> {
            TestHost.set_daemon_enabled(data_root, enabled)
        }
        fn request_daemon_shutdown(
            &self,
            data_root: &Path,
            timeout: Duration,
            response_limit: u64,
        ) -> Result<()> {
            TestHost.request_daemon_shutdown(data_root, timeout, response_limit)
        }
        fn terminate_current_executable_daemon(&self, data_root: &Path) -> Result<()> {
            TestHost.terminate_current_executable_daemon(data_root)
        }
        fn remove_released_daemon_service_artifacts(&self, data_root: &Path) -> Result<()> {
            TestHost.remove_released_daemon_service_artifacts(data_root)
        }
        fn cancel_core_finalization_generation_lease(
            &self,
            data_root: &Path,
            reason: &str,
        ) -> Result<()> {
            TestHost.cancel_core_finalization_generation_lease(data_root, reason)
        }
        fn observe_source_refresh_endpoint(
            &self,
            identity_path: &Path,
        ) -> crate::DaemonEndpointObservation {
            TestHost.observe_source_refresh_endpoint(identity_path)
        }
        fn deliver_daemon_events(
            &self,
            data_root: &Path,
            events: &[ctx_client_observability::analytics::PublicEventV1],
        ) {
            TestHost.deliver_daemon_events(data_root, events);
        }
    }

    let _env_lock = crate::test_environment_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let _restore = RestoreTestEnvironment::capture(&["CTX_DATA_ROOT", "CODEX_HOME", "LANG", "TZ"]);
    let temp = tempfile::tempdir()?;
    env::set_var("CTX_DATA_ROOT", temp.path());
    env::remove_var("CODEX_HOME");
    env::remove_var("LANG");
    env::remove_var("TZ");

    let config = TestHost.daemon_config(temp.path())?;
    let _lock = ctx_daemon_runtime::DaemonLock::acquire(temp.path())?.expect("daemon lock");
    let executable = temp.path().join("ctx");
    let host = ManagedTestHost(executable.clone());

    // Missing receipt -> needs reconciliation.
    assert!(!supervisor_matches_live_owner_without_drift(
        &host,
        temp.path(),
        &config
    )?);

    let snapshot = configured_supervisor_environment(&host, temp.path(), None)?;
    let spec = install_environment_fixture(temp.path(), &snapshot)?;
    let artifact = spec.identity().artifact_path().to_path_buf();
    fs::write(&artifact, b"installed native service")?;
    write_installed_receipt(
        temp.path(),
        &executable,
        Some(artifact.clone()),
        std::process::id(),
        Some(snapshot.contract_report()),
    )?;

    // Exact match -> fast path.
    assert!(supervisor_matches_live_owner_without_drift(
        &host,
        temp.path(),
        &config
    )?);

    // Caller process-context drift only -> still fast path.
    env::set_var("LANG", "C.UTF-8");
    env::set_var("TZ", "America/Chicago");
    assert!(supervisor_matches_live_owner_without_drift(
        &host,
        temp.path(),
        &config
    )?);

    // Policy/provider drift -> requires reconciliation.
    env::set_var("CODEX_HOME", "/other/codex-home");
    assert!(!supervisor_matches_live_owner_without_drift(
        &host,
        temp.path(),
        &config
    )?);
    env::remove_var("CODEX_HOME");
    assert!(supervisor_matches_live_owner_without_drift(
        &host,
        temp.path(),
        &config
    )?);

    // Missing artifact file -> requires reconciliation.
    fs::remove_file(&artifact)?;
    assert!(!supervisor_matches_live_owner_without_drift(
        &host,
        temp.path(),
        &config
    )?);
    fs::write(&artifact, b"installed native service")?;

    // Owner PID mismatch -> requires reconciliation.
    write_installed_receipt(
        temp.path(),
        &executable,
        Some(artifact),
        std::process::id().saturating_add(1),
        Some(snapshot.contract_report()),
    )?;
    assert!(!supervisor_matches_live_owner_without_drift(
        &host,
        temp.path(),
        &config
    )?);
    Ok(())
}
