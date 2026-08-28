use super::*;

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
        let _ = refresh_wait.kill();
        let _ = refresh_wait.wait_with_output();
        marker_result.unwrap_or_else(|error| panic!("{error}"));

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
