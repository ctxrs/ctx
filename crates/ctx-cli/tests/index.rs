mod support;

use support::*;

fn import_ready_history(temp: &TempDir) {
    let fixture = provider_history_fixture("codex-sessions");
    let imported = json_output(ctx(temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &fixture,
        "--format=json",
    ]));
    let generation = imported["sources"][0]["published_generation"]
        .as_str()
        .expect("import should publish a Core generation");
    wait_for_test_lexical_projection(temp, generation);
}

fn persist_stopped_daemon_state(temp: &TempDir, semantic_enabled: bool) {
    ctx(temp)
        .args(["daemon", "disable", "--format=json"])
        .assert()
        .success();
    fs::write(
        data_root(temp).join("config.toml"),
        format!("[daemon]\nenabled = true\n\n[search]\nsemantic = {semantic_enabled}\n"),
    )
    .unwrap();
    let daemon_root = data_root(temp).join("daemon");
    fs::create_dir_all(daemon_root.join("jobs")).unwrap();
    fs::write(
        daemon_root.join("status.json"),
        json!({
            "schema_version": 1,
            "status": "running",
            "pid": 0,
            "started_at_ms": 1,
            "semantic_runtime_active": false,
        })
        .to_string(),
    )
    .unwrap();
}

fn strip_ansi(rendered: &[u8]) -> Vec<u8> {
    let mut stream = anstream::StripStream::new(Vec::new());
    stream.write_all(rendered).unwrap();
    stream.into_inner()
}

fn contains_cursor_up(rendered: &[u8]) -> bool {
    rendered.windows(2).enumerate().any(|(offset, prefix)| {
        if prefix != b"\x1b[" {
            return false;
        }
        let parameters = &rendered[offset + 2..];
        let digits = parameters
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        digits > 0 && parameters.get(digits) == Some(&b'A')
    })
}

fn assert_single_human_diagnosis(
    output: &std::process::Output,
    expected_title: &str,
    expected_action: &str,
) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        stdout.matches(expected_title).count(),
        1,
        "expected exactly one {expected_title:?} diagnosis: {stdout}"
    );
    assert_eq!(
        stdout.matches(expected_action).count(),
        1,
        "expected exactly one {expected_action:?} action: {stdout}"
    );
    assert!(
        !stdout.contains("Error:"),
        "human diagnosis must stay structured: {stdout}"
    );
    assert!(
        stderr.is_empty(),
        "structured human diagnosis must not be forwarded again: {stderr}"
    );
}

#[test]
fn index_watch_and_wait_are_read_only_for_missing_store() {
    let temp = tempdir();

    let watch_machine = ctx(&temp)
        .args([
            "index",
            "watch",
            "--format=jsonl",
            "--interval-seconds",
            "1",
        ])
        .assert()
        .failure()
        .get_output()
        .clone();
    let watch_snapshot: Value = serde_json::from_slice(&watch_machine.stdout).unwrap();
    assert_eq!(watch_snapshot["lexical"]["status"], "unavailable");
    assert_eq!(
        watch_snapshot["lexical"]["reason"],
        "generation_not_published"
    );
    assert_eq!(
        String::from_utf8(watch_machine.stderr).unwrap(),
        "Error: ctx index does not exist yet; run `ctx setup` first\n"
    );

    let wait_machine = ctx(&temp)
        .args(["index", "wait", "--format=json", "--interval-seconds", "1"])
        .assert()
        .failure()
        .get_output()
        .clone();
    let wait_snapshot: Value = serde_json::from_slice(&wait_machine.stdout).unwrap();
    assert_eq!(wait_snapshot["status"], "blocked");
    assert_eq!(
        wait_snapshot["readiness"]["lexical"]["status"],
        "unavailable"
    );
    assert_eq!(
        String::from_utf8(wait_machine.stderr).unwrap(),
        "Error: ctx index does not exist yet; run `ctx setup` first\n"
    );

    let watch_human = ctx(&temp)
        .args(["--color=never", "index", "watch", "--interval-seconds", "1"])
        .assert()
        .failure()
        .get_output()
        .clone();
    assert_single_human_diagnosis(&watch_human, "Search index is not set up", "ctx setup");

    let wait_human = ctx(&temp)
        .args(["--color=never", "index", "wait", "--interval-seconds", "1"])
        .assert()
        .failure()
        .get_output()
        .clone();
    assert_single_human_diagnosis(&wait_human, "Search index is not set up", "ctx setup");
    assert!(
        !data_root(&temp).join("work.sqlite").exists(),
        "readiness observation must not initialize legacy storage"
    );
    assert!(
        !data_root(&temp).join("work.sqlite").exists(),
        "index watch/wait failures must not initialize the store"
    );
}

