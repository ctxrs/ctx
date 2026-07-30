use super::*;
use std::{
    env,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Barrier, Mutex,
    },
};

const SUPERVISOR_ENV_ARTIFACT_PROBE_STAGE: &str = "CTX_SUPERVISOR_ENV_ARTIFACT_PROBE_STAGE";
const SUPERVISOR_ENV_ARTIFACT_PROBE_TEST: &str =
    "semantic::daemon_supervisor::tests::native_supervisor_artifacts_exclude_authority_and_fail_closed_on_controls";

#[derive(Default)]
struct FakeSupervisorState {
    registered: bool,
    live_owner: Option<u32>,
    installs: usize,
    disables: usize,
    starts: usize,
    upgrade_fence_released: bool,
    start_observed_released_fence: bool,
}

#[derive(Default)]
struct FakeSupervisorBackend {
    state: Mutex<FakeSupervisorState>,
    delay_install: bool,
    fail_install_after_registration: bool,
}

impl FakeSupervisorBackend {
    fn with_registration(live_owner: Option<u32>) -> Self {
        Self {
            state: Mutex::new(FakeSupervisorState {
                registered: true,
                live_owner,
                ..FakeSupervisorState::default()
            }),
            delay_install: false,
            fail_install_after_registration: false,
        }
    }
}

impl NativeSupervisorBackend for FakeSupervisorBackend {
    fn artifact_path(&self, data_root: &Path) -> Result<Option<PathBuf>> {
        Ok(Some(data_root.join("fake-native-registration")))
    }

    fn install(
        &self,
        data_root: &Path,
        _executable: &Path,
        _environment: &SupervisorEnvironmentSnapshot,
    ) -> Result<PathBuf> {
        {
            let mut state = self.state.lock().unwrap();
            state.installs += 1;
        }
        if self.delay_install {
            std::thread::sleep(Duration::from_millis(100));
        }
        let mut state = self.state.lock().unwrap();
        state.registered = true;
        state.live_owner = Some(4_242);
        if self.fail_install_after_registration {
            return Err(anyhow!(
                "fake installer failed after publishing valid registration"
            ));
        }
        Ok(data_root.join("fake-native-registration"))
    }

    fn disable(&self, _data_root: &Path) -> Result<Option<PathBuf>> {
        let mut state = self.state.lock().unwrap();
        state.disables += 1;
        state.registered = false;
        state.live_owner = None;
        Ok(None)
    }

    fn verify_registration(&self, _data_root: &Path, _executable: &Path) -> Result<()> {
        self.state
            .lock()
            .unwrap()
            .registered
            .then_some(())
            .ok_or_else(|| anyhow!("fake native registration is absent"))
    }

    fn verify_live_owner(&self, _data_root: &Path, _executable: &Path) -> Result<u32> {
        self.state
            .lock()
            .unwrap()
            .live_owner
            .ok_or_else(|| anyhow!("fake native manager has no owner"))
    }

    fn start(&self, _data_root: &Path) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        state.starts += 1;
        state.start_observed_released_fence = state.upgrade_fence_released;
        state.live_owner = Some(4_242);
        Ok(())
    }
}

#[cfg(target_os = "linux")]
#[test]
fn systemd_unit_is_persistent_and_restart_on_failure() {
    let unit = linux_systemd_unit(
        Path::new("/home/user/.local/bin/ctx"),
        Path::new("/home/user/.local/share/ctx"),
    )
    .unwrap();
    assert!(unit.contains("Restart=on-failure"));
    assert!(unit.contains("WantedBy=default.target"));
    assert!(unit.contains("ExecStart=/usr/bin/env -i "));
    assert!(!unit.contains("CTX_RELEASE_"));
    assert!(!unit.contains("idle-exit-seconds"));
    assert!(!unit.contains("loop-interval-seconds"));
}

#[test]
fn systemd_registration_requires_a_nonzero_live_main_pid() {
    assert_eq!(systemd_main_pid(b"4242\n").unwrap(), 4242);
    assert!(systemd_main_pid(b"0\n").is_err());
    assert!(systemd_main_pid(b"\n").is_err());
}

