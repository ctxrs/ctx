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
        child: Child,
    }

    impl DaemonGuard {
        fn pid(&self) -> u32 {
            self.child.id()
        }

        fn assert_running(&mut self) {
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "daemon exited unexpectedly"
            );
        }

        fn wait_for_exit(&mut self) -> std::process::ExitStatus {
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                if let Some(status) = self.child.try_wait().unwrap() {
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
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    fn write_config(root: &Path, semantic: bool) {
        fs::write(
            root.join("config.toml"),
            format!(
                "[analytics]\nenabled = false\n\n[upgrade]\nauto = \"off\"\n\n[daemon]\nenabled = true\n\n[search]\nsemantic = {semantic}\n"
            ),
        )
        .unwrap();
    }

    fn initialize_store(temp: &tempfile::TempDir, binary: &Path) {
        fs::create_dir_all(temp.path().join(".codex/sessions")).unwrap();
        let mut command = std_ctx_from_binary(temp, binary);
        command.args(["setup", "--catalog-only", "--progress", "none"]);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match command.output() {
                Ok(output) => {
                    assert!(
                        output.status.success(),
                        "store initialization failed: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                    break;
                }
                Err(error) if error.raw_os_error() == Some(libc::ETXTBSY) => {
                    assert!(
                        Instant::now() < deadline,
                        "timed out waiting for copied test binary to become executable"
                    );
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(error) => panic!("failed to run store initialization: {error}"),
            }
        }
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
        DaemonGuard { child }
    }

    fn read_json(path: PathBuf) -> Option<Value> {
        fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    }

    fn daemon_lifecycle(root: &Path) -> Option<Value> {
        read_json(root.join("daemon/status.json"))
    }

    fn semantic_job(root: &Path) -> Option<Value> {
        read_json(root.join("daemon/jobs/semantic-index.json"))
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

    fn wait_for_disabled_cycle(root: &Path, pid: u32) {
        wait_for("semantic-disabled daemon cycle", || {
            let Some(lifecycle) = daemon_lifecycle(root) else {
                return false;
            };
            lifecycle["status"] == "running"
                && lifecycle["pid"] == pid
                && lifecycle["semantic_runtime_active"] == false
                && lifecycle["config_reload"]["status"] == "applied"
                && lifecycle["config_reload"]["applied"]["semantic_enabled"] == false
                && semantic_job(root).is_some_and(|job| job["status"] == "disabled")
        });
    }

    fn wait_for_active_cycle(root: &Path, pid: u32) {
        wait_for("semantic-active daemon cycle", || {
            let Some(lifecycle) = daemon_lifecycle(root) else {
                return false;
            };
            lifecycle["status"] == "running"
                && lifecycle["pid"] == pid
                && lifecycle["semantic_runtime_active"] == true
                && lifecycle["config_reload"]["status"] == "applied"
                && lifecycle["config_reload"]["applied"]["semantic_enabled"] == true
                && semantic_job(root).is_some_and(|job| {
                    job["status"] == "empty" && job["reason"] == "no_searchable_items"
                })
        });
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
        write_config(temp.path(), false);
        initialize_store(&temp, &binary);
        let mut daemon = spawn_daemon(&temp, &binary, 1);
        let original_pid = daemon.pid();
        wait_for_disabled_cycle(temp.path(), original_pid);

        write_config(temp.path(), true);
        let setup = run_supported_setup(&temp, &binary);
        assert_eq!(
            setup["background_indexing"]["daemon_autostart"]["status"],
            "deferred"
        );

        wait_for("live semantic daemon activation", || {
            let Some(lifecycle) = daemon_lifecycle(temp.path()) else {
                return false;
            };
            lifecycle["pid"] == original_pid
                && lifecycle["semantic_runtime_active"] == true
                && lifecycle["config_reload"]["status"] == "applied"
                && lifecycle["config_reload"]["applied"]["semantic_enabled"] == true
                && temp.path().join("daemon/query-endpoint.json").exists()
                && semantic_job(temp.path()).is_some_and(|job| job["status"] == "empty")
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

        write_config(temp.path(), false);
        let _setup = run_supported_setup(&temp, &binary);
        wait_for("live semantic daemon deactivation", || {
            daemon_lifecycle(temp.path()).is_some_and(|lifecycle| {
                lifecycle["pid"] == original_pid
                    && lifecycle["semantic_runtime_active"] == false
                    && lifecycle["config_reload"]["status"] == "applied"
                    && lifecycle["config_reload"]["applied"]["semantic_enabled"] == false
                    && !temp.path().join("daemon/query-endpoint.json").exists()
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
    fn status_distinguishes_config_mutation_from_live_activation() {
        let temp = tempdir();
        let binary = copied_ctx_binary(&temp);
        write_config(temp.path(), false);
        initialize_store(&temp, &binary);
        let daemon = spawn_daemon(&temp, &binary, 120);
        wait_for_disabled_cycle(temp.path(), daemon.pid());

        write_config(temp.path(), true);
        let _setup = run_supported_setup(&temp, &binary);
        let status = daemon_status(&temp, &binary);

        assert_eq!(status["running"], true);
        assert_eq!(status["semantic_runtime_active"], false);
        assert_eq!(status["config_reload"]["status"], "pending");
        assert_eq!(status["config_reload"]["out_of_sync"], true);
        assert_eq!(
            status["config_reload"]["applied"]["semantic_enabled"],
            false
        );
        assert_eq!(
            status["config_reload"]["requested"]["semantic_enabled"],
            true
        );
        assert_eq!(status["jobs"]["semantic_index"]["enabled"], true);
        assert_eq!(status["jobs"]["semantic_index"]["runtime_active"], false);
        assert_eq!(status["jobs"]["semantic_index"]["status"], "pending");
        assert_eq!(
            status["jobs"]["semantic_index"]["reason"],
            "daemon_config_reload_pending"
        );
        assert!(!temp.path().join("daemon/query-endpoint.json").exists());
    }

    #[test]
    fn malformed_reload_is_failed_and_retried_without_changing_live_state() {
        let temp = tempdir();
        let binary = copied_ctx_binary(&temp);
        write_config(temp.path(), false);
        initialize_store(&temp, &binary);
        let mut daemon = spawn_daemon(&temp, &binary, 1);
        wait_for_disabled_cycle(temp.path(), daemon.pid());

        fs::write(
            temp.path().join("config.toml"),
            "[search\nsemantic = true\n",
        )
        .unwrap();
        wait_for("failed config reload", || {
            daemon_lifecycle(temp.path()).is_some_and(|status| {
                status["config_reload"]["status"] == "failed"
                    && status["config_reload"]["applied"]["semantic_enabled"] == false
                    && status["semantic_runtime_active"] == false
                    && status["config_reload"]["last_error"]
                        .as_str()
                        .is_some_and(|error| error.contains("parse"))
            })
        });
        daemon.assert_running();
        assert!(!temp.path().join("daemon/query-endpoint.json").exists());

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

        write_config(temp.path(), true);
        wait_for("reload recovery and semantic activation", || {
            daemon_lifecycle(temp.path()).is_some_and(|status| {
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
        write_config(temp.path(), true);
        initialize_store(&temp, &binary);
        let mut daemon = spawn_daemon(&temp, &binary, 1);
        wait_for_active_cycle(temp.path(), daemon.pid());

        fs::write(
            temp.path().join("config.toml"),
            "[search\nsemantic = false\n",
        )
        .unwrap();
        wait_for(
            "failed config reload with retained semantic runtime",
            || {
                daemon_lifecycle(temp.path()).is_some_and(|status| {
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
        assert_eq!(status["jobs"]["semantic_index"]["status"], "empty");
        assert_eq!(
            status["jobs"]["semantic_index"]["reason"],
            "no_searchable_items"
        );
        assert_eq!(status["jobs"]["semantic_index"]["last_run_status"], "empty");
        assert_eq!(
            status["jobs"]["semantic_index"]["last_run_reason"],
            "no_searchable_items"
        );
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
        write_config(temp.path(), false);
        initialize_store(&temp, &binary);
        let mut daemon = spawn_daemon(&temp, &binary, 1);
        wait_for_disabled_cycle(temp.path(), daemon.pid());

        fs::create_dir(temp.path().join("daemon/query-endpoint.json")).unwrap();
        write_config(temp.path(), true);
        let setup = run_supported_setup(&temp, &binary);
        assert_eq!(
            setup["background_indexing"]["daemon_autostart"]["status"],
            "deferred"
        );
        wait_for("failed semantic runtime activation", || {
            daemon_lifecycle(temp.path()).is_some_and(|status| {
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
            temp.path().join("daemon/query-endpoint.json").is_dir(),
            "the endpoint path should remain the blocking test fixture"
        );
    }

    #[test]
    fn initial_semantic_activation_failure_fails_daemon_startup_truthfully() {
        let temp = tempdir();
        let binary = copied_ctx_binary(&temp);
        write_config(temp.path(), true);
        initialize_store(&temp, &binary);
        fs::create_dir_all(temp.path().join("daemon/query-endpoint.json")).unwrap();

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
