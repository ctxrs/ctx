mod support;

#[cfg(all(
    unix,
    any(
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64"),
            target_env = "gnu"
        ),
        all(
            target_os = "macos",
            any(target_arch = "x86_64", target_arch = "aarch64")
        ),
        all(target_os = "freebsd", target_arch = "x86_64")
    )
))]
mod unix {
    use std::{
        fs,
        path::{Path, PathBuf},
        process::{Child, Command, Stdio},
        sync::{Mutex, MutexGuard},
        time::{Duration, Instant},
    };

    use serde_json::Value;

    use super::support::*;

    // These contracts deliberately exercise the production five-second handoff
    // and recovery budget. Parallel daemon/setup fixtures can exhaust that
    // budget through test-only host contention and legitimately trigger a PID
    // replacement, invalidating the same-owner assertions below.
    static DAEMON_CONFIG_RELOAD_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn serial_daemon_test() -> MutexGuard<'static, ()> {
        DAEMON_CONFIG_RELOAD_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    struct DaemonGuard {
        child: Option<Child>,
    }

    impl DaemonGuard {
        fn pid(&self) -> u32 {
            self.child.as_ref().expect("running daemon child").id()
        }

        fn assert_running(&mut self) {
            assert!(
                self.child
                    .as_mut()
                    .expect("running daemon child")
                    .try_wait()
                    .unwrap()
                    .is_none(),
                "daemon exited unexpectedly"
            );
        }

        fn wait_for_exit(&mut self) -> std::process::ExitStatus {
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                if let Some(status) = self
                    .child
                    .as_mut()
                    .expect("running daemon child")
                    .try_wait()
                    .unwrap()
                {
                    return status;
                }
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for daemon exit"
                );
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    }

    impl Drop for DaemonGuard {
        fn drop(&mut self) {
            if let Err(error) =
                terminate_and_reap_test_child(&mut self.child, "config-reload daemon")
            {
                if std::thread::panicking() {
                    eprintln!("config-reload daemon teardown also failed: {error}");
                } else {
                    panic!("config-reload daemon teardown failed: {error}");
                }
            }
        }
    }

    fn write_config(temp: &tempfile::TempDir, semantic: bool) {
        write_mode_config(temp, "full", semantic);
    }

