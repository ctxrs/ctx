mod support;

#[cfg(all(unix, ctx_semantic_fastembed))]
mod unix {
    use std::{
        fs,
        path::{Path, PathBuf},
        process::{Child, Command, Stdio},
        time::{Duration, Instant},
    };

    use serde_json::Value;

    use super::support::*;

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
            .args(["daemon", "run", "--idle-exit-seconds", "600"])
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
    fn semantic_opt_in_live_activates_the_existing_daemon() {
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
        assert_eq!(status["jobs"]["semantic_index"]["status"], "disabled");
        assert_eq!(
            status["jobs"]["semantic_index"]["reason"],
            "semantic_disabled"
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
    fn malformed_reload_preserves_active_semantic_job_status() {
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
            "failed config reload with retained semantic runtime",
            || {
                daemon_lifecycle(&temp).is_some_and(|status| {
                    status["config_reload"]["status"] == "failed"
                        && status["config_reload"]["applied"]["semantic_enabled"] == true
                        && status["semantic_runtime_active"] == true
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
        assert_eq!(status["jobs"]["semantic_index"]["enabled"], true);
        assert_eq!(status["jobs"]["semantic_index"]["runtime_active"], true);
        assert_eq!(status["jobs"]["semantic_index"]["status"], "ready");
        assert!(status["jobs"]["semantic_index"]["reason"].is_null());
        assert_eq!(status["jobs"]["semantic_index"]["last_run_status"], "ready");
        assert!(status["jobs"]["semantic_index"]["last_run_reason"].is_null());
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
        assert_eq!(status["jobs"]["semantic_index"]["enabled"], true);
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
    }

    #[test]
    fn initial_semantic_activation_failure_fails_daemon_startup_truthfully() {
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
        assert_eq!(status["jobs"]["semantic_index"]["enabled"], true);
        assert_eq!(status["jobs"]["semantic_index"]["status"], "failed");
        assert_eq!(
            status["jobs"]["semantic_index"]["reason"],
            "semantic_activation_failed"
        );
    }
}
