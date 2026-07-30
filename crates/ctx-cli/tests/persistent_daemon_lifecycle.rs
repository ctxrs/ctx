mod support;

#[cfg(any(unix, windows))]
mod native {
    use std::{
        collections::BTreeMap,
        fs,
        io::{BufRead, BufReader, Write},
        path::{Path, PathBuf},
        process::{Child, ChildStdin, ChildStdout, Command as StdCommand, Output, Stdio},
        sync::Mutex,
        thread,
        time::{Duration, Instant, SystemTime},
    };

    use serde_json::{json, Value};

    use super::support::{copied_ctx_binary, ctx_from_binary, tempdir, Command, TempDir};

    const COMMAND_TIMEOUT: Duration = Duration::from_secs(20);
    const OBSERVATION_TIMEOUT: Duration = Duration::from_secs(12);
    const QUIESCENT_WINDOW: Duration = Duration::from_secs(3);
    #[cfg(target_os = "linux")]
    const LINUX_IDLE_SOAK: Duration = Duration::from_secs(8);
    #[cfg(target_os = "linux")]
    const LINUX_IDLE_ONE_CORE_PERCENT_CEILING: f64 = 0.5;

    static TEST_SERIAL: Mutex<()> = Mutex::new(());

    struct Harness {
        temp: TempDir,
        binary: PathBuf,
    }

    impl Harness {
        fn new() -> Self {
            let temp = tempdir();
            let binary = copied_ctx_binary(&temp);
            fs::write(
                temp.path().join("config.toml"),
                concat!(
                    "[analytics]\n",
                    "enabled = false\n\n",
                    "[upgrade]\n",
                    "auto = \"off\"\n\n",
                    "[search]\n",
                    "semantic = false\n",
                ),
            )
            .unwrap();
            Self { temp, binary }
        }

        fn root(&self) -> &Path {
            self.temp.path()
        }