#[test]
fn index_watch_exits_when_background_indexing_has_terminally_failed() {
    let temp = daemon_test_root();
    import_ready_history(&temp);
    ctx(&temp)
        .args(["daemon", "disable", "--format=json"])
        .assert()
        .success();
    fs::write(
        data_root(&temp).join("config.toml"),
        "[daemon]\nenabled = true\n\n[search]\nsemantic = true\n",
    )
    .unwrap();
    let daemon_root = data_root(&temp).join("daemon");
    fs::create_dir_all(&daemon_root).unwrap();
    fs::write(
        daemon_root.join("status.json"),
        json!({
            "schema_version": 1,
            "status": "failed",
            "pid": 0,
            "finished_at_ms": 1,
            "last_error": "synthetic terminal failure",
            "semantic_runtime_active": false,
        })
        .to_string(),
    )
    .unwrap();

    let output = ctx(&temp)
        .timeout(Duration::from_secs(3))
        .args([
            "index",
            "watch",
            "--format=jsonl",
            "--interval-seconds",
            "1",
        ])
        .assert()
        .failure()
        .get_output()
        .clone();
    let snapshots = String::from_utf8(output.stdout).unwrap();
    let snapshots = snapshots.lines().collect::<Vec<_>>();
    assert_eq!(snapshots.len(), 1, "{snapshots:#?}");
    let status: Value = serde_json::from_str(snapshots[0]).unwrap();
    assert_eq!(status["lexical"]["status"], "ready", "{status:#}");
    assert_eq!(status["semantic"]["status"], "pending", "{status:#}");
    assert_eq!(status["daemon"]["status"], "failed", "{status:#}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(
        stderr,
        "Error: background indexing stopped before the index was ready; run `ctx doctor` for details\n"
    );

    let human = ctx(&temp)
        .timeout(Duration::from_secs(3))
        .args(["--color=never", "index", "watch", "--interval-seconds", "1"])
        .assert()
        .failure()
        .get_output()
        .clone();
    assert_single_human_diagnosis(&human, "Background indexing stopped", "ctx doctor");
}

#[test]
fn index_watch_reports_missing_generation_before_stale_daemon_failure() {
    let temp = tempdir();
    let daemon_root = data_root(&temp).join("daemon");
    fs::create_dir_all(&daemon_root).unwrap();
    fs::write(
        daemon_root.join("status.json"),
        json!({
            "schema_version": 1,
            "status": "failed",
            "pid": 0,
            "finished_at_ms": 1,
            "last_error": "synthetic terminal failure",
            "semantic_runtime_active": false,
        })
        .to_string(),
    )
    .unwrap();

    let output = ctx(&temp)
        .timeout(Duration::from_secs(3))
        .args([
            "index",
            "watch",
            "--format=jsonl",
            "--interval-seconds",
            "1",
        ])
        .assert()
        .failure()
        .get_output()
        .clone();
    let snapshots = String::from_utf8(output.stdout).unwrap();
    let snapshots = snapshots.lines().collect::<Vec<_>>();
    assert_eq!(snapshots.len(), 1, "{snapshots:#?}");
    let status: Value = serde_json::from_str(snapshots[0]).unwrap();
    assert_eq!(status["lexical"]["status"], "unavailable", "{status:#}");
    assert_eq!(status["daemon"]["status"], "failed", "{status:#}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(
        stderr,
        "Error: ctx index does not exist yet; run `ctx setup` first\n"
    );
}

