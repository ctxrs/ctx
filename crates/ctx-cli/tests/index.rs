mod support;

use support::*;

fn import_ready_history(temp: &TempDir) {
    let fixture = provider_history_fixture("codex-sessions");
    json_output(ctx(temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &fixture,
        "--format=json",
    ]));
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

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    target_env = "gnu"
))]
fn write_fake_semantic_model_cache(cache_root: &Path) {
    let model_root = cache_root.join("models--intfloat--multilingual-e5-small");
    let snapshot = model_root
        .join("snapshots")
        .join("614241f622f53c4eeff9890bdc4f31cfecc418b3");
    fs::create_dir_all(&snapshot).unwrap();
    for (file, size) in [
        ("onnx/model.onnx", 470_268_510),
        ("tokenizer.json", 17_082_730),
        ("config.json", 655),
        ("special_tokens_map.json", 167),
        ("tokenizer_config.json", 443),
    ] {
        let path = snapshot.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::File::create(path).unwrap().set_len(size).unwrap();
    }
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    target_env = "gnu"
))]
fn remove_semantic_cache_env(command: &mut Command) {
    command.env_remove("CTX_SEMANTIC_CACHE_DIR");
    command.env_remove("FASTEMBED_CACHE_DIR");
    command.env_remove("HF_HOME");
    command.env_remove("HF_HUB_CACHE");
    command.env_remove("XDG_CACHE_HOME");
}

#[test]
fn index_status_and_watch_are_read_only_for_missing_store() {
    let temp = tempdir();

    let status =
        json_output(ctx(&temp).args(["--color=always", "index", "status", "--format=json"]));
    assert_eq!(status["schema_version"], 2);
    assert_eq!(status["initialized"], false);
    assert_eq!(status["lexical"]["status"], "missing");
    assert_eq!(status["local_only"], true);
    assert_eq!(status["read_only"], true);
    assert!(
        !temp.path().join("work.sqlite").exists(),
        "index status must not initialize the store"
    );

    let stderr = failure_stderr(ctx(&temp).args([
        "index",
        "watch",
        "--format=jsonl",
        "--interval-seconds",
        "1",
    ]));
    assert!(stderr.contains("ctx index does not exist yet"), "{stderr}");
    assert!(
        !temp.path().join("work.sqlite").exists(),
        "index watch failure must not initialize the store"
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
    assert!(
        stderr.contains(
            "background indexing stopped before the index was ready; run `ctx doctor` for details"
        ),
        "{stderr}"
    );
}

#[test]
fn index_status_reports_stale_daemon_lock_as_recoverable() {
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

    let status = json_output(ctx(&temp).args(["index", "status", "--format=json"]));
    assert_eq!(status["daemon"]["status"], "stale_lock");
    assert_eq!(status["daemon"]["recoverable"], true);
    assert_eq!(status["daemon"]["reason"], "daemon_lock_stale");
    assert!(
        !temp.path().join("work.sqlite").exists(),
        "stale lock reporting must not initialize the store"
    );
}

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    target_env = "gnu"
))]
#[test]
fn index_status_omits_transient_semantic_cache_policy_fields() {
    let temp = tempdir();
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    write_fake_semantic_model_cache(&workspace.join(".fastembed_cache"));

    let mut current_dir_command = ctx(&temp);
    current_dir_command.current_dir(&workspace);
    remove_semantic_cache_env(&mut current_dir_command);
    let status = json_output(current_dir_command.args(["index", "status", "--format=json"]));
    assert!(
        status["semantic"].get("model_cache_available").is_none(),
        "{status:#}"
    );
    assert!(
        status["semantic"].get("embed_policy").is_none(),
        "{status:#}"
    );

    let temp = tempdir();
    let hf_home = temp.path().join("hf-home");
    write_fake_semantic_model_cache(&hf_home.join("hub"));
    let mut hf_home_command = ctx(&temp);
    remove_semantic_cache_env(&mut hf_home_command);
    hf_home_command.env("HF_HOME", &hf_home);
    let status = json_output(hf_home_command.args(["index", "status", "--format=json"]));
    assert!(
        status["semantic"].get("model_cache_available").is_none(),
        "{status:#}"
    );
    assert!(
        status["semantic"].get("embed_policy").is_none(),
        "{status:#}"
    );
}

#[test]
fn index_wait_lexical_reports_ready_after_import() {
    let temp = daemon_test_root();
    import_ready_history(&temp);

    let status = json_output(ctx(&temp).args(["index", "status", "--format=json"]));
    assert_eq!(status["initialized"], true);
    assert_eq!(status["lexical"]["status"], "ready");
    assert!(status["lexical"]["indexed_items"].as_u64().unwrap() > 0);

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
    assert_eq!(wait["index"]["lexical"]["status"], "ready");
    assert_eq!(wait["local_only"], true);
    assert_eq!(wait["read_only"], true);
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
    assert_eq!(wait["index"]["lexical"]["status"], "ready");
    assert_eq!(wait["index"]["semantic"]["enabled"], false);
    assert_eq!(wait["index"]["semantic"]["config_source"], "default");
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
    assert_eq!(status["semantic"]["config_source"], "default");
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
    assert_eq!(stdout["index"]["semantic"]["enabled"], false);
    assert!(stderr.contains("semantic indexing is disabled"), "{stderr}");
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
    assert_eq!(stdout["index"]["lexical"]["status"], "ready");
    assert_eq!(stdout["index"]["semantic"]["enabled"], false);
    assert!(stderr.contains("semantic indexing is disabled"), "{stderr}");
}

#[test]
fn human_status_and_wait_share_the_ready_document() {
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
    let status = ctx(&temp)
        .args(["--color=never", "index", "status"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    assert_eq!(status, wait);
    let rendered = String::from_utf8(status).unwrap();
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
fn human_wait_blocked_renders_the_final_searchable_snapshot() {
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
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();

    assert!(
        stdout.starts_with("✓ Your history is searchable\n")
            || stdout.starts_with("OK Your history is searchable\n"),
        "{stdout}"
    );
    assert!(stdout.contains("Semantic search  Off"));
    assert!(stderr.contains("semantic indexing is disabled"), "{stderr}");
}

#[test]
fn human_wait_timeout_renders_the_final_active_snapshot() {
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

    assert!(stdout.matches("Semantic search").count() >= 2, "{stdout}");
    assert!(stdout.contains("Your history is searchable"), "{stdout}");
    assert!(stdout.contains("Embedded"));
    assert!(stdout.contains("Throughput"));
    assert!(stdout.contains("Remaining"));
    assert!(
        stderr.contains("timed out before indexing was ready"),
        "{stderr}"
    );
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