        fn std_command(&self) -> StdCommand {
            let mut prepared: Command = ctx_from_binary(&self.temp, &self.binary);
            prepared.env_remove("CTX_DAEMON_AUTOSTART_IDLE_EXIT_SECONDS");
            let mut command = StdCommand::new(prepared.get_program());
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

        fn spawn(&self, args: &[&str], input: Option<&[u8]>) -> Child {
            let mut command = self.std_command();
            command
                .args(args)
                .stdin(if input.is_some() {
                    Stdio::piped()
                } else {
                    Stdio::null()
                })
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let deadline = Instant::now() + Duration::from_secs(1);
            let mut child = loop {
                match command.spawn() {
                    Ok(child) => break child,
                    Err(error) if error.raw_os_error() == Some(26) && Instant::now() < deadline => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("spawn ctx {:?}: {error}", args),
                }
            };
            if let Some(input) = input {
                child
                    .stdin
                    .take()
                    .expect("piped ctx stdin")
                    .write_all(input)
                    .expect("write ctx stdin");
            }
            child
        }

        fn output(&self, args: &[&str]) -> Output {
            wait_for_output(self.spawn(args, None), COMMAND_TIMEOUT, args)
        }

        fn json(&self, args: &[&str]) -> Value {
            success_json(self.output(args), args)
        }

        fn mcp_initialize(&self) -> Output {
            let input = mcp_initialize_input();
            wait_for_output(
                self.spawn(&["mcp", "serve"], Some(&input)),
                COMMAND_TIMEOUT,
                &["mcp", "serve"],
            )
        }

        fn mcp_session(&self) -> McpSession {
            let mut command = self.std_command();
            command
                .args(["mcp", "serve"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = command.spawn().expect("spawn persistent MCP session");
            McpSession {
                stdin: child.stdin.take().expect("MCP session stdin"),
                stdout: BufReader::new(child.stdout.take().expect("MCP session stdout")),
                child,
            }
        }

        fn daemon_status(&self) -> Value {
            self.json(&["daemon", "status", "--format=json"])["daemon"].clone()
        }

        fn setup_wait(&self) -> String {
            let setup = self.json(&["setup", "--wait", "--format=json", "--progress", "none"]);
            assert_eq!(setup["schema_version"], 2, "{setup:#}");
            assert_eq!(setup["mode"], "ready", "{setup:#}");
            setup["lexical"]["generation_id"]
                .as_str()
                .expect("setup should publish a lexical generation")
                .to_owned()
        }

        fn search(&self, query: &str, refresh: &str) -> Value {
            self.json(&[
                "search",
                query,
                "--provider",
                "codex",
                "--refresh",
                refresh,
                "--format=json",
            ])
        }

        fn best_effort_disable(&self) {
            let mut command = self.std_command();
            command
                .args(["daemon", "disable", "--format=json"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            if let Ok(child) = command.spawn() {
                let _ = wait_for_output_best_effort(child, Duration::from_secs(6));
            }
            if let Some(pid) = read_lock(self.root())
                .as_ref()
                .and_then(|lock| json_u32(lock, "pid"))
                .filter(|pid| process_is_running(*pid))
            {
                let _ = terminate_process(pid);
                let _ = wait_for_process_state(pid, false, Duration::from_secs(3));
            }
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            self.best_effort_disable();
        }
    }

    struct McpSession {
        child: Child,
        stdin: ChildStdin,
        stdout: BufReader<ChildStdout>,
    }

    impl McpSession {
        fn request(&mut self, request: Value) -> Value {
            serde_json::to_writer(&mut self.stdin, &request).expect("encode MCP request");
            self.stdin.write_all(b"\n").expect("terminate MCP request");
            self.stdin.flush().expect("flush MCP request");
            let mut line = String::new();
            self.stdout
                .read_line(&mut line)
                .expect("read MCP response");
            assert!(!line.is_empty(), "MCP server exited without a response");
            serde_json::from_str(&line).expect("decode MCP response")
        }
    }

    impl Drop for McpSession {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct FileSnapshot {
        bytes: Vec<u8>,
        len: u64,
        modified: SystemTime,
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct QuietSnapshot {
        index: BTreeMap<PathBuf, FileSnapshot>,
        wakeup: FileSnapshot,
    }

    #[test]
    fn persistent_daemon_release_lifecycle_is_event_driven_and_self_healing() {
        let _serial = TEST_SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let harness = Harness::new();
        let source = write_codex_session(
            harness.root(),
            "persistent daemon initial generation oracle",
        );

        let initial_generation = harness.setup_wait();
        let initial = harness.search("persistent daemon initial generation oracle", "off");
        assert_search_result(
            &initial,
            "persistent daemon initial generation oracle",
            &initial_generation,
        );
        let initial_config = fs::read_to_string(harness.root().join("config.toml")).unwrap();
        assert!(
            !initial_config.contains("[daemon]"),
            "the daemon must be enabled by default without a persisted opt-in: {initial_config}"
        );

        let initial_status = wait_for_daemon(&harness, None);
        assert_eq!(initial_status["enabled"], true, "{initial_status:#}");
        assert_eq!(initial_status["running"], true, "{initial_status:#}");
        assert_eq!(
            initial_status["wakeup"]["idle_strategy"], "blocking",
            "{initial_status:#}"
        );
        let initial_pid = live_pid(&initial_status);
        wait_for_relational_generation(&harness, &initial_generation);

        let quiet_before = wait_for_stable_quiet_snapshot(&harness);
        let wakeup_before: Value = serde_json::from_slice(&quiet_before.wakeup.bytes).unwrap();
        assert_eq!(
            wakeup_before["wakeup"]["timeout_wakeups"], 0,
            "{wakeup_before:#}"
        );
        thread::sleep(QUIESCENT_WINDOW);
        assert!(
            process_is_running(initial_pid),
            "default-on daemon {initial_pid} exited while quiescent"
        );
        let quiet_after = capture_quiet_snapshot(&harness);
        assert_eq!(
            quiet_after.index, quiet_before.index,
            "quiescent daemon rewrote lexical index or metadata"
        );
        assert_eq!(
            quiet_after.wakeup, quiet_before.wakeup,
            "blocking daemon rewrote its event/work receipt while idle"
        );

        append_codex_message(
            &source,
            "2026-07-29T12:01:00.000Z",
            "assistant",
            "persistent daemon passive append oracle",
        );
        let append_job = wait_for_job(&harness, "passive append publication", |job| {
            (job["request_state"] == "published"
                && job["published_generation"]
                    .as_str()
                    .is_some_and(|generation| generation != initial_generation))
            .then_some(job)
        });
        let append_generation = append_job["published_generation"]
            .as_str()
            .expect("passive refresh generation")
            .to_owned();
        assert_ne!(append_generation, initial_generation, "{append_job:#}");

        // The job receipt above is the first observation after the append.
        // No ctx command runs between the provider mutation and publication.
        let appended = harness.search("persistent daemon passive append oracle", "off");
        assert_search_result(
            &appended,
            "persistent daemon passive append oracle",
            &append_generation,
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let retained_meta_path = harness.root().join("search/lexical/meta.json");
            let retained_manifest_path =
                generation_manifest_path(harness.root(), &append_generation);
            let retained_meta = snapshot_file(&retained_meta_path);
            let retained_manifest = snapshot_file(&retained_manifest_path);
            let sessions_root = harness.root().join(".codex/sessions");
            let original_mode = fs::metadata(&sessions_root).unwrap().permissions().mode();
            fs::set_permissions(&sessions_root, fs::Permissions::from_mode(0)).unwrap();
            let failed_job =
                wait_for_job(&harness, "failed refresh with retained generation", |job| {
                    (job["request_state"] == "failed"
                        && (job["published_generation"] == append_generation
                            || job["previous_generation"] == append_generation))
                        .then_some(job)
                });
            fs::set_permissions(
                &sessions_root,
                fs::Permissions::from_mode(original_mode),
            )
            .unwrap();
            assert_eq!(
                failed_job["error_code"],
                "all_provider_terminal_coverage_unavailable",
                "{failed_job:#}"
            );
            assert!(
                failed_job["last_error"]
                    .as_str()
                    .is_some_and(|error| {
                        error.contains("no current resolver-owning route family for codex")
                    }),
                "{failed_job:#}"
            );
            assert_eq!(
                snapshot_file(&retained_meta_path),
                retained_meta,
                "failed refresh rewrote the verified Tantivy generation metadata"
            );
            assert_eq!(
                snapshot_file(&retained_manifest_path),
                retained_manifest,
                "failed refresh rewrote the verified generation manifest"
            );
            let retained = harness.search("persistent daemon passive append oracle", "off");
            assert_search_result(
                &retained,
                "persistent daemon passive append oracle",
                &append_generation,
            );
        }

        let disabled = harness.json(&["daemon", "disable", "--format=json"]);
        assert_eq!(disabled["daemon_enabled"], false, "{disabled:#}");
        assert_eq!(disabled["running"], false, "{disabled:#}");
        wait_for_process_state(initial_pid, false, Duration::from_secs(5))
            .expect("disabled daemon should stop");
        let disabled_status = harness.daemon_status();
        assert_eq!(disabled_status["enabled"], false, "{disabled_status:#}");
        assert_eq!(disabled_status["running"], false, "{disabled_status:#}");
        let disabled_config = fs::read_to_string(harness.root().join("config.toml")).unwrap();
        assert!(
            disabled_config.contains("[daemon]") && disabled_config.contains("enabled = false"),
            "disable was not durable: {disabled_config}"
        );

        let stopped_wakeup = read_json_file(&wakeup_path(harness.root()));
        assert_eq!(stopped_wakeup["status"], "stopped", "{stopped_wakeup:#}");
        assert_eq!(
            stopped_wakeup["wakeup"]["timeout_wakeups"], 0,
            "bounded qualification observed a polling timeout signature: {stopped_wakeup:#}"
        );
        assert!(
            counter(&stopped_wakeup, "filesystem_signals")
                > counter(&wakeup_before, "filesystem_signals"),
            "provider mutations did not produce a filesystem wake: {stopped_wakeup:#}"
        );
        assert!(
            counter(&stopped_wakeup, "work_cycles") > counter(&wakeup_before, "work_cycles"),
            "provider mutation did not produce bounded work: {stopped_wakeup:#}"
        );

        let disabled_mcp = harness.mcp_initialize();
        assert_mcp_initialize(&disabled_mcp);
        thread::sleep(Duration::from_millis(300));
        assert_eq!(
            harness.daemon_status()["running"],
            false,
            "MCP must not undo durable daemon disable"
        );

        let enabled = harness.json(&["daemon", "enable", "--format=json"]);
        assert_eq!(enabled["daemon_enabled"], true, "{enabled:#}");
        assert_eq!(enabled["running"], true, "{enabled:#}");
        let enabled_pid = json_u32(&enabled, "pid").expect("enabled daemon pid");
        assert_ne!(enabled_pid, initial_pid, "{enabled:#}");
        assert!(process_is_running(enabled_pid), "{enabled:#}");
        let enabled_config = fs::read_to_string(harness.root().join("config.toml")).unwrap();
        assert!(
            enabled_config.contains("enabled = true"),
            "enable was not durable: {enabled_config}"
        );

        let search_stale = force_unexpected_death(&harness, enabled_pid);
        let search_recovery =
            harness.search("persistent daemon passive append oracle", "background");
        assert_search_result(
            &search_recovery,
            "persistent daemon passive append oracle",
            &append_generation,
        );
        let search_recovered = wait_for_daemon(&harness, Some(enabled_pid));
        let search_recovered_pid = live_pid(&search_recovered);
        assert_replaced_stale_owner(&harness, &search_stale, search_recovered_pid);

        let mcp_stale = force_unexpected_death(&harness, search_recovered_pid);
        let mcp_recovery = harness.mcp_initialize();
        assert_mcp_initialize(&mcp_recovery);
        let mcp_recovered = wait_for_daemon(&harness, Some(search_recovered_pid));
        let mcp_recovered_pid = live_pid(&mcp_recovered);
        assert_replaced_stale_owner(&harness, &mcp_stale, mcp_recovered_pid);

        let wait_stale = force_unexpected_death(&harness, mcp_recovered_pid);
        let wait_recovery = harness.search("persistent daemon passive append oracle", "wait");
        assert_search_result(
            &wait_recovery,
            "persistent daemon passive append oracle",
            &append_generation,
        );
        let wait_recovered = wait_for_daemon(&harness, Some(mcp_recovered_pid));
        let wait_recovered_pid = live_pid(&wait_recovered);
        assert_replaced_stale_owner(&harness, &wait_stale, wait_recovered_pid);
        let dedup_owner = read_lock(harness.root()).expect("active dedup owner");

        let mcp_input = mcp_initialize_input();
        let mcp_child = harness.spawn(&["mcp", "serve"], Some(&mcp_input));
        let search_child = harness.spawn(
            &[
                "search",
                "persistent daemon passive append oracle",
                "--provider",
                "codex",
                "--format=json",
            ],
            None,
        );
        let wait_child = harness.spawn(
            &[
                "search",
                "persistent daemon passive append oracle",
                "--provider",
                "codex",
                "--refresh",
                "wait",
                "--format=json",
            ],
            None,
        );
        let mcp_output = wait_for_output(mcp_child, COMMAND_TIMEOUT, &["mcp", "serve", "dedup"]);
        let search_output =
            wait_for_output(search_child, COMMAND_TIMEOUT, &["search", "json", "dedup"]);
        let wait_output =
            wait_for_output(wait_child, COMMAND_TIMEOUT, &["search", "wait", "dedup"]);
        assert_mcp_initialize(&mcp_output);
        let concurrent_search = success_json(search_output, &["search", "json", "dedup"]);
        let concurrent_wait = success_json(wait_output, &["search", "wait", "dedup"]);
        assert_search_result(
            &concurrent_search,
            "persistent daemon passive append oracle",
            &append_generation,
        );
        assert_search_result(
            &concurrent_wait,
            "persistent daemon passive append oracle",
            &append_generation,
        );

        let dedup_status = wait_for_daemon(&harness, None);
        let dedup_pid = live_pid(&dedup_status);
        assert_eq!(dedup_pid, wait_recovered_pid, "{dedup_status:#}");
        let dedup_after = read_lock(harness.root()).expect("deduplicated owner lock");
        assert_eq!(dedup_after["owner_id"], dedup_owner["owner_id"]);
        assert_eq!(
            dedup_status["lock_identity"]["pid"], dedup_status["live_pid"],
            "{dedup_status:#}"
        );
        assert_single_daemon_process(&harness, dedup_pid);
    }

    #[test]
    fn long_lived_mcp_search_recovers_daemon_after_startup() {
        let _serial = TEST_SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let harness = Harness::new();
        write_codex_session(harness.root(), "long lived mcp recovery oracle");
        let generation = harness.setup_wait();
        let daemon = wait_for_daemon(&harness, None);
        let daemon_pid = live_pid(&daemon);

        let mut mcp = harness.mcp_session();
        let initialized = mcp.request(json!({
            "jsonrpc": "2.0",
            "id": "init",
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "daemon-recovery-test", "version": "0" }
            }
        }));
        assert_eq!(initialized["result"]["serverInfo"]["name"], "ctx");

        let stale = force_unexpected_death(&harness, daemon_pid);
        let searched = mcp.request(json!({
            "jsonrpc": "2.0",
            "id": "search-after-daemon-death",
            "method": "tools/call",
            "params": {
                "name": "search",
                "arguments": {
                    "query": "long lived mcp recovery oracle",
                    "provider": "codex",
                    "limit": 5
                }
            }
        }));
        assert!(
            searched.get("error").is_none()
                && searched["result"]["isError"].as_bool() != Some(true),
            "{searched:#}"
        );
        let recovered = wait_for_daemon(&harness, Some(daemon_pid));
        let recovered_pid = live_pid(&recovered);
        assert_replaced_stale_owner(&harness, &stale, recovered_pid);

        let payload = &searched["result"]["structuredContent"];
        assert_eq!(
            payload["retrieval"]["generation_id"], generation,
            "{searched:#}"
        );
    }

    #[test]
    fn explicit_finite_idle_daemon_exits_without_orphaning() {
        let _serial = TEST_SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let harness = Harness::new();
        write_codex_session(harness.root(), "finite idle orphan oracle");
        let child = harness.spawn(
            &[
                "daemon",
                "run",
                "--force",
                "--idle-exit-seconds",
                "1",
            ],
            None,
        );
        let pid = child.id();
        let output = wait_for_output(
            child,
            Duration::from_secs(8),
            &["daemon", "run", "--idle-exit-seconds", "1"],
        );
        assert!(
            output.status.success(),
            "finite-idle daemon failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        wait_for_process_state(pid, false, Duration::from_secs(2))
            .expect("finite-idle daemon child must exit");
        assert!(
            read_lock(harness.root())
                .as_ref()
                .and_then(|lock| json_u32(lock, "pid"))
                .is_none_or(|owner| !process_is_running(owner)),
            "finite-idle daemon left a live lock owner"
        );
        #[cfg(target_os = "linux")]
        assert!(
            linux_daemon_processes(&harness).is_empty(),
            "finite-idle daemon left a task-root orphan"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn concurrent_dead_daemon_triggers_all_join_the_replacement_owner() {
        let _serial = TEST_SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let harness = Harness::new();
        write_codex_session(harness.root(), "concurrent dead daemon recovery oracle");
        let generation = harness.setup_wait();
        let daemon = wait_for_daemon(&harness, None);
        let stale = force_unexpected_death(&harness, live_pid(&daemon));

        // Model an ordinary installation-lock checker overlapping these
        // triggers. No upgrade scheduler record is active, so the callers
        // should still start or join one replacement daemon.
        let installation_check = hold_non_upgrade_installation_lock(&harness);
        let mcp_input = mcp_initialize_input();
        let mcp_child = harness.spawn(&["mcp", "serve"], Some(&mcp_input));
        let search_child = harness.spawn(
            &[
                "search",
                "concurrent dead daemon recovery oracle",
                "--provider",
                "codex",
                "--format=json",
            ],
            None,
        );
        let wait_child = harness.spawn(
            &[
                "search",
                "concurrent dead daemon recovery oracle",
                "--provider",
                "codex",
                "--refresh",
                "wait",
                "--format=json",
            ],
            None,
        );

        let mcp_output =
            wait_for_output(mcp_child, COMMAND_TIMEOUT, &["mcp", "serve", "dead-race"]);
        let search_output = wait_for_output(
            search_child,
            COMMAND_TIMEOUT,
            &["search", "json", "dead-race"],
        );
        let wait_output = wait_for_output(
            wait_child,
            COMMAND_TIMEOUT,
            &["search", "wait", "dead-race"],
        );
        drop(installation_check);
        assert_mcp_initialize(&mcp_output);
        let search = success_json(search_output, &["search", "json", "dead-race"]);
        let wait = success_json(wait_output, &["search", "wait", "dead-race"]);
        assert_search_result(
            &search,
            "concurrent dead daemon recovery oracle",
            &generation,
        );
        assert_search_result(&wait, "concurrent dead daemon recovery oracle", &generation);

        let replacement = wait_for_daemon(&harness, None);
        let replacement_pid = live_pid(&replacement);
        assert_replaced_stale_owner(&harness, &stale, replacement_pid);
        assert_single_daemon_process(&harness, replacement_pid);
    }

    #[cfg(target_os = "linux")]
    fn hold_non_upgrade_installation_lock(harness: &Harness) -> fs::File {
        use std::os::fd::AsRawFd as _;

        let binary_name = harness
            .binary
            .file_name()
            .expect("copied ctx binary name")
            .to_string_lossy();
        let lock_path = harness
            .binary
            .with_file_name(format!(".{binary_name}.install.lock"));
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap_or_else(|error| {
                panic!(
                    "open test-owned installation lock {}: {error}",
                    lock_path.display()
                )
            });
        let acquired = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
        assert!(
            acquired,
            "acquire test-owned installation lock {}: {}",
            lock_path.display(),
            std::io::Error::last_os_error()
        );
        lock
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_idle_soak_emits_native_sub_half_percent_receipt() {
        let _serial = TEST_SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let harness = Harness::new();
        write_codex_session(harness.root(), "linux daemon idle soak oracle");
        let generation = harness.setup_wait();
        wait_for_relational_generation(&harness, &generation);
        let daemon = wait_for_daemon(&harness, None);
        let pid = live_pid(&daemon);
        let quiet_before = wait_for_stable_quiet_snapshot(&harness);
        let ticks_per_second = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        assert!(ticks_per_second > 0, "Linux _SC_CLK_TCK must be available");
        let cpu_before = linux_process_cpu_ticks(pid);
        let wall_started = Instant::now();
        thread::sleep(LINUX_IDLE_SOAK);
        let wall_elapsed = wall_started.elapsed();
        let cpu_after = linux_process_cpu_ticks(pid);
        let quiet_after = capture_quiet_snapshot(&harness);
        let consumed_ticks = cpu_after.saturating_sub(cpu_before);
        let cpu_seconds = consumed_ticks as f64 / ticks_per_second as f64;
        let one_core_percent = cpu_seconds / wall_elapsed.as_secs_f64() * 100.0;
        let index_unchanged = quiet_after.index == quiet_before.index;
        let wakeup_unchanged = quiet_after.wakeup == quiet_before.wakeup;
        let daemon_alive = process_is_running(pid);
        let passed = daemon_alive
            && index_unchanged
            && wakeup_unchanged
            && one_core_percent < LINUX_IDLE_ONE_CORE_PERCENT_CEILING;
        let wakeup: Value = serde_json::from_slice(&quiet_after.wakeup.bytes).unwrap();
        let receipt = json!({
            "schema_version": 1,
            "evidence": "platform_native",
            "platform": "linux",
            "measurement": "proc_pid_stat",
            "pid": pid,
            "sample_ms": wall_elapsed.as_millis(),
            "clock_ticks_per_second": ticks_per_second,
            "cpu_ticks": consumed_ticks,
            "one_core_percent": one_core_percent,
            "ceiling_one_core_percent_exclusive": LINUX_IDLE_ONE_CORE_PERCENT_CEILING,
            "daemon_alive": daemon_alive,
            "index_unchanged": index_unchanged,
            "wakeup_receipt_unchanged": wakeup_unchanged,
            "timeout_wakeups": counter(&wakeup, "timeout_wakeups"),
            "work_cycles": counter(&wakeup, "work_cycles"),
            "no_work_cycles": counter(&wakeup, "no_work_cycles"),
            "passed": passed,
        });
        write_linux_idle_receipt(harness.root(), &receipt);
        println!(
            "CTX_LINUX_IDLE_SOAK_RECEIPT={}",
            serde_json::to_string(&receipt).unwrap()
        );
        assert!(
            passed,
            "Linux idle soak exceeded the conservative sanity gate: {receipt:#}"
        );
    }

    fn write_codex_session(root: impl AsRef<Path>, text: &str) -> PathBuf {
        let path = root
            .as_ref()
            .join(".codex/sessions/2026/07/29/persistent-daemon.jsonl");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            format!(
                "{}\n{}\n",
                json!({
                    "timestamp": "2026-07-29T12:00:00.000Z",
                    "type": "session_meta",
                    "payload": {
                        "id": "019c08d7-0000-7000-8000-000000000029",
                        "timestamp": "2026-07-29T12:00:00.000Z",
                        "cwd": "/repo/persistent-daemon",
                        "originator": "codex-cli",
                        "cli_version": "0.200.0",
                        "source": "cli",
                        "model_provider": "openai"
                    }
                }),
                json!({
                    "timestamp": "2026-07-29T12:00:01.000Z",
                    "type": "response_item",
                    "payload": {
                        "type": "message",
                        "role": "user",
                        "content": [{"type": "input_text", "text": text}]
                    }
                })
            ),
        )
        .unwrap();
        path
    }

    fn append_codex_message(path: &Path, timestamp: &str, role: &str, text: &str) {
        let content_type = if role == "user" {
            "input_text"
        } else {
            "output_text"
        };
        let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
        writeln!(
            file,
            "{}",
            json!({
                "timestamp": timestamp,
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": role,
                    "content": [{"type": content_type, "text": text}]
                }
            })
        )
        .unwrap();
    }

    fn success_json(output: Output, args: &[&str]) -> Value {
        assert!(
            output.status.success(),
            "ctx {:?} failed with {}:\nstdout:\n{}\nstderr:\n{}",
            args,
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "ctx {:?} returned invalid JSON ({error}):\n{}",
                args,
                String::from_utf8_lossy(&output.stdout)
            )
        })
    }

    fn wait_for_output(mut child: Child, timeout: Duration, args: &[&str]) -> Output {
        let deadline = Instant::now() + timeout;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return child.wait_with_output().expect("collect ctx output"),
                Ok(None) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(20));
                }
                Ok(None) => {
                    let pid = child.id();
                    let _ = child.kill();
                    let output = child
                        .wait_with_output()
                        .expect("collect timed-out ctx output");
                    panic!(
                        "ctx {:?} pid {pid} exceeded {:?}:\nstdout:\n{}\nstderr:\n{}",
                        args,
                        timeout,
                        String::from_utf8_lossy(&output.stdout),
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
                Err(error) => panic!("poll ctx {:?}: {error}", args),
            }
        }
    }

    fn wait_for_output_best_effort(mut child: Child, timeout: Duration) -> Option<Output> {
        let deadline = Instant::now() + timeout;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return child.wait_with_output().ok(),
                Ok(None) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(20));
                }
                Ok(None) => {
                    let _ = child.kill();
                    return child.wait_with_output().ok();
                }
                Err(_) => return None,
            }
        }
    }

    fn wait_for_daemon(harness: &Harness, previous_pid: Option<u32>) -> Value {
        let deadline = Instant::now() + OBSERVATION_TIMEOUT;
        loop {
            let last = harness.daemon_status();
            if last["running"] == true
                && last["source_refresh_endpoint"]["available"] == true
                && previous_pid.is_none_or(|previous| live_pid(&last) != previous)
            {
                return last;
            }
            assert!(
                Instant::now() < deadline,
                "daemon did not become ready after previous pid {previous_pid:?}: {last:#}"
            );
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn wait_for_relational_generation(harness: &Harness, generation: &str) {
        let deadline = Instant::now() + OBSERVATION_TIMEOUT;
        loop {
            let last = harness.json(&["status", "--format=json"]);
            if last["relational"]["status"] == "ready"
                && last["relational"]["active_core_generation_id"] == generation
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "relational projection did not reach generation {generation}: {last:#}"
            );
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn wait_for_job(
        harness: &Harness,
        description: &str,
        predicate: impl Fn(Value) -> Option<Value>,
    ) -> Value {
        let path = harness.root().join("daemon/jobs/core-refresh.json");
        let deadline = Instant::now() + OBSERVATION_TIMEOUT;
        loop {
            let last = fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
            if let Some(result) = last.clone().and_then(&predicate) {
                return result;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {description}: job={last:#?}, wakeup={:#?}",
                fs::read(wakeup_path(harness.root()))
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            );
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn assert_search_result(packet: &Value, query: &str, generation: &str) {
        assert_eq!(packet["retrieval"]["index"], "source_backed", "{packet:#}");
        assert_eq!(
            packet["retrieval"]["generation_id"], generation,
            "{packet:#}"
        );
        assert!(
            packet["results"].as_array().is_some_and(|results| {
                results.iter().any(|result| {
                    result["snippet"]
                        .as_str()
                        .is_some_and(|snippet| snippet.contains(query))
                })
            }),
            "{packet:#}"
        );
    }

    fn wait_for_stable_quiet_snapshot(harness: &Harness) -> QuietSnapshot {
        let deadline = Instant::now() + OBSERVATION_TIMEOUT;
        let stable_for = Duration::from_millis(750);
        let mut previous = capture_quiet_snapshot(harness);
        let mut stable_since = Instant::now();
        loop {
            thread::sleep(Duration::from_millis(100));
            let current = capture_quiet_snapshot(harness);
            if current == previous {
                if stable_since.elapsed() >= stable_for {
                    return current;
                }
            } else {
                previous = current;
                stable_since = Instant::now();
            }
            assert!(
                Instant::now() < deadline,
                "daemon/index state did not quiesce: {previous:#?}"
            );
        }
    }

    fn capture_quiet_snapshot(harness: &Harness) -> QuietSnapshot {
        QuietSnapshot {
            index: snapshot_tree(&harness.root().join("search/lexical")),
            wakeup: snapshot_file(&wakeup_path(harness.root())),
        }
    }

    fn snapshot_tree(root: &Path) -> BTreeMap<PathBuf, FileSnapshot> {
        fn visit(root: &Path, path: &Path, files: &mut BTreeMap<PathBuf, FileSnapshot>) {
            let mut entries = fs::read_dir(path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
                .map(|entry| entry.unwrap())
                .collect::<Vec<_>>();
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let path = entry.path();
                let metadata = entry.metadata().unwrap();
                if metadata.is_dir() {
                    visit(root, &path, files);
                } else if metadata.is_file() {
                    files.insert(
                        path.strip_prefix(root).unwrap().to_path_buf(),
                        snapshot_file(&path),
                    );
                }
            }
        }

        let mut files = BTreeMap::new();
        visit(root, root, &mut files);
        files
    }

    fn snapshot_file(path: &Path) -> FileSnapshot {
        let metadata =
            fs::metadata(path).unwrap_or_else(|error| panic!("stat {}: {error}", path.display()));
        FileSnapshot {
            bytes: fs::read(path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display())),
            len: metadata.len(),
            modified: metadata
                .modified()
                .unwrap_or_else(|error| panic!("mtime {}: {error}", path.display())),
        }
    }

    fn generation_manifest_path(root: &Path, generation: &str) -> PathBuf {
        root.join("search/lexical/ctx-generations")
            .join(format!("{generation}.json"))
    }

    fn wakeup_path(root: &Path) -> PathBuf {
        root.join("daemon/wakeup.json")
    }

    fn read_json_file(path: &Path) -> Value {
        serde_json::from_slice(
            &fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display())),
        )
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()))
    }

    fn counter(receipt: &Value, name: &str) -> u64 {
        receipt["wakeup"][name]
            .as_u64()
            .unwrap_or_else(|| panic!("missing wakeup counter {name}: {receipt:#}"))
    }

    fn live_pid(status: &Value) -> u32 {
        json_u32(status, "live_pid")
            .unwrap_or_else(|| panic!("daemon status has no live pid: {status:#}"))
    }

    fn json_u32(value: &Value, name: &str) -> Option<u32> {
        value
            .get(name)
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
    }

    fn read_lock(root: &Path) -> Option<Value> {
        fs::read(root.join("daemon/daemon.lock"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
    }

    fn force_unexpected_death(harness: &Harness, pid: u32) -> Value {
        let before = read_lock(harness.root()).expect("active daemon lock");
        assert_eq!(json_u32(&before, "pid"), Some(pid), "{before:#}");
        assert_eq!(before["released"], false, "{before:#}");
        terminate_process(pid).unwrap_or_else(|error| panic!("terminate daemon {pid}: {error}"));
        wait_for_process_state(pid, false, Duration::from_secs(5))
            .unwrap_or_else(|error| panic!("wait for daemon {pid} death: {error}"));
        let stale = read_lock(harness.root()).expect("stale daemon lock metadata");
        assert_eq!(stale["owner_id"], before["owner_id"], "{stale:#}");
        assert_eq!(json_u32(&stale, "pid"), Some(pid), "{stale:#}");
        assert_eq!(stale["released"], false, "{stale:#}");
        let report = harness.daemon_status();
        assert_eq!(report["running"], false, "{report:#}");
        assert_eq!(report["recoverable"], true, "{report:#}");
        stale
    }

    fn assert_replaced_stale_owner(harness: &Harness, stale: &Value, new_pid: u32) {
        let replacement = read_lock(harness.root()).expect("replacement daemon lock");
        assert_ne!(
            replacement["owner_id"], stale["owner_id"],
            "{replacement:#}"
        );
        assert_ne!(json_u32(stale, "pid"), Some(new_pid), "{replacement:#}");
        assert_eq!(
            json_u32(&replacement, "pid"),
            Some(new_pid),
            "{replacement:#}"
        );
        assert_eq!(replacement["released"], false, "{replacement:#}");
        assert!(process_is_running(new_pid), "{replacement:#}");
    }

    fn mcp_initialize_input() -> Vec<u8> {
        format!(
            "{}\n",
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {
                        "name": "persistent-daemon-qualification",
                        "version": "1"
                    }
                }
            })
        )
        .into_bytes()
    }

    fn assert_mcp_initialize(output: &Output) {
        assert!(
            output.status.success(),
            "MCP failed with {}:\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let response: Value = serde_json::from_slice(
            output
                .stdout
                .split(|byte| *byte == b'\n')
                .find(|line| !line.is_empty())
                .expect("MCP initialize response"),
        )
        .expect("valid MCP initialize response");
        assert_eq!(
            response["result"]["serverInfo"]["name"], "ctx",
            "{response:#}"
        );
    }

    fn wait_for_process_state(
        pid: u32,
        expected_running: bool,
        timeout: Duration,
    ) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        loop {
            let running = process_is_running(pid);
            if running == expected_running {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "pid {pid} running={running}, expected {expected_running}"
                ));
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[cfg(unix)]
    fn process_is_running(pid: u32) -> bool {
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return false;
        };
        let running = unsafe { libc::kill(pid, 0) } == 0;
        #[cfg(target_os = "linux")]
        if running {
            return fs::read_to_string(format!("/proc/{pid}/stat"))
                .ok()
                .and_then(|stat| stat.rsplit_once(") ").map(|(_, rest)| rest.to_owned()))
                .and_then(|rest| rest.split_whitespace().next().map(str::to_owned))
                .is_some_and(|state| state != "Z");
        }
        running
    }

    #[cfg(windows)]
    fn process_is_running(pid: u32) -> bool {
        use windows_sys::Win32::{
            Foundation::{CloseHandle, STILL_ACTIVE},
            System::Threading::{
                GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
            },
        };

        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            return false;
        }
        let mut exit_code = 0;
        let queried = unsafe { GetExitCodeProcess(handle, &mut exit_code) } != 0;
        unsafe {
            CloseHandle(handle);
        }
        queried && exit_code == STILL_ACTIVE as u32
    }

    #[cfg(unix)]
    fn terminate_process(pid: u32) -> std::io::Result<()> {
        let pid = libc::pid_t::try_from(pid).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "pid exceeds pid_t")
        })?;
        if unsafe { libc::kill(pid, libc::SIGKILL) } == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    #[cfg(windows)]
    fn terminate_process(pid: u32) -> std::io::Result<()> {
        use windows_sys::Win32::{
            Foundation::CloseHandle,
            System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE},
        };

        let handle = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let terminated = unsafe { TerminateProcess(handle, 137) } != 0;
        unsafe {
            CloseHandle(handle);
        }
        if terminated {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    #[cfg(target_os = "linux")]
    fn assert_single_daemon_process(harness: &Harness, expected_pid: u32) {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let pids = linux_daemon_processes(harness);
            if pids == vec![expected_pid] {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "concurrent triggers did not deduplicate to pid {expected_pid}: {pids:?}"
            );
            thread::sleep(Duration::from_millis(25));
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn assert_single_daemon_process(_harness: &Harness, expected_pid: u32) {
        assert!(
            process_is_running(expected_pid),
            "deduplicated daemon owner {expected_pid} is not running"
        );
    }

    #[cfg(target_os = "linux")]
    fn linux_daemon_processes(harness: &Harness) -> Vec<u32> {
        let expected_binary = fs::canonicalize(&harness.binary).unwrap();
        let expected_root = harness.root().as_os_str().as_encoded_bytes();
        let mut pids = Vec::new();
        for entry in fs::read_dir("/proc").unwrap() {
            let entry = entry.unwrap();
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };
            let Ok(command_line) = fs::read(entry.path().join("cmdline")) else {
                continue;
            };
            let args = command_line
                .split(|byte| *byte == 0)
                .filter(|arg| !arg.is_empty())
                .collect::<Vec<_>>();
            let binary_matches = args
                .first()
                .and_then(|arg| std::str::from_utf8(arg).ok())
                .and_then(|arg| fs::canonicalize(arg).ok())
                .is_some_and(|binary| binary == expected_binary);
            let root_matches = args.iter().any(|arg| *arg == expected_root);
            let daemon_run = args
                .windows(2)
                .any(|args| args[0] == b"daemon" && args[1] == b"run");
            if binary_matches && root_matches && daemon_run && process_is_running(pid) {
                pids.push(pid);
            }
        }
        pids.sort_unstable();
        pids
    }

    #[cfg(target_os = "linux")]
    fn linux_process_cpu_ticks(pid: u32) -> u64 {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat"))
            .unwrap_or_else(|error| panic!("read Linux process stat for {pid}: {error}"));
        let (_, fields) = stat
            .rsplit_once(") ")
            .unwrap_or_else(|| panic!("invalid Linux process stat for {pid}: {stat}"));
        let fields = fields.split_whitespace().collect::<Vec<_>>();
        let user_ticks = fields
            .get(11)
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or_else(|| panic!("missing Linux utime for {pid}: {stat}"));
        let system_ticks = fields
            .get(12)
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or_else(|| panic!("missing Linux stime for {pid}: {stat}"));
        user_ticks.saturating_add(system_ticks)
    }

    #[cfg(target_os = "linux")]
    fn write_linux_idle_receipt(root: &Path, receipt: &Value) {
        let directory = root.join("daemon/qualification");
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("linux-idle-soak.json"),
            serde_json::to_vec_pretty(receipt).unwrap(),
        )
        .unwrap();
    }
}
