mod support;

use support::{ctx, fs, json, json_output, Duration, Instant, TempDir, Value};

fn wait_for_daemon_status(
    temp: &TempDir,
    expected_status: &str,
    expected_running: bool,
    trigger_command: &str,
) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last_status = Value::Null;
    while Instant::now() < deadline {
        last_status = json_output(ctx(temp).args(["daemon", "status", "--format=json"]));
        let daemon = &last_status["daemon"];
        if daemon["status"] == expected_status
            && daemon["running"] == expected_running
            && daemon["trigger_command"] == trigger_command
        {
            return last_status;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "daemon did not reach status={expected_status:?}, running={expected_running}, trigger={trigger_command:?}: {last_status:#}"
    );
}

fn assert_daemon_process_running(pid: u32) {
    assert!(pid > 0, "daemon status must report a positive process id");
    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn kill(pid: i32, signal: i32) -> i32;
        }
        assert_eq!(
            unsafe { kill(pid as i32, 0) },
            0,
            "daemon pid {pid} was not an active process"
        );
    }
}

fn write_codex_setup_session(temp: &TempDir) {
    let sessions = temp
        .path()
        .join(".codex")
        .join("sessions")
        .join("2026/06/24");
    fs::create_dir_all(&sessions).unwrap();
    fs::write(
        sessions.join("rollout-2026-06-24T10-00-00-codex-session-setup.jsonl"),
        concat!(
            r#"{"timestamp":"2026-06-24T10:00:00.000Z","type":"session_meta","payload":{"id":"codex-session-setup","timestamp":"2026-06-24T10:00:00.000Z","cwd":"/repo/app","originator":"codex-cli","cli_version":"0.200.0","source":"cli","model_provider":"openai"}}"#,
            "\n",
            r#"{"timestamp":"2026-06-24T10:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"setup should import"}]}}"#,
            "\n"
        ),
    )
    .unwrap();
}

fn write_active_daemon_upgrade_handoff(temp: &TempDir) {
    let daemon_root = temp.path().join("daemon");
    fs::create_dir_all(&daemon_root).unwrap();
    fs::write(
        daemon_root.join("upgrade-handoff.json"),
        json!({
            "schema_version": 1,
            "handoff_id": "ua_machine_readable_contract",
            "phase": "ready",
            "owner_pid": std::process::id(),
            "helper_pid": null,
            "updated_at_ms": 0,
        })
        .to_string(),
    )
    .unwrap();
}

fn assert_no_daemon_autostart_mutation(temp: &TempDir) {
    assert!(!temp.path().join("daemon/status.json").exists());
    assert!(!temp.path().join("daemon/upgrade-restart-requests").exists());
}

#[path = "setup_sources_import/import_orchestration.rs"]
mod import_orchestration;
#[path = "setup_sources_import/lifecycle.rs"]
mod lifecycle;
#[path = "setup_sources_import/source_inventory.rs"]
mod source_inventory;
