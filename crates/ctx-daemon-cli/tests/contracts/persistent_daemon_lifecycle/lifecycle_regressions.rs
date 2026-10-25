use super::*;

#[test]
fn scratch_search_reports_persistent_start_and_lists_registered_roots() {
    let _serial = TEST_SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut harness = Harness::new();
    harness.json(&["index", "mode", "manual", "--format=json"]);
    harness.data_root = harness.home().join("scratch root");
    let notice = "Launched a persistent background indexer";

    let first = harness.output(&["search", "empty scratch oracle", "--limit", "1"]);
    assert!(
        !first.status.success(),
        "empty search unexpectedly succeeded"
    );
    let stderr = String::from_utf8_lossy(&first.stderr);
    assert_eq!(stderr.matches(notice).count(), 1, "{first:?}");
    assert!(
        stderr.contains(&harness.root().display().to_string()),
        "{stderr}"
    );
    assert!(stderr.contains("index mode manual"), "{stderr}");
    assert!(!String::from_utf8_lossy(&first.stdout).contains(notice));
    let pid = live_pid(&wait_for_daemon(&harness, None));
    assert!(process_is_running(pid), "search left no persistent daemon");

    let second = harness.output(&["search", "empty scratch oracle", "--limit", "1"]);
    assert!(
        !String::from_utf8_lossy(&second.stderr).contains(notice),
        "{second:?}"
    );
    let listed = harness.json(&["daemon", "status", "--all", "--format=json"]);
    assert_eq!(listed["schema_version"], 1);
    let rows = listed["daemons"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{listed:#}");
    assert_eq!(rows[0]["data_root"], json!(harness.root()));
    assert_eq!(rows[0]["daemon"]["running"], true, "{listed:#}");
    assert_eq!(rows[0]["daemon"]["live_pid"], pid, "{listed:#}");
    let human = harness.output(&["daemon", "status", "--all"]);
    assert!(human.status.success(), "{human:?}");
    assert!(String::from_utf8_lossy(&human.stdout).contains("Running"));
    assert!(process_is_running(pid), "status stopped the daemon");

    harness.json(&["index", "mode", "manual", "--format=json"]);
    wait_for_process_state(pid, false, OBSERVATION_TIMEOUT).unwrap();
    let manual = harness.output(&["search", "empty scratch oracle"]);
    assert!(
        !String::from_utf8_lossy(&manual.stderr).contains(notice),
        "{manual:?}"
    );
    assert_eq!(harness.daemon_status()["running"], false);
}

#[test]
fn scratch_notice_is_silent_for_managed_and_machine_searches() {
    let _serial = TEST_SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut harness = Harness::new();
    let notice = "Launched a persistent background indexer";
    let managed = harness.output(&["search", "empty managed oracle"]);
    assert!(
        !String::from_utf8_lossy(&managed.stderr).contains(notice),
        "{managed:?}"
    );
    let managed_pid = live_pid(&wait_for_daemon(&harness, None));
    harness.json(&["index", "mode", "manual", "--format=json"]);
    wait_for_process_state(managed_pid, false, OBSERVATION_TIMEOUT).unwrap();

    harness.data_root = harness.home().join("scratch");
    let off = harness.output(&["search", "empty scratch oracle", "--refresh", "off"]);
    assert!(
        !String::from_utf8_lossy(&off.stderr).contains(notice),
        "{off:?}"
    );
    assert_eq!(harness.daemon_status()["running"], false);
    write_codex_session(harness.home(), "scratch machine output oracle");
    let machine = harness.output(&[
        "search",
        "scratch machine output oracle",
        "--refresh",
        "wait",
        "--format=json",
    ]);
    assert!(
        !String::from_utf8_lossy(&machine.stderr).contains(notice),
        "{machine:?}"
    );
    assert!(machine.status.success(), "{machine:?}");
    let _: Value = serde_json::from_slice(&machine.stdout).expect("search JSON stays parseable");
    let pid = live_pid(&wait_for_daemon(&harness, None));
    harness.json(&["index", "mode", "manual", "--format=json"]);
    wait_for_process_state(pid, false, OBSERVATION_TIMEOUT).unwrap();
    let finite = harness.output(&[
        "search",
        "scratch machine output oracle",
        "--refresh",
        "wait",
    ]);
    assert!(finite.status.success(), "{finite:?}");
    assert!(
        !String::from_utf8_lossy(&finite.stderr).contains(notice),
        "{finite:?}"
    );
    if let Some(pid) = read_lock(harness.root())
        .as_ref()
        .and_then(|lock| json_u32(lock, "pid"))
    {
        wait_for_process_state(pid, false, OBSERVATION_TIMEOUT).unwrap();
    }
    assert_eq!(harness.daemon_status()["running"], false);
}

#[cfg(unix)]
#[test]
fn scratch_rendered_stop_command_stops_the_control_character_root() {
    let _serial = TEST_SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut harness = Harness::new();
    harness.data_root = harness.home().join("scratch\nwith-escape-\u{1b}\n");
    let first = harness.output(&["search", "empty scratch oracle"]);
    let stderr = String::from_utf8_lossy(&first.stderr);
    let stop = stderr
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("ctx --data-root="))
        .expect("stop command");
    let pid = live_pid(&wait_for_daemon(&harness, None));
    let prepared = harness.std_command();
    let mut shell = StdCommand::new("/bin/sh");
    for (name, value) in prepared.get_envs() {
        match value {
            Some(value) => {
                shell.env(name, value);
            }
            None => {
                shell.env_remove(name);
            }
        }
    }
    // Use the exact rendered command with the fixture executable on PATH.
    let bin = harness.home().join("shell-bin");
    fs::create_dir(&bin).unwrap();
    std::os::unix::fs::symlink(&harness.binary, bin.join("ctx")).unwrap();
    shell.env("PATH", &bin).args(["-c", stop]);
    let output = shell.output().unwrap();
    assert!(output.status.success(), "{output:?}");
    wait_for_process_state(pid, false, OBSERVATION_TIMEOUT).unwrap();
    assert_eq!(harness.daemon_status()["running"], false);
    assert!(fs::read_to_string(harness.root().join("config.toml"))
        .unwrap()
        .contains("manual"));
}

#[test]
fn scratch_notice_is_silent_for_a_relative_managed_root() {
    let _serial = TEST_SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let harness = Harness::new();
    let mut command = harness.std_command();
    command
        .current_dir(harness.home())
        .arg("--data-root")
        .arg(harness.root().file_name().unwrap())
        .args(["search", "relative managed oracle"]);
    let child = CapturedChild::spawn(&mut command, harness.home()).unwrap();
    let output = wait_for_output(
        child,
        COMMAND_TIMEOUT,
        &["search", "relative managed oracle"],
    );
    assert!(
        !String::from_utf8_lossy(&output.stderr)
            .contains("Launched a persistent background indexer"),
        "{output:?}"
    );
    let pid = live_pid(&wait_for_daemon(&harness, None));
    harness.json(&["index", "mode", "manual", "--format=json"]);
    wait_for_process_state(pid, false, OBSERVATION_TIMEOUT).unwrap();
}

fn relative_scratch_search(harness: &mut Harness, name: &str) -> Output {
    let mut command = harness.std_command();
    harness.data_root = harness.home().join(name);
    command
        .current_dir(harness.home())
        .arg(format!("--data-root={name}"))
        .args(["search", "relative scratch oracle"]);
    let child = CapturedChild::spawn(&mut command, harness.home()).unwrap();
    wait_for_output(
        child,
        COMMAND_TIMEOUT,
        &["search", "relative scratch oracle"],
    )
}

#[test]
fn scratch_relative_root_is_listed_from_another_directory() {
    let _serial = TEST_SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for foreground in [false, true] {
        let mut harness = Harness::new();
        let foreground_child = if foreground {
            let mut command = harness.std_command();
            harness.data_root = harness.home().join("relative-scratch");
            command.current_dir(harness.home()).args([
                "--data-root=relative-scratch",
                "daemon",
                "run",
                "--format=json",
            ]);
            Some(CapturedChild::spawn(&mut command, harness.home()).unwrap())
        } else {
            relative_scratch_search(&mut harness, "relative-scratch");
            None
        };
        let pid = live_pid(&wait_for_daemon(&harness, None));
        let installation = fs::read_dir(data_root(&harness.temp).join("daemon-installations"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let registrations = installation.join("daemon-quiescence-acks");
        fs::write(registrations.join("malformed.json"), b"not json").unwrap();
        fs::write(
            registrations.join("relative.json"),
            serde_json::to_vec(&json!({
                "schema_version": 1, "registration_id": "legacy", "status": "live",
                "data_root": "legacy-relative", "pid": pid,
            }))
            .unwrap(),
        )
        .unwrap();
        let elsewhere = harness.home().join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        let mut command = harness.std_command();
        let args = ["daemon", "status", "--all", "--format=json"];
        command.current_dir(&elsewhere).args(args);
        let child = CapturedChild::spawn(&mut command, harness.home()).unwrap();
        let listed = success_json(wait_for_output(child, COMMAND_TIMEOUT, &args), &args);
        let rows = listed["daemons"].as_array().unwrap();
        assert_eq!(rows.len(), 3, "{listed:#}");
        assert_eq!(rows[0]["data_root"], json!(harness.root()));
        assert_eq!(rows[0]["daemon"]["live_pid"], pid, "{listed:#}");
        assert_eq!(rows[0]["daemon"]["running"], true, "{listed:#}");
        assert!(rows[1]["data_root"].is_null());
        assert!(rows[1]["daemon"].is_null());
        assert!(rows[1]["error"]
            .as_str()
            .unwrap()
            .contains("parse ctx daemon acknowledgement"));
        assert_eq!(rows[2]["data_root"], "legacy-relative");
        assert!(rows[2]["daemon"].is_null());
        assert!(rows[2]["error"]
            .as_str()
            .unwrap()
            .contains("invalid ctx daemon acknowledgement"));
        assert!(!elsewhere.join("relative-scratch").exists());
        assert!(!elsewhere.join("legacy-relative").exists());
        assert_eq!(
            fs::read(registrations.join("malformed.json")).unwrap(),
            b"not json"
        );
        harness.json(&["index", "mode", "manual", "--format=json"]);
        wait_for_process_state(pid, false, OBSERVATION_TIMEOUT).unwrap();
        if let Some(child) = foreground_child {
            let _ = wait_for_output(child, COMMAND_TIMEOUT, &["daemon", "run"]);
        }
    }
}

#[cfg(unix)]
#[test]
fn scratch_relative_leading_dash_stop_command_works() {
    let _serial = TEST_SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut harness = Harness::new();
    let first = relative_scratch_search(&mut harness, "-scratch-relative");
    let stderr = String::from_utf8_lossy(&first.stderr);
    let stop = stderr
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("ctx --data-root"))
        .expect("stop command");
    let pid = live_pid(&wait_for_daemon(&harness, None));
    let mut shell = StdCommand::new("/bin/sh");
    let prepared = harness.std_command();
    for (name, value) in prepared.get_envs() {
        if let Some(value) = value {
            shell.env(name, value);
        } else {
            shell.env_remove(name);
        }
    }
    let bin = harness.home().join("shell-bin");
    fs::create_dir(&bin).unwrap();
    std::os::unix::fs::symlink(&harness.binary, bin.join("ctx")).unwrap();
    let output = shell
        .current_dir(harness.home())
        .env("PATH", bin)
        .args(["-c", stop])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    wait_for_process_state(pid, false, OBSERVATION_TIMEOUT).unwrap();
    assert_eq!(harness.daemon_status()["running"], false);
}

#[test]
fn abandoned_daemon_lock_and_status_recover_after_binary_relocation() {
    let _serial = TEST_SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for retain_lock in [true, false] {
        let mut harness = Harness::new();
        let old_binary = harness.binary.clone();
        let mut original = harness.spawn(&["daemon", "run", "--format=json"], None);
        let old_pid = original.id();
        assert_eq!(live_pid(&wait_for_daemon(&harness, None)), old_pid);
        let old_lock = read_lock(harness.root()).expect("original daemon lock");

        let new_bin = harness.home().join("relocated-bin");
        fs::create_dir(&new_bin).unwrap();
        harness.binary = new_bin.join(old_binary.file_name().unwrap());
        fs::copy(&old_binary, &harness.binary).unwrap();

        // A second image must leave the live owner alone, even though its
        // reported identity does not match the invoking binary.
        let _ = harness.output(&["daemon", "run", "--format=json"]);
        assert!(process_is_running(old_pid));
        assert_eq!(read_lock(harness.root()).unwrap(), old_lock);

        original
            .terminate()
            .expect("crash and reap original daemon");
        fs::remove_file(&old_binary).unwrap();
        assert_eq!(read_lock(harness.root()).unwrap(), old_lock);
        assert_eq!(old_lock["released"], false);
        assert_eq!(old_lock["lock_protocol"], "advisory-v1");
        let status_path = harness.root().join("daemon/status.json");
        let abandoned_status = fs::read(&status_path).unwrap();
        let status: Value = serde_json::from_slice(&abandoned_status).unwrap();
        assert_eq!(status["status"], "running", "{status:#}");
        assert_eq!(json_u32(&status, "pid"), Some(old_pid));
        if !retain_lock {
            fs::remove_file(harness.root().join("daemon/daemon.lock")).unwrap();
        }

        let stale = harness.daemon_status();
        assert_eq!(stale["running"], false, "{stale:#}");
        assert_eq!(stale["recoverable"], true, "{stale:#}");
        assert_eq!(
            stale["reason"],
            if retain_lock {
                "daemon_lock_stale"
            } else {
                "daemon_status_stale"
            },
            "{stale:#}"
        );
        assert_eq!(fs::read(&status_path).unwrap(), abandoned_status);
        assert_eq!(
            read_lock(harness.root()),
            retain_lock.then_some(old_lock.clone())
        );

        let replacement = harness.spawn(&["daemon", "run", "--format=json"], None);
        let ready = wait_for_daemon(&harness, Some(old_pid));
        assert_eq!(live_pid(&ready), replacement.id(), "{ready:#}");
        assert_replaced_stale_owner(&harness, &old_lock, replacement.id());
        let replacement_lock = read_lock(harness.root()).unwrap();
        assert_eq!(replacement_lock["binary"], json!(harness.binary));
        let replacement_status = read_json_file(&status_path);
        assert_eq!(json_u32(&replacement_status, "pid"), Some(replacement.id()));
        assert_eq!(replacement_status["status"], "running");

        harness.json(&["daemon", "disable", "--format=json"]);
        let output = wait_for_output(replacement, COMMAND_TIMEOUT, &["daemon", "run"]);
        assert!(output.status.success(), "{output:?}");
    }
}

#[test]
fn long_lived_mcp_search_recovers_daemon_after_startup() {
    let _serial = TEST_SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let harness = Harness::new();
    write_codex_session(harness.home(), "long lived mcp recovery oracle");
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
        searched.get("error").is_none() && searched["result"]["isError"].as_bool() != Some(true),
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
fn live_readiness_rejoins_while_the_main_scheduler_is_blocked() {
    let _serial = TEST_SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let harness = Harness::new();
    write_codex_session(harness.home(), "blocked scheduler readiness oracle");
    harness.setup_wait();
    harness.json(&["daemon", "disable", "--format=json"]);

    let block = harness
        .root()
        .join(".block-daemon-main-after-ready-for-test");
    let blocked = harness
        .root()
        .join(".daemon-main-blocked-after-ready-for-test");
    fs::write(&block, b"block\n").unwrap();
    let started = harness.json(&["daemon", "enable", "--format=json"]);
    let pid = json_u32(&started, "pid").expect("daemon pid");
    let deadline = Instant::now() + OBSERVATION_TIMEOUT;
    while !blocked.exists() {
        assert!(
            Instant::now() < deadline,
            "daemon did not reach Ready fence"
        );
        thread::sleep(Duration::from_millis(20));
    }
    let owner = read_lock(harness.root()).expect("daemon owner lock");

    let status_path = harness.root().join("daemon/status.json");
    let mut status = read_json_file(&status_path);
    status["heartbeat_at_ms"] = json!(1);
    fs::write(&status_path, serde_json::to_vec(&status).unwrap()).unwrap();
    fs::remove_file(harness.root().join("daemon/jobs/core-refresh.json")).unwrap();

    let rejoined = harness.json(&["daemon", "enable", "--format=json"]);
    assert_eq!(json_u32(&rejoined, "pid"), Some(pid), "{rejoined:#}");
    assert_eq!(
        read_lock(harness.root()).expect("rejoined owner")["owner_id"],
        owner["owner_id"]
    );

    fs::remove_file(block).unwrap();
    harness.json(&["daemon", "disable", "--format=json"]);
}

#[cfg(unix)]
#[test]
fn active_daemon_work_exits_within_the_process_signal_deadline() {
    let _serial = TEST_SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    for signal in [ShutdownSignal::Terminate, ShutdownSignal::Interrupt] {
        let harness = Harness::new();
        let source = write_codex_session(
            harness.home(),
            &format!("active daemon {signal:?} retained generation oracle"),
        );
        let generation = harness.setup_wait();
        let initial_daemon = wait_for_daemon(&harness, None);
        let initial_pid = live_pid(&initial_daemon);
        wait_for_core_generation(&harness, &generation);
        request_graceful_shutdown(initial_pid).unwrap_or_else(|error| {
            panic!("stop initial daemon {initial_pid} before {signal:?} case: {error}")
        });
        wait_for_process_state(initial_pid, false, Duration::from_secs(2))
            .unwrap_or_else(|error| panic!("initial daemon {initial_pid} did not stop: {error}"));

        let child = harness.spawn(&["daemon", "run", "--force"], None);
        let pid = child.id();
        let foreground_daemon = wait_for_daemon(&harness, Some(initial_pid));
        assert_eq!(live_pid(&foreground_daemon), pid, "{foreground_daemon:#}");

        let pointer_path = harness.root().join("search/lexical/active-generation.json");
        let meta_path = active_generation_meta_path(harness.root(), &generation);
        let manifest_path = generation_manifest_path(harness.root(), &generation);
        let retained_pointer = snapshot_file(&pointer_path);
        let retained_meta = snapshot_file(&meta_path);
        let retained_manifest = snapshot_file(&manifest_path);

        let block = harness
            .root()
            .join(".block-daemon-main-after-ready-for-test");
        let blocked = harness
            .root()
            .join(".daemon-main-blocked-after-ready-for-test");
        fs::write(&block, b"block\n").unwrap();
        let unpublished_query = format!("active daemon {signal:?} unpublished work oracle");
        append_codex_message(
            &source,
            "2026-07-29T12:01:00.000Z",
            "assistant",
            &unpublished_query,
        );
        let mut refresh_wait = harness.spawn(
            &[
                "search",
                &unpublished_query,
                "--provider",
                "codex",
                "--refresh",
                "wait",
                "--format=json",
            ],
            None,
        );
        let marker_result = wait_for_marker(&blocked, "daemon active refresh cycle");
        refresh_wait
            .terminate()
            .expect("cancel and reap refresh wait");
        marker_result.unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(snapshot_file(&pointer_path), retained_pointer);

        let signaled_at = Instant::now();
        request_shutdown(pid, signal)
            .unwrap_or_else(|error| panic!("send {signal:?} to daemon {pid}: {error}"));
        let shutdown_bound = Duration::from_secs(2);
        let output = wait_for_output(child, shutdown_bound, &["daemon", "run", "--signal"]);
        assert!(
            signaled_at.elapsed() <= shutdown_bound,
            "daemon exceeded the {shutdown_bound:?} {signal:?} shutdown bound"
        );
        assert_eq!(
            output.status.code(),
            Some(1),
            "deadline-forced {signal:?} shutdown should report failure:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        assert_eq!(snapshot_file(&pointer_path), retained_pointer);
        assert_eq!(snapshot_file(&meta_path), retained_meta);
        assert_eq!(snapshot_file(&manifest_path), retained_manifest);
        let _ = active_generation_meta_path(harness.root(), &generation);

        let stale = read_lock(harness.root()).expect("deadline-forced daemon lock metadata");
        assert_eq!(json_u32(&stale, "pid"), Some(pid), "{stale:#}");
        assert_eq!(stale["released"], false, "{stale:#}");
        #[cfg(target_os = "linux")]
        assert!(
            linux_daemon_processes(&harness).is_empty(),
            "signal test unexpectedly restarted or leaked a daemon"
        );

        fs::remove_file(&block).expect("release active-work test gate after daemon exit");
        fs::remove_file(&blocked).expect("remove stale active-work test marker");
        harness.best_effort_disable();
        assert!(
            read_lock(harness.root())
                .as_ref()
                .and_then(|lock| json_u32(lock, "pid"))
                .is_none_or(|owner| !process_is_running(owner)),
            "signal test cleanup left a live daemon lock owner"
        );
        #[cfg(target_os = "linux")]
        assert!(
            linux_daemon_processes(&harness).is_empty(),
            "signal test cleanup left a daemon process"
        );
    }
}

#[cfg(unix)]
fn wait_for_marker(path: &Path, description: &str) -> Result<(), String> {
    let deadline = Instant::now() + OBSERVATION_TIMEOUT;
    while !path.exists() {
        if Instant::now() >= deadline {
            return Err(format!(
                "timed out waiting for {description} marker {}",
                path.display()
            ));
        }
        thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}