    fn write_mode_config(temp: &tempfile::TempDir, daemon_mode: &str, semantic: bool) {
        let root = data_root(temp);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("config.toml"),
            format!(
                "[analytics]\nenabled = false\n\n[upgrade]\nauto = \"off\"\n\n[daemon]\nenabled = true\nmode = \"{daemon_mode}\"\n\n[search]\nsemantic = {semantic}\n"
            ),
        )
        .unwrap();
    }

    fn write_config_with_retired_upgrade_control(
        temp: &tempfile::TempDir,
        daemon_mode: &str,
        semantic: bool,
    ) {
        let root = data_root(temp);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("config.toml"),
            format!(
                "[analytics]\nenabled = false\n\n[upgrade]\nauto = \"off\"\nallow_rfc2544_fake_ip = true\n\n[daemon]\nenabled = true\nmode = \"{daemon_mode}\"\n\n[search]\nsemantic = {semantic}\n"
            ),
        )
        .unwrap();
    }

    fn initialize_store(temp: &tempfile::TempDir, binary: &Path) {
        fs::create_dir_all(temp.path().join(".codex/sessions")).unwrap();
        let mut command = std_ctx_from_binary(temp, binary);
        // This fixture owns the daemon child directly so it can assert PID-stable
        // live reload. Suppress only this initialization spawn, including for
        // semantic-enabled configurations that intentionally reject --no-daemon.
        command
            .args(["setup", "--catalog-only", "--progress", "none"])
            .env("CTX_DAEMON_AUTOSTART_OFF", "1");
        let output = command
            .output()
            .unwrap_or_else(|error| panic!("run store initialization: {error}"));
        assert!(
            output.status.success(),
            "store initialization failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn write_nonsemantic_codex_session(temp: &tempfile::TempDir) {
        let sessions = temp.path().join(".codex/sessions/2026/08/02");
        fs::create_dir_all(&sessions).unwrap();
        fs::write(
            sessions.join("daemon-config-reload.jsonl"),
            concat!(
                r#"{"timestamp":"2026-08-02T12:00:00.000Z","type":"session_meta","payload":{"id":"daemon-config-reload","timestamp":"2026-08-02T12:00:00.000Z","cwd":"/repo/daemon-config-reload","originator":"codex-cli","cli_version":"0.200.0","source":"cli","model_provider":"openai"}}"#,
                "\n",
                r#"{"timestamp":"2026-08-02T12:00:01.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"daemon config reload source publication oracle"}]}}"#,
                "\n"
            ),
        )
        .unwrap();
    }

    fn std_ctx_from_binary(temp: &tempfile::TempDir, binary: &Path) -> Command {
        let prepared = ctx_from_binary(temp, binary);
        let mut command = Command::new(prepared.get_program());
        for (name, value) in prepared.get_envs() {
            match value {
                Some(value) => {
                    command.env(name, value);
                }
                None => {
                    command.env_remove(name);
                }
            }
        }
        command
    }

    fn spawn_daemon(
        temp: &tempfile::TempDir,
        binary: &Path,
        loop_interval_seconds: u64,
    ) -> DaemonGuard {
        let mut command = std_ctx_from_binary(temp, binary);
        let child = command
            .args(["daemon", "run"])
            .arg("--loop-interval-seconds")
            .arg(loop_interval_seconds.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        DaemonGuard { child: Some(child) }
    }

    fn read_json(path: PathBuf) -> Option<Value> {
        fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    }

    fn daemon_lifecycle(temp: &tempfile::TempDir) -> Option<Value> {
        read_json(data_root(temp).join("daemon/status.json"))
    }

    fn semantic_job(temp: &tempfile::TempDir) -> Option<Value> {
        read_json(data_root(temp).join("daemon/jobs/semantic-index.json"))
    }

    fn core_refresh_job(temp: &tempfile::TempDir) -> Option<Value> {
        read_json(data_root(temp).join("daemon/jobs/core-refresh.json"))
    }

    fn wait_for(description: &str, mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !condition() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {description}"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn wait_for_disabled_cycle(temp: &tempfile::TempDir, pid: u32) {
        wait_for("semantic-disabled daemon cycle", || {
            let Some(lifecycle) = daemon_lifecycle(temp) else {
                return false;
            };
            lifecycle["status"] == "running"
                && lifecycle["pid"] == pid
                && lifecycle["semantic_runtime_active"] == false
                && lifecycle["config_reload"]["status"] == "applied"
                && lifecycle["config_reload"]["applied"]["semantic_enabled"] == false
        });
    }

    fn wait_for_active_cycle(temp: &tempfile::TempDir, pid: u32) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let lifecycle = daemon_lifecycle(temp);
            let semantic = semantic_job(temp);
            if lifecycle.as_ref().is_some_and(|lifecycle| {
                lifecycle["status"] == "running"
                    && lifecycle["pid"] == pid
                    && lifecycle["semantic_runtime_active"] == true
                    && lifecycle["config_reload"]["status"] == "applied"
                    && lifecycle["config_reload"]["applied"]["semantic_enabled"] == true
                    && semantic
                        .as_ref()
                        .is_some_and(|job| job["status"] == "ready")
            }) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for semantic-active daemon cycle; lifecycle={lifecycle:#?}; semantic_job={semantic:#?}; core_refresh_job={:#?}",
                core_refresh_job(temp)
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn run_supported_setup(temp: &tempfile::TempDir, binary: &Path) -> Value {
        let mut command = ctx_from_binary(temp, binary);
        command
            .args(["setup", "--format=json", "--progress", "none"])
            .env_remove("CTX_DAEMON_AUTOSTART_OFF");
        json_output(&mut command)
    }

    fn daemon_status(temp: &tempfile::TempDir, binary: &Path) -> Value {
        let mut command = ctx_from_binary(temp, binary);
        command.args(["daemon", "status", "--format=json"]);
        json_output(&mut command)["daemon"].clone()
    }

    #[test]
    fn retired_upgrade_control_migrates_on_startup_and_live_reload_without_restart() {
        let _serial = serial_daemon_test();
        let temp = tempdir();
        let binary = copied_ctx_binary(&temp);
        write_config(&temp, false);
        initialize_store(&temp, &binary);

        write_config_with_retired_upgrade_control(&temp, "full", false);
        let mut daemon = spawn_daemon(&temp, &binary, 1);
        let original_pid = daemon.pid();
        wait_for_disabled_cycle(&temp, original_pid);
        let config_path = data_root(&temp).join("config.toml");
        wait_for("startup config migration", || {
            fs::read_to_string(&config_path)
                .is_ok_and(|text| !text.contains("allow_rfc2544_fake_ip"))
        });

        write_config_with_retired_upgrade_control(&temp, "source-refresh-only", false);
        wait_for("live config migration and mode reload", || {
            daemon_lifecycle(&temp).is_some_and(|lifecycle| {
                lifecycle["pid"] == original_pid
                    && lifecycle["config_reload"]["status"] == "applied"
                    && lifecycle["config_reload"]["applied"]["daemon_mode"] == "source-refresh-only"
                    && fs::read_to_string(&config_path)
                        .is_ok_and(|text| !text.contains("allow_rfc2544_fake_ip"))
            })
        });
        let status = daemon_status(&temp, &binary);
        assert_eq!(status["pid"], original_pid);
        assert_eq!(status["config_reload"]["status"], "applied");
        assert_eq!(status["config_reload"]["out_of_sync"], false);
        let migrated = fs::read(&config_path).unwrap();
        std::thread::sleep(Duration::from_secs(2));
        assert_eq!(fs::read(&config_path).unwrap(), migrated);
        daemon.assert_running();
    }

    #[test]
    fn semantic_opt_in_live_activates_the_existing_daemon() {
        let _serial = serial_daemon_test();
        let temp = tempdir();
        let binary = copied_ctx_binary(&temp);
        write_config(&temp, false);
        initialize_store(&temp, &binary);
        let mut daemon = spawn_daemon(&temp, &binary, 1);
        let original_pid = daemon.pid();
        wait_for_disabled_cycle(&temp, original_pid);

        write_config(&temp, true);
        let setup = run_supported_setup(&temp, &binary);
        assert_eq!(setup["schema_version"], 2);
        assert_eq!(setup["daemon_autostart"]["status"], "degraded");
        assert_eq!(setup["daemon_autostart"]["pid"], original_pid);

        wait_for("live semantic daemon activation", || {
            let Some(lifecycle) = daemon_lifecycle(&temp) else {
                return false;
            };
            lifecycle["pid"] == original_pid
                && lifecycle["semantic_runtime_active"] == true
                && lifecycle["config_reload"]["status"] == "applied"
                && lifecycle["config_reload"]["applied"]["semantic_enabled"] == true
                && data_root(&temp).join("daemon/query-endpoint.json").exists()
                && semantic_job(&temp).is_some_and(|job| job["status"] == "ready")
        });
        daemon.assert_running();

        let status = daemon_status(&temp, &binary);
        assert_eq!(status["pid"], original_pid);
        assert_eq!(status["running"], true);
        assert_eq!(status["semantic_runtime_active"], true);
        assert_eq!(status["config_reload"]["status"], "applied");
        assert_eq!(status["config_reload"]["out_of_sync"], false);
        assert_eq!(status["jobs"]["semantic_index"]["enabled"], true);
        assert_eq!(status["jobs"]["semantic_index"]["runtime_active"], true);
        assert_eq!(
            status["jobs"]["semantic_index"]["config_reload_status"],
            "applied"
        );

        write_config(&temp, false);
        let _setup = run_supported_setup(&temp, &binary);
        wait_for("live semantic daemon deactivation", || {
            daemon_lifecycle(&temp).is_some_and(|lifecycle| {
                lifecycle["pid"] == original_pid
                    && lifecycle["semantic_runtime_active"] == false
                    && lifecycle["config_reload"]["status"] == "applied"
                    && lifecycle["config_reload"]["applied"]["semantic_enabled"] == false
                    && !data_root(&temp).join("daemon/query-endpoint.json").exists()
            })
        });
        daemon.assert_running();
        let status = daemon_status(&temp, &binary);
        assert_eq!(status["pid"], original_pid);
        assert_eq!(status["jobs"]["semantic_index"]["enabled"], false);
        assert_eq!(status["jobs"]["semantic_index"]["status"], "disabled");
        assert_eq!(
            status["jobs"]["semantic_index"]["reason"],
            "semantic_disabled"
        );
    }

    #[test]
    fn daemon_mode_switch_updates_query_endpoint_before_setup_handoff_returns() {
        let _serial = serial_daemon_test();
        let temp = tempdir();
        let binary = copied_ctx_binary(&temp);
        write_mode_config(&temp, "source-refresh-only", true);
        initialize_store(&temp, &binary);
        let mut daemon = spawn_daemon(&temp, &binary, 1);
        let original_pid = daemon.pid();

        wait_for("source-refresh-only daemon cycle", || {
            daemon_lifecycle(&temp).is_some_and(|lifecycle| {
                lifecycle["status"] == "running"
                    && lifecycle["pid"] == original_pid
                    && lifecycle["semantic_runtime_active"] == false
                    && lifecycle["config_reload"]["status"] == "applied"
                    && lifecycle["config_reload"]["applied"]["daemon_mode"] == "source-refresh-only"
                    && lifecycle["config_reload"]["applied"]["semantic_enabled"] == true
                    && !data_root(&temp).join("daemon/query-endpoint.json").exists()
            })
        });

        write_mode_config(&temp, "full", true);
        let _setup = run_supported_setup(&temp, &binary);
        let full = daemon_lifecycle(&temp).expect("full-mode lifecycle");
        assert_eq!(full["pid"], original_pid);
        assert_eq!(full["semantic_runtime_active"], true);
        assert_eq!(full["config_reload"]["status"], "applied");
        assert_eq!(full["config_reload"]["applied"]["daemon_mode"], "full");
        assert_eq!(full["config_reload"]["applied"]["semantic_enabled"], true);
        assert!(
            data_root(&temp)
                .join("daemon/query-endpoint.json")
                .is_file(),
            "full-mode setup returned before the query endpoint was live"
        );
        daemon.assert_running();

        write_mode_config(&temp, "source-refresh-only", true);
        let _setup = run_supported_setup(&temp, &binary);
        let source_only = daemon_lifecycle(&temp).expect("source-refresh-only lifecycle");
        assert_eq!(source_only["pid"], original_pid);
        assert_eq!(source_only["semantic_runtime_active"], false);
        assert_eq!(source_only["config_reload"]["status"], "applied");
        assert_eq!(
            source_only["config_reload"]["applied"]["daemon_mode"],
            "source-refresh-only"
        );
        assert_eq!(
            source_only["config_reload"]["applied"]["semantic_enabled"],
            true
        );
        assert!(
            !data_root(&temp).join("daemon/query-endpoint.json").exists(),
            "source-refresh-only setup returned before the query endpoint was removed"
        );
        daemon.assert_running();
    }

    #[test]
    fn setup_handoff_observes_event_driven_live_activation() {
        let _serial = serial_daemon_test();
        let temp = tempdir();
        let binary = copied_ctx_binary(&temp);
        write_config(&temp, false);
        initialize_store(&temp, &binary);
        let daemon = spawn_daemon(&temp, &binary, 120);
        wait_for_disabled_cycle(&temp, daemon.pid());

        write_config(&temp, true);
        let setup = run_supported_setup(&temp, &binary);
        assert_eq!(setup["daemon_autostart"]["status"], "degraded");
        assert_eq!(setup["daemon_autostart"]["pid"], daemon.pid());
        let status = daemon_status(&temp, &binary);

        assert_eq!(status["running"], true);
        assert_eq!(status["semantic_runtime_active"], true);
        assert_eq!(status["config_reload"]["status"], "applied");
        assert_eq!(status["config_reload"]["out_of_sync"], false);
        assert_eq!(status["config_reload"]["applied"]["semantic_enabled"], true);
        assert_eq!(
            status["config_reload"]["requested"]["semantic_enabled"],
            true
        );
        assert_eq!(status["jobs"]["semantic_index"]["enabled"], true);
        assert_eq!(status["jobs"]["semantic_index"]["runtime_active"], true);
        assert!(data_root(&temp)
            .join("daemon/query-endpoint.json")
            .is_file());
    }

    #[test]
    fn malformed_reload_is_failed_and_retried_without_changing_live_state() {
        let _serial = serial_daemon_test();
        let temp = tempdir();
        let binary = copied_ctx_binary(&temp);
        write_config(&temp, false);
        initialize_store(&temp, &binary);
        let mut daemon = spawn_daemon(&temp, &binary, 1);
        wait_for_disabled_cycle(&temp, daemon.pid());

        fs::write(
            data_root(&temp).join("config.toml"),
            "[search\nsemantic = true\n",
        )
        .unwrap();
        wait_for("failed config reload", || {
            daemon_lifecycle(&temp).is_some_and(|status| {
                status["config_reload"]["status"] == "failed"
                    && status["config_reload"]["applied"]["semantic_enabled"] == false
                    && status["semantic_runtime_active"] == false
                    && status["config_reload"]["last_error"]
                        .as_str()
                        .is_some_and(|error| error.contains("parse"))
            })
        });
        daemon.assert_running();
        assert!(!data_root(&temp).join("daemon/query-endpoint.json").exists());

        let status = daemon_status(&temp, &binary);
        assert_eq!(status["config_reload"]["status"], "failed");
        assert_eq!(status["config_reload"]["out_of_sync"], true);
        assert!(status["config_reload"]["last_error"]
            .as_str()
            .is_some_and(|error| error.contains("parse")));
        assert_eq!(status["jobs"]["semantic_index"]["enabled"], false);
        assert_eq!(status["jobs"]["semantic_index"]["runtime_active"], false);
        assert_eq!(status["jobs"]["semantic_index"]["status"], "failed");
        assert_eq!(
            status["jobs"]["semantic_index"]["reason"],
            "daemon_config_reload_failed"
        );
        assert_eq!(
            status["jobs"]["semantic_index"]["config_reload_status"],
            "failed"
        );
        assert_eq!(
            status["jobs"]["semantic_index"]["configuration_pending"],
            false
        );

        let mut setup = ctx_from_binary(&temp, &binary);
        setup.args(["setup", "--catalog-only", "--progress", "none"]);
        let stderr = failure_stderr(&mut setup);
        assert!(
            stderr.contains("invalid config section header"),
            "ordinary setup unexpectedly accepted malformed config: {stderr}"
        );

        write_config(&temp, true);
        wait_for("reload recovery and semantic activation", || {
            daemon_lifecycle(&temp).is_some_and(|status| {
                status["config_reload"]["status"] == "applied"
                    && status["config_reload"]["applied"]["semantic_enabled"] == true
                    && status["semantic_runtime_active"] == true
            })
        });
        daemon.assert_running();
    }

    #[test]
    fn malformed_reload_deactivates_semantic_runtime_and_reports_failure() {
        let _serial = serial_daemon_test();
        let temp = tempdir();
        let binary = copied_ctx_binary(&temp);
        write_config(&temp, true);
        // Semantic scheduling follows a published Core generation. Keep this
        // fixture ineligible for embedding so the daemon proves a completed
        // semantic cycle without loading a model or weakening the required
        // `ready` receipt.
        write_nonsemantic_codex_session(&temp);
        initialize_store(&temp, &binary);
        let mut daemon = spawn_daemon(&temp, &binary, 1);
        wait_for_active_cycle(&temp, daemon.pid());
        let setup = run_supported_setup(&temp, &binary);
        assert_eq!(setup["daemon_autostart"]["pid"], daemon.pid());
        wait_for_active_cycle(&temp, daemon.pid());

        fs::write(
            data_root(&temp).join("config.toml"),
            "[search\nsemantic = false\n",
        )
        .unwrap();
        wait_for(
            "failed config reload with deactivated semantic runtime",
            || {
                daemon_lifecycle(&temp).is_some_and(|status| {
                    status["config_reload"]["status"] == "failed"
                        && status["config_reload"]["applied"]["semantic_enabled"] == false
                        && status["semantic_runtime_active"] == false
                        && status["config_reload"]["last_error"]
                            .as_str()
                            .is_some_and(|error| error.contains("parse"))
                })
            },
        );
        daemon.assert_running();

        let status = daemon_status(&temp, &binary);
        assert_eq!(status["config_reload"]["status"], "failed");
        assert_eq!(status["config_reload"]["out_of_sync"], true);
        assert!(status["config_reload"]["last_error"]
            .as_str()
            .is_some_and(|error| error.contains("parse")));
        assert_eq!(status["jobs"]["semantic_index"]["enabled"], false);
        assert_eq!(status["jobs"]["semantic_index"]["runtime_active"], false);
        assert_eq!(status["jobs"]["semantic_index"]["status"], "failed");
        assert_eq!(
            status["jobs"]["semantic_index"]["reason"],
            "daemon_config_reload_failed"
        );
        assert_eq!(status["jobs"]["semantic_index"]["last_run_status"], "ready");
        assert!(status["jobs"]["semantic_index"]["last_run_reason"].is_null());
        assert!(status["jobs"]["semantic_index"]["last_error"]
            .as_str()
            .is_some_and(|error| error.contains("parse")));
        assert_eq!(
            status["jobs"]["semantic_index"]["config_reload_status"],
            "failed"
        );
        assert_eq!(
            status["jobs"]["semantic_index"]["configuration_pending"],
            false
        );
    }

    #[test]
    fn semantic_activation_failure_never_reports_success() {
        let _serial = serial_daemon_test();
        let temp = tempdir();
        let binary = copied_ctx_binary(&temp);
        write_config(&temp, false);
        initialize_store(&temp, &binary);
        let mut daemon = spawn_daemon(&temp, &binary, 1);
        wait_for_disabled_cycle(&temp, daemon.pid());

        fs::create_dir(data_root(&temp).join("daemon/query-endpoint.json")).unwrap();
        write_config(&temp, true);
        let mut setup = ctx_from_binary(&temp, &binary);
        setup
            .args(["setup", "--format=json", "--progress", "none"])
            .env_remove("CTX_DAEMON_AUTOSTART_OFF");
        let setup_error = failure_stderr(&mut setup);
        assert!(
            setup_error.contains("ctx daemon did not become ready")
                && setup_error.contains("query-endpoint.json"),
            "{setup_error}"
        );
        wait_for("failed semantic runtime activation", || {
            daemon_lifecycle(&temp).is_some_and(|status| {
                status["config_reload"]["status"] == "activation_failed"
                    && status["semantic_runtime_active"] == false
            })
        });
        daemon.assert_running();

        let status = daemon_status(&temp, &binary);
        assert_eq!(status["running"], true);
        assert_eq!(status["semantic_runtime_active"], false);
        assert_eq!(status["config_reload"]["status"], "activation_failed");
        assert_eq!(status["config_reload"]["out_of_sync"], true);
        assert!(status["config_reload"]["last_error"]
            .as_str()
            .is_some_and(|error| error.contains("query-endpoint.json")));
        assert_eq!(
            status["config_reload"]["requested"]["semantic_enabled"],
            true
        );
        assert_eq!(
            status["config_reload"]["requested"]["semantic_executor"],
            "builtin"
        );
        assert_eq!(
            status["config_reload"]["applied"]["semantic_enabled"],
            false
        );
        assert!(status["config_reload"]["applied"]["semantic_executor"].is_null());
        assert_eq!(status["jobs"]["semantic_index"]["enabled"], false);
        assert_eq!(status["jobs"]["semantic_index"]["semantic_requested"], true);
        assert_eq!(status["jobs"]["semantic_index"]["semantic_enabled"], false);
        assert_eq!(status["jobs"]["semantic_index"]["runtime_active"], false);
        assert_eq!(status["jobs"]["semantic_index"]["status"], "failed");
        assert_eq!(
            status["jobs"]["semantic_index"]["reason"],
            "semantic_activation_failed"
        );
        assert!(
            data_root(&temp).join("daemon/query-endpoint.json").is_dir(),
            "the endpoint path should remain the blocking test fixture"
        );

        fs::remove_dir(data_root(&temp).join("daemon/query-endpoint.json")).unwrap();
        wait_for("semantic activation recovery", || {
            daemon_lifecycle(&temp).is_some_and(|status| {
                status["config_reload"]["status"] == "applied"
                    && status["config_reload"]["applied"]["semantic_enabled"] == true
                    && status["semantic_runtime_active"] == true
            })
        });
        let recovered = daemon_status(&temp, &binary);
        assert_eq!(recovered["jobs"]["semantic_index"]["enabled"], true);
        assert_eq!(
            recovered["jobs"]["semantic_index"]["semantic_requested"],
            true
        );
        assert_eq!(
            recovered["jobs"]["semantic_index"]["semantic_enabled"],
            true
        );
        assert_eq!(recovered["jobs"]["semantic_index"]["runtime_active"], true);
    }

    #[test]
    fn historical_activation_failure_yields_to_current_manual_policy() {
        let _serial = serial_daemon_test();
        let temp = tempdir();
        let binary = copied_ctx_binary(&temp);
        let root = data_root(&temp);
        fs::create_dir_all(root.join("daemon")).unwrap();
        fs::write(
            root.join("daemon/status.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 1,
                "status": "failed",
                "semantic_runtime_active": false,
                "config_reload": {
                    "status": "activation_failed",
                    "out_of_sync": true,
                    "applied": {
                        "daemon_enabled": true,
                        "daemon_mode": "full",
                        "semantic_enabled": true
                    },
                    "last_error": "historical semantic activation failure"
                }
            }))
            .unwrap(),
        )
        .unwrap();

        for (configured, environment) in [
            ("manual", None),
            ("automatic", Some(("CTX_DAEMON_ENABLED", "false"))),
            ("automatic", Some(("CTX_DAEMON_OFF", "1"))),
        ] {
            fs::write(
                root.join("config.toml"),
                format!(
                    "[analytics]\nenabled = false\n\n[upgrade]\nauto = \"off\"\n\n[indexing]\nmode = \"{configured}\"\n\n[daemon]\nmode = \"full\"\n\n[search]\nsemantic = true\n"
                ),
            )
            .unwrap();
            let mut command = ctx_from_binary(&temp, &binary);
            command.args(["daemon", "status", "--format=json"]);
            if let Some((name, value)) = environment {
                command.env(name, value);
            }
            let status = json_output(&mut command)["daemon"].clone();

            assert_eq!(status["config_reload"]["status"], "activation_failed");
            assert_eq!(
                status["config_reload"]["last_error"],
                "historical semantic activation failure"
            );
            assert_eq!(status["jobs"]["semantic_index"]["enabled"], false);
            assert_eq!(status["jobs"]["semantic_index"]["status"], "disabled");
            assert_eq!(
                status["jobs"]["semantic_index"]["reason"], "daemon_disabled",
                "configured={configured} environment={environment:?}: {status:#}"
            );
            assert_eq!(
                status["jobs"]["semantic_index"]["config_reload_status"],
                "activation_failed"
            );
        }
    }

    #[test]
    fn initial_semantic_activation_failure_fails_daemon_startup_truthfully() {
        let _serial = serial_daemon_test();
        let temp = tempdir();
        let binary = copied_ctx_binary(&temp);
        write_config(&temp, true);
        initialize_store(&temp, &binary);
        fs::create_dir_all(data_root(&temp).join("daemon/query-endpoint.json")).unwrap();

        let mut daemon = spawn_daemon(&temp, &binary, 1);
        assert!(!daemon.wait_for_exit().success());

        let status = daemon_status(&temp, &binary);
        assert_eq!(status["running"], false);
        assert_eq!(status["status"], "failed");
        assert_eq!(status["semantic_runtime_active"], false);
        assert_eq!(status["config_reload"]["status"], "activation_failed");
        assert_eq!(
            status["config_reload"]["requested"]["semantic_enabled"],
            true
        );
        assert_eq!(
            status["config_reload"]["applied"]["semantic_enabled"],
            false
        );
        assert_eq!(status["jobs"]["semantic_index"]["enabled"], false);
        assert_eq!(status["jobs"]["semantic_index"]["semantic_requested"], true);
        assert_eq!(status["jobs"]["semantic_index"]["semantic_enabled"], false);
        assert_eq!(status["jobs"]["semantic_index"]["status"], "failed");
        assert_eq!(
            status["jobs"]["semantic_index"]["reason"],
            "semantic_activation_failed"
        );
    }
}