#[test]
fn authoritative_status_reports_stale_daemon_lock_as_recoverable() {
    let temp = tempdir();
    let daemon = data_root(&temp).join("daemon");
    fs::create_dir_all(&daemon).unwrap();
    fs::write(
        daemon.join("daemon.lock"),
        json!({
            "pid": u32::MAX,
            "started_at_ms": 0,
        })
        .to_string(),
    )
    .unwrap();

    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(status["daemon"]["status"], "stale_lock");
    assert_eq!(status["daemon"]["recoverable"], true);
    assert_eq!(status["daemon"]["reason"], "daemon_lock_stale");
    assert!(
        !data_root(&temp).join("work.sqlite").exists(),
        "stale lock reporting must not initialize the store"
    );
}

#[test]
fn index_command_has_only_watch_and_wait_subcommands() {
    let temp = tempdir();
    let help = String::from_utf8(
        ctx(&temp)
            .args(["index", "--help"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone(),
    )
    .unwrap();
    assert!(help.contains("watch"), "{help}");
    assert!(help.contains("wait"), "{help}");
    assert!(!help.contains("\n  status"), "{help}");

    ctx(&temp)
        .args(["index", "status"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unrecognized subcommand 'status'"));
}

#[test]
fn index_wait_lexical_reports_ready_after_import() {
    let temp = daemon_test_root();
    import_ready_history(&temp);

    let wait = json_output(ctx(&temp).args([
        "index",
        "wait",
        "--lexical",
        "--format=json",
        "--timeout-seconds",
        "1",
        "--interval-seconds",
        "1",
    ]));
    assert_eq!(wait["schema_version"], 1);
    assert_eq!(wait["status"], "ready");
    assert_eq!(wait["selection"]["lexical"], true);
    assert_eq!(wait["selection"]["semantic"], false);
    assert_eq!(wait["readiness"]["lexical"]["status"], "ready");
    assert!(
        wait["readiness"]["lexical"]["indexed_items"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert_eq!(wait["local_only"], true);
    assert_eq!(wait["read_only"], true);
}

#[test]
fn index_wait_lexical_accepts_retained_core_after_refresh_failure() {
    let temp = daemon_test_root();
    import_ready_history(&temp);
    persist_stopped_daemon_state(&temp, false);
    fs::write(
        data_root(&temp).join("daemon/jobs/core-refresh.json"),
        json!({
            "status": "failed",
            "request_state": "failed",
            "last_error": "synthetic refresh failure",
        })
        .to_string(),
    )
    .unwrap();

    let wait = json_output(ctx(&temp).args([
        "index",
        "wait",
        "--lexical",
        "--format=json",
        "--timeout-seconds",
        "1",
        "--interval-seconds",
        "1",
    ]));
    assert_eq!(wait["status"], "ready", "{wait:#}");
    assert_eq!(wait["selection"]["lexical"], true, "{wait:#}");
    assert_eq!(wait["readiness"]["lexical"]["status"], "ready");
    assert_eq!(
        wait["readiness"]["refresh"]["reason"],
        "core_refresh_failed"
    );
    assert_eq!(wait["readiness"]["daemon"]["running"], false);
}

#[test]
fn stopped_queued_or_running_refresh_blocks_default_wait_despite_stale_status() {
    let temp = daemon_test_root();
    import_ready_history(&temp);
    persist_stopped_daemon_state(&temp, false);

    for request_state in ["queued", "running"] {
        fs::write(
            data_root(&temp).join("daemon/jobs/core-refresh.json"),
            json!({
                "status": "pending",
                "request_state": request_state,
            })
            .to_string(),
        )
        .unwrap();

        let output = ctx(&temp)
            .timeout(Duration::from_secs(3))
            .args(["index", "wait", "--format=json", "--interval-seconds", "1"])
            .assert()
            .failure()
            .get_output()
            .clone();
        let wait: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(wait["status"], "blocked", "{wait:#}");
        assert_eq!(
            wait["readiness"]["refresh"]["request_state"], request_state,
            "{wait:#}"
        );
        let persisted: Value =
            serde_json::from_slice(&fs::read(data_root(&temp).join("daemon/status.json")).unwrap())
                .unwrap();
        assert_eq!(persisted["status"], "running");
        assert_eq!(wait["readiness"]["daemon"]["status"], "stale_lock");
        assert_eq!(wait["readiness"]["daemon"]["running"], false);
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            "Error: background indexing stopped before the index was ready; run `ctx doctor` for details\n"
        );
    }
}

#[test]
fn stopped_queued_or_running_semantic_blocks_wait_despite_stale_status() {
    let temp = daemon_test_root();
    import_ready_history(&temp);
    persist_stopped_daemon_state(&temp, true);

    for semantic_state in ["queued", "running"] {
        fs::write(
            data_root(&temp).join("daemon/jobs/semantic-index.json"),
            json!({
                "status": semantic_state,
            })
            .to_string(),
        )
        .unwrap();

        let output = ctx(&temp)
            .timeout(Duration::from_secs(3))
            .args([
                "index",
                "wait",
                "--semantic",
                "--format=json",
                "--interval-seconds",
                "1",
            ])
            .assert()
            .failure()
            .get_output()
            .clone();
        let wait: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(wait["status"], "blocked", "{wait:#}");
        assert_eq!(
            wait["readiness"]["daemon"]["jobs"]["semantic_index"]["status"], semantic_state,
            "{wait:#}"
        );
        let persisted: Value =
            serde_json::from_slice(&fs::read(data_root(&temp).join("daemon/status.json")).unwrap())
                .unwrap();
        assert_eq!(persisted["status"], "running");
        assert_eq!(wait["readiness"]["daemon"]["status"], "stale_lock");
        assert_eq!(wait["readiness"]["daemon"]["running"], false);
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            "Error: background indexing stopped before the index was ready; run `ctx doctor` for details\n"
        );
    }
}

#[test]
fn index_wait_default_skips_semantic_when_disabled_after_import() {
    let temp = daemon_test_root();
    import_ready_history(&temp);

    let wait = json_output(ctx(&temp).args([
        "index",
        "wait",
        "--format=json",
        "--timeout-seconds",
        "1",
        "--interval-seconds",
        "1",
    ]));
    assert_eq!(wait["schema_version"], 1);
    assert_eq!(wait["status"], "ready");
    assert_eq!(wait["selection"]["lexical"], true);
    assert_eq!(wait["selection"]["semantic"], false);
    assert_eq!(wait["readiness"]["lexical"]["status"], "ready");
    assert_eq!(wait["readiness"]["semantic"]["enabled"], false);
}

#[test]
fn index_watch_default_skips_semantic_when_disabled_after_import() {
    let temp = daemon_test_root();
    import_ready_history(&temp);

    let output = ctx(&temp)
        .args([
            "--color=always",
            "index",
            "watch",
            "--format=jsonl",
            "--interval-seconds",
            "1",
        ])
        .assert()
        .success()
        .get_output()
        .clone();
    let snapshots = String::from_utf8(output.stdout).unwrap();
    assert!(!snapshots.contains('\u{1b}'), "{snapshots:?}");
    let snapshots = snapshots.lines().collect::<Vec<_>>();
    assert_eq!(snapshots.len(), 1, "{snapshots:#?}");
    let status: Value = serde_json::from_str(snapshots[0]).unwrap();
    assert_eq!(status["lexical"]["status"], "ready");
    assert_eq!(status["semantic"]["enabled"], false);
}

#[test]
fn index_wait_semantic_stays_strict_when_semantic_is_disabled() {
    let temp = daemon_test_root();
    import_ready_history(&temp);

    let output = ctx(&temp)
        .args([
            "index",
            "wait",
            "--semantic",
            "--format=json",
            "--timeout-seconds",
            "1",
            "--interval-seconds",
            "1",
        ])
        .assert()
        .failure()
        .get_output()
        .clone();
    let stdout: Value = serde_json::from_slice(&output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(stdout["schema_version"], 1);
    assert_eq!(stdout["status"], "blocked");
    assert_eq!(stdout["selection"]["lexical"], false);
    assert_eq!(stdout["selection"]["semantic"], true);
    assert_eq!(stdout["readiness"]["semantic"]["enabled"], false);
    assert_eq!(stderr, "Error: semantic indexing is disabled\n");
}

#[test]
fn index_wait_all_stays_strict_when_semantic_is_disabled() {
    let temp = daemon_test_root();
    import_ready_history(&temp);

    let output = ctx(&temp)
        .args([
            "index",
            "wait",
            "--all",
            "--format=json",
            "--timeout-seconds",
            "1",
            "--interval-seconds",
            "1",
        ])
        .assert()
        .failure()
        .get_output()
        .clone();
    let stdout: Value = serde_json::from_slice(&output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(stdout["schema_version"], 1);
    assert_eq!(stdout["status"], "blocked");
    assert_eq!(stdout["selection"]["lexical"], true);
    assert_eq!(stdout["selection"]["semantic"], true);
    assert_eq!(stdout["readiness"]["lexical"]["status"], "ready");
    assert_eq!(stdout["readiness"]["semantic"]["enabled"], false);
    assert_eq!(stderr, "Error: semantic indexing is disabled\n");
}

#[test]
fn semantic_model_cache_missing_is_an_immediate_terminal_wait() {
    let temp = daemon_test_root();
    import_ready_history(&temp);
    ctx(&temp)
        .args(["daemon", "disable", "--format=json"])
        .assert()
        .success();
    fs::write(
        data_root(&temp).join("config.toml"),
        "[daemon]\nenabled = true\n\n[search]\nsemantic = true\n",
    )
    .unwrap();
    let jobs = data_root(&temp).join("daemon/jobs");
    fs::create_dir_all(&jobs).unwrap();
    fs::write(
        jobs.join("semantic-index.json"),
        json!({
            "status": "skipped",
            "reason": "model_cache_missing",
            "last_run_at_ms": 1,
        })
        .to_string(),
    )
    .unwrap();

    let status = json_output(ctx(&temp).args(["status", "--format=json"]));
    assert_eq!(
        status["daemon"]["jobs"]["semantic_index"]["status"], "skipped",
        "{status:#}"
    );
    assert_eq!(
        status["daemon"]["jobs"]["semantic_index"]["reason"], "model_cache_missing",
        "{status:#}"
    );

    let output = ctx(&temp)
        .timeout(Duration::from_secs(3))
        .args([
            "index",
            "wait",
            "--semantic",
            "--format=json",
            "--interval-seconds",
            "1",
        ])
        .assert()
        .failure()
        .get_output()
        .clone();
    let wait: Value = serde_json::from_slice(&output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert_eq!(wait["status"], "blocked", "{wait:#}");
    assert_eq!(wait["selection"]["semantic"], true, "{wait:#}");
    assert_eq!(
        wait["readiness"]["daemon"]["jobs"]["semantic_index"]["reason"], "model_cache_missing",
        "{wait:#}"
    );
    assert_eq!(
        stderr,
        "Error: semantic indexing is skipped because the local embedding model cache is missing\n"
    );
    assert!(!stderr.contains("timed out"), "{stderr}");

    let human = ctx(&temp)
        .timeout(Duration::from_secs(3))
        .args([
            "--color=never",
            "index",
            "wait",
            "--semantic",
            "--interval-seconds",
            "1",
        ])
        .assert()
        .failure()
        .get_output()
        .clone();
    assert_single_human_diagnosis(&human, "Semantic search needs attention", "ctx doctor");
}

#[test]
fn human_wait_renders_the_lightweight_readiness_document() {
    let temp = daemon_test_root();
    import_ready_history(&temp);

    let wait = ctx(&temp)
        .args([
            "--color=never",
            "index",
            "wait",
            "--lexical",
            "--timeout-seconds",
            "1",
            "--interval-seconds",
            "1",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let rendered = String::from_utf8(wait).unwrap();
    assert!(
        rendered.starts_with("✓ Your history is searchable\n")
            || rendered.starts_with("OK Your history is searchable\n"),
        "{rendered}"
    );
    assert!(rendered.contains("\nProcessed  "));
    assert!(rendered.contains("\nSessions   "));
    assert!(rendered.contains("\nRecords    "));
    assert!(rendered.contains("\nSemantic search  Off\n"));
    assert!(!rendered.contains("lexical_status:"));
    assert!(!rendered.contains('\u{1b}'));
}

#[test]
fn human_wait_semantic_disabled_renders_a_truthful_blocked_snapshot() {
    let temp = daemon_test_root();
    import_ready_history(&temp);

    let output = ctx(&temp)
        .args([
            "--color=never",
            "index",
            "wait",
            "--semantic",
            "--timeout-seconds",
            "1",
            "--interval-seconds",
            "1",
        ])
        .assert()
        .failure()
        .get_output()
        .clone();
    assert_single_human_diagnosis(
        &output,
        "Semantic indexing is blocked",
        "ctx setup --semantic",
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();

    assert!(
        stdout.starts_with("✗ Semantic indexing is blocked\n")
            || stdout.starts_with("ERROR Semantic indexing is blocked\n"),
        "{stdout}"
    );
    assert!(stdout.contains("Keyword search remains available."));
    assert!(stdout.contains("\nKeyword search\n"));
    assert!(stdout.contains("\nSemantic search\nStatus  Off\n"));
    assert!(stdout.ends_with("\nNext\n  ctx setup --semantic\n"));
    assert!(!stdout.contains("Your history is searchable"));
    assert!(!stdout.contains("ctx doctor"));
    assert!(stderr.is_empty(), "{stderr}");
}

#[test]
fn human_wait_stops_when_selected_semantic_work_has_no_running_daemon() {
    let temp = daemon_test_root();
    import_ready_history(&temp);
    fs::write(
        data_root(&temp).join("config.toml"),
        "[daemon]\nenabled = false\n\n[search]\nsemantic = true\n",
    )
    .unwrap();

    let output = ctx(&temp)
        .timeout(Duration::from_secs(3))
        .args([
            "--color=never",
            "index",
            "wait",
            "--semantic",
            "--timeout-seconds",
            "1",
            "--interval-seconds",
            "1",
        ])
        .assert()
        .failure()
        .get_output()
        .clone();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();

    let searchable_frames = stdout.matches("Your history is searchable").count();
    assert_eq!(
        stdout.matches("Semantic search").count(),
        searchable_frames,
        "{stdout}"
    );
    assert!((1..=2).contains(&searchable_frames), "{stdout}");
    assert!(stdout.contains("Your history is searchable"), "{stdout}");
    assert!(stdout.contains("Embedded"));
    assert!(stdout.contains("Background indexing stopped"), "{stdout}");
    assert!(!stdout.contains("Throughput"));
    assert!(!stdout.contains("Remaining"));
    assert!(stderr.is_empty(), "{stderr}");
}

#[test]
fn forced_color_on_a_pipe_adds_only_sgr_not_cursor_motion() {
    let temp = daemon_test_root();
    import_ready_history(&temp);

    let plain = ctx(&temp)
        .args(["--color=never", "index", "watch", "--interval-seconds", "1"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let automatic = ctx(&temp)
        .args(["index", "watch", "--interval-seconds", "1"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let styled = ctx(&temp)
        .args([
            "--color=always",
            "index",
            "watch",
            "--interval-seconds",
            "1",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    assert_eq!(automatic, plain);
    assert!(!automatic.contains(&b'\x1b'));
    assert!(styled.windows(2).any(|bytes| bytes == b"\x1b["));
    assert_eq!(strip_ansi(&styled), plain);
    assert!(!contains_cursor_up(&styled));
    assert!(!styled.contains(&b'\r'));
    assert!(!styled.windows(4).any(|bytes| bytes == b"\x1b[2K"));
}
