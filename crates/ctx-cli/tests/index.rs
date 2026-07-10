mod support;

use support::*;

fn write_fake_semantic_model_cache(cache_root: &Path) {
    let model_root = cache_root.join("models--intfloat--multilingual-e5-small");
    let snapshot = model_root.join("snapshots").join("test-snapshot");
    fs::create_dir_all(model_root.join("refs")).unwrap();
    fs::create_dir_all(&snapshot).unwrap();
    fs::write(model_root.join("refs").join("main"), "test-snapshot\n").unwrap();
    for file in [
        "onnx/model.onnx",
        "tokenizer.json",
        "config.json",
        "special_tokens_map.json",
        "tokenizer_config.json",
    ] {
        let path = snapshot.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "x").unwrap();
    }
}

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

    let status = json_output(ctx(&temp).args(["index", "status", "--json"]));
    assert_eq!(status["schema_version"], 1);
    assert_eq!(status["initialized"], false);
    assert_eq!(status["lexical"]["status"], "missing");
    assert_eq!(status["local_only"], true);
    assert_eq!(status["read_only"], true);
    assert!(
        !temp.path().join("work.sqlite").exists(),
        "index status must not initialize the store"
    );

    let stderr =
        failure_stderr(ctx(&temp).args(["index", "watch", "--json", "--interval-seconds", "1"]));
    assert!(stderr.contains("ctx index does not exist yet"), "{stderr}");
    assert!(
        !temp.path().join("work.sqlite").exists(),
        "index watch failure must not initialize the store"
    );
}

#[test]
fn index_status_reports_stale_daemon_lock_as_recoverable() {
    let temp = tempdir();
    let daemon = temp.path().join("daemon");
    fs::create_dir_all(&daemon).unwrap();
    fs::write(
        daemon.join("daemon.lock"),
        json!({
            "pid": 0,
            "started_at_ms": 0,
        })
        .to_string(),
    )
    .unwrap();

    let status = json_output(ctx(&temp).args(["index", "status", "--json"]));
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
fn index_status_discovers_existing_semantic_model_cache_dirs() {
    let temp = tempdir();
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    write_fake_semantic_model_cache(&workspace.join(".fastembed_cache"));

    let mut current_dir_command = ctx(&temp);
    current_dir_command.current_dir(&workspace);
    remove_semantic_cache_env(&mut current_dir_command);
    let status = json_output(current_dir_command.args(["index", "status", "--json"]));
    assert_eq!(status["semantic"]["model_cache_available"], true);

    let temp = tempdir();
    let hf_home = temp.path().join("hf-home");
    write_fake_semantic_model_cache(&hf_home.join("hub"));
    let mut hf_home_command = ctx(&temp);
    remove_semantic_cache_env(&mut hf_home_command);
    hf_home_command.env("HF_HOME", &hf_home);
    let status = json_output(hf_home_command.args(["index", "status", "--json"]));
    assert_eq!(status["semantic"]["model_cache_available"], true);
}

#[test]
fn index_wait_lexical_reports_ready_after_import() {
    let temp = tempdir();
    let fixture = provider_history_fixture("codex-sessions");
    json_output(ctx(&temp).args([
        "import",
        "--provider",
        "codex",
        "--path",
        &fixture,
        "--json",
    ]));

    let status = json_output(ctx(&temp).args(["index", "status", "--json"]));
    assert_eq!(status["initialized"], true);
    assert_eq!(status["lexical"]["status"], "ready");
    assert!(status["lexical"]["indexed_items"].as_u64().unwrap() > 0);

    let wait = json_output(ctx(&temp).args([
        "index",
        "wait",
        "--lexical",
        "--json",
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