#[test]
fn launch_agent_plist_is_persistent_sanitized_and_gui_registration_is_identity_bearing() {
    let plist = launch_agent_plist(
        Path::new("/Users/test/Library/Application Support/ctx/ctx"),
        Path::new("/Users/test/Library/Application Support/ctx/data"),
    )
    .unwrap();
    assert!(plist.contains("<key>Label</key><string>rs.ctx.daemon</string>"));
    assert!(plist.contains("<key>RunAtLoad</key><true/>"));
    assert!(plist.contains("<key>KeepAlive</key>"));
    assert!(plist.contains("<string>/usr/bin/env</string><string>-i</string>"));
    assert!(!plist.contains("CTX_RELEASE_"));
    assert!(!plist.contains("idle-exit-seconds"));
    assert_eq!(
        launchctl_print_pid("state = running\n\tpid = 73\n"),
        Some(73)
    );
    assert_eq!(launchctl_print_pid("state = waiting\n"), None);
}

#[test]
fn windows_task_contract_is_current_user_restartable_and_spawns_with_a_clear_environment() {
    let script = windows_sanitized_daemon_script(
        Path::new(r"C:\Program Files\ctx\ctx.exe"),
        Path::new(r"C:\Users\test\AppData\Local\ctx"),
    )
    .unwrap();
    assert!(script.contains("EnvironmentVariables.Clear()"));
    assert!(script.contains("UseShellExecute=$false"));
    assert!(!script.contains("CTX_RELEASE_"));
    assert!(!script.contains("idle-exit-seconds"));

    let xml = windows_task_xml(
        Path::new(r"C:\Program Files\ctx\ctx.exe"),
        Path::new(r"C:\Users\test\AppData\Local\ctx"),
        Path::new(r"C:\Windows"),
        "S-1-5-21-1000",
        r"\ctx-daemon-S-1-5-21-1000",
    )
    .unwrap();
    assert!(windows_task_registration_matches(
        &xml,
        Path::new(r"C:\Program Files\ctx\ctx.exe"),
        Path::new(r"C:\Users\test\AppData\Local\ctx"),
        Path::new(r"C:\Windows"),
        "S-1-5-21-1000",
        r"\ctx-daemon-S-1-5-21-1000",
    )
    .unwrap());
    assert!(xml.contains("<LogonTrigger>"));
    assert!(xml.contains("<UserId>S-1-5-21-1000</UserId>"));
    assert!(xml.contains("<RestartOnFailure>"));
    assert!(xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
    assert!(!windows_task_registration_matches(
        "<Task><LogonType>InteractiveToken</LogonType></Task>",
        Path::new(r"C:\Program Files\ctx\ctx.exe"),
        Path::new(r"C:\Users\test\AppData\Local\ctx"),
        Path::new(r"C:\Windows"),
        "S-1-5-21-1000",
        r"\ctx-daemon-S-1-5-21-1000",
    )
    .unwrap());
    assert_eq!(
        windows_task_name("S-1-5-21-1000"),
        r"\ctx-daemon-S-1-5-21-1000"
    );
    let state_script = windows_task_state_script(r"\ctx-daemon-S-1-5-21-1000");
    assert!(state_script.contains("-TaskPath '\\'"));
    assert!(state_script.contains("-TaskName 'ctx-daemon-S-1-5-21-1000'"));
    assert_eq!(parse_windows_task_state(b"4\r\n"), Some(4));
    assert_ne!(parse_windows_task_state(b"3\r\n"), Some(4));
}

#[test]
fn native_supervisor_artifacts_exclude_authority_and_fail_closed_on_controls() -> Result<()> {
    let forbidden = [
        "CTX_PRO_HELPER",
        "CTX_SEMANTIC_MODEL_ONNX",
        "CTX_RELEASE_CONFIGURED_AUTHORITY",
        "CTX_RELEASE_METADATA_URL",
        "CTX_RELEASE_METADATA_PUBLIC_KEY_PEM",
        "CTX_RELEASE_METADATA_SIGNATURE_URL",
        "CTX_RELEASE_PUBLIC_KEY",
        "CTX_RELEASE_SIGNATURE",
        "CTX_RELEASE_VERSION",
        "GITHUB_TOKEN",
    ];
    if env::var(SUPERVISOR_ENV_ARTIFACT_PROBE_STAGE).as_deref() != Ok("final") {
        let mut child = Command::new(env::current_exe()?);
        child
            .args(["--exact", SUPERVISOR_ENV_ARTIFACT_PROBE_TEST, "--nocapture"])
            .env(SUPERVISOR_ENV_ARTIFACT_PROBE_STAGE, "final");
        for name in forbidden {
            child.env(name, format!("secret-value-for-{name}"));
        }
        assert!(child.status()?.success());
        return Ok(());
    }

    let executable = Path::new("/opt/ctx/bin/ctx");
    let data_root = Path::new("/tmp/ctx-native-supervisor-environment");
    let systemd = linux_systemd_unit(executable, data_root)?;
    let launchd = launch_agent_plist(executable, data_root)?;
    let windows = windows_sanitized_daemon_script(executable, data_root)?;
    for name in forbidden {
        let value = format!("secret-value-for-{name}");
        for artifact in [&systemd, &launchd, &windows] {
            assert!(!artifact.contains(name), "{name} leaked into {artifact}");
            assert!(
                !artifact.contains(&value),
                "{name} value leaked into {artifact}"
            );
        }
    }

    env::set_var("CODEX_HOME", "line\nbreak");
    assert!(linux_systemd_unit(executable, data_root).is_err());
    assert!(launch_agent_plist(executable, data_root).is_err());
    assert!(windows_sanitized_daemon_script(executable, data_root).is_err());
    assert!(windows_task_xml(
        executable,
        data_root,
        Path::new(r"C:\Windows"),
        "S-1-5-21-1000",
        r"\ctx-daemon-S-1-5-21-1000",
    )
    .is_err());
    let hostile_root = Path::new("/tmp/ctx\ninjected-directive");
    assert!(linux_systemd_unit(executable, hostile_root).is_err());
    assert!(launch_agent_plist(executable, hostile_root).is_err());
    assert!(windows_task_xml(
        executable,
        hostile_root,
        Path::new(r"C:\Windows"),
        "S-1-5-21-1000",
        r"\ctx-daemon-S-1-5-21-1000",
    )
    .is_err());
    Ok(())
}

#[test]
fn windows_task_status_decoder_handles_task_scheduler_utf16_xml() {
    let source =
        r#"<Task><RegistrationInfo><URI>\ctx-daemon-S-1-5-21-1000</URI></RegistrationInfo></Task>"#;
    let mut encoded = vec![0xff, 0xfe];
    encoded.extend(source.encode_utf16().flat_map(u16::to_le_bytes));
    assert_eq!(decode_supervisor_text(&encoded), source);
}

#[test]
fn windows_command_line_quoting_preserves_spaces_quotes_and_trailing_separators() {
    assert_eq!(windows_command_line_quote("plain"), "plain");
    assert_eq!(windows_command_line_quote("two words"), "\"two words\"");
    assert_eq!(windows_command_line_quote(r#"C:\a b\"#), r#""C:\a b\\""#,);
}

#[test]
fn freebsd_limitation_names_the_missing_product_authority_without_claiming_support() {
    let limitation = freebsd_supervisor_authority_blocker();
    assert!(limitation.contains("no standard current-user service manager"));
    assert!(limitation.contains("will not mutate the user's crontab"));
    assert!(limitation.contains("typed CLI self-healing"));
}

#[test]
fn concurrent_recovery_revalidates_registration_under_the_installation_lock() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("ctx");
    let backend = Arc::new(FakeSupervisorBackend {
        delay_install: true,
        fail_install_after_registration: true,
        ..FakeSupervisorBackend::default()
    });
    let barrier = Arc::new(Barrier::new(3));
    let callers = (0..2)
        .map(|_| {
            let data_root = temp.path().to_path_buf();
            let executable = executable.clone();
            let backend = Arc::clone(&backend);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                ensure_native_supervisor_with(&data_root, &executable, backend.as_ref())
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    for caller in callers {
        assert_eq!(
            caller
                .join()
                .expect("join concurrent supervisor recovery")?,
            DaemonSupervisorStart::Native
        );
    }
    let state = backend.state.lock().unwrap();
    assert_eq!(state.installs, 1);
    assert_eq!(state.disables, 0);
    assert_eq!(state.starts, 0);
    assert!(state.registered);
    assert_eq!(state.live_owner, Some(4_242));
    drop(state);
    let receipt = stored_supervisor_report(temp.path());
    assert_eq!(receipt["status"], "installed");
    assert_eq!(receipt["registration_verified"], true);
    assert_eq!(receipt["live_owner_verified"], true);
    assert_eq!(receipt["owner_pid"], 4_242);
    assert_eq!(receipt["environment_snapshot"]["schema_version"], 1);
    assert!(receipt["environment_snapshot"]["captured_at_ms"]
        .as_i64()
        .is_some());
    assert!(receipt["environment_snapshot"]["sha256"]
        .as_str()
        .is_some_and(|value| value.len() == 64));
    assert_eq!(receipt["environment_snapshot"]["values_exposed"], false);
    assert!(receipt["environment_snapshot"].get("values").is_none());
    Ok(())
}

#[test]
fn upgrade_handoff_releases_fence_before_native_manager_start() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("ctx");
    let backend = FakeSupervisorBackend::with_registration(None);
    let result =
        resume_daemon_supervisor_after_upgrade_with(temp.path(), &executable, &backend, || {
            backend.state.lock().unwrap().upgrade_fence_released = true;
            Ok(())
        })?;
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
    let result =
        resume_daemon_supervisor_after_upgrade_with(temp.path(), &executable, &backend, || {
            fence_released.store(true, Ordering::SeqCst);
            Ok(())
        })?;
    assert_eq!(result, DaemonSupervisorUpgradeResume::Fallback);
    assert!(!fence_released.load(Ordering::SeqCst));
    assert_eq!(backend.state.lock().unwrap().starts, 0);
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
        Some(supervisor_environment_snapshot()?.contract_report()),
    )?;

    backend.state.lock().unwrap().live_owner = Some(7_331);
    let restarted = revalidated_supervisor_report_with(temp.path(), &backend);
    assert_eq!(restarted["status"], "installed");
    assert_eq!(restarted["registration_verified"], true);
    assert_eq!(restarted["live_owner_verified"], true);
    assert_eq!(restarted["owner_pid"], 7_331);

    backend.state.lock().unwrap().live_owner = None;
    let stopped = revalidated_supervisor_report_with(temp.path(), &backend);
    assert_eq!(stopped["status"], "registered_not_running");
    assert_eq!(stopped["registration_verified"], true);
    assert_eq!(stopped["live_owner_verified"], false);
    assert_eq!(stopped["owner_pid"], Value::Null);

    backend.state.lock().unwrap().registered = false;
    let stale = revalidated_supervisor_report_with(temp.path(), &backend);
    assert_eq!(stale["status"], "stale_registration");
    assert_eq!(stale["registration_verified"], false);
    assert_eq!(stale["live_owner_verified"], false);
    Ok(())
}

#[test]
fn status_preserves_installed_environment_hash_and_flags_current_mismatch() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let executable = temp.path().join("ctx");
    let backend = FakeSupervisorBackend::with_registration(Some(4_242));
    write_installed_receipt(
        temp.path(),
        &executable,
        backend.artifact_path(temp.path())?,
        4_242,
        Some(supervisor_environment_snapshot()?.contract_report()),
    )?;
    let receipt_path =
        super::super::paths_status::daemon_root_path(temp.path()).join("supervisor.json");
    let mut installed: Value = serde_json::from_slice(&fs::read(&receipt_path)?)?;
    installed["environment_snapshot"]["sha256"] = json!("0".repeat(64));
    installed["environment_snapshot"]["captured_at_ms"] = json!(1234);
    super::super::paths_status::write_private_json_file(&receipt_path, &installed)?;

    let report = revalidated_supervisor_report_with(temp.path(), &backend);
    assert_eq!(
        report["environment_snapshot"]["sha256"],
        "0".repeat(64),
        "status must retain the installed snapshot hash"
    );
    assert_eq!(report["environment_snapshot"]["captured_at_ms"], 1234);
    assert_eq!(report["environment_snapshot"]["restart_required"], true);
    assert_ne!(
        report["environment_snapshot"]["current_sha256"],
        report["environment_snapshot"]["sha256"]
    );
    assert_eq!(report["environment_snapshot"]["values_exposed"], false);
    Ok(())
}

#[test]
fn supervisor_report_states_forced_termination_identity_limitations() {
    let temp = tempfile::tempdir().unwrap();
    let report = daemon_supervisor_report(temp.path());
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
    let _lock = super::super::paths_status::DaemonLock::acquire(temp.path())
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
    disable_daemon_supervisor(temp.path()).unwrap();
    disable_daemon_supervisor(temp.path()).unwrap();
    let status = daemon_supervisor_report(temp.path());
    assert_eq!(status["status"], "disabled");
    assert_eq!(status["registration_verified"], false);
    assert_eq!(status["live_owner_verified"], false);
    assert_eq!(status["autostart_supported"], false);
    assert_eq!(status["restart_supported"], false);
}

#[test]
fn canonical_supervisor_root_is_independent_of_ctx_data_root_override() {
    static ENV_LOCK: Mutex<()> = Mutex::new(());
    let _guard = ENV_LOCK.lock().unwrap();
    let previous = env::var_os("CTX_DATA_ROOT");
    let canonical = ctx_history_core::managed_data_root().unwrap();
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
