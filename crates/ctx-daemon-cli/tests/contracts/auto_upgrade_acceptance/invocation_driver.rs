use super::*;

fn configure_automatic_upgrades(temp: &tempfile::TempDir, indexing_mode: &str) {
    let root = data_root(temp);
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("config.toml"),
        format!("[indexing]\nmode = \"{indexing_mode}\"\n\n[upgrade]\nauto = \"apply\"\n"),
    )
    .unwrap();
}

#[test]
fn manual_indexing_command_launches_a_sanitized_detached_worker() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let binary = managed_hook_candidate(&temp, "ia_manual_invocation_launch");
    let receipt = temp.path().join("automatic-worker.receipt");
    configure_automatic_upgrades(&temp, "auto");

    let output = managed_release_env(
        ctx_from_binary(&temp, &binary).args(["index", "mode", "manual", "--format=json"]),
        &release,
        &binary,
    )
    .env("CTX_AUTOMATIC_UPGRADE_WORKER_RECEIPT_FOR_TESTS", &receipt)
    .output()
    .unwrap();

    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice::<Value>(&output.stdout).unwrap();
    assert!(output.stderr.is_empty(), "{output:?}");
    wait_for(
        "detached automatic worker receipt",
        Duration::from_secs(10),
        || receipt.exists(),
    );
    assert_eq!(fs::read_to_string(receipt).unwrap(), "started\n");
    assert!(!scheduler_state_path(&binary).exists());
    assert!(fs::read_to_string(data_root(&temp).join("config.toml"))
        .unwrap()
        .contains("mode = \"manual\""));
}

#[test]
fn source_refresh_only_mode_uses_the_invocation_driver() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let binary = managed_hook_candidate(&temp, "ia_source_refresh_invocation_launch");
    let receipt = temp.path().join("automatic-worker.receipt");
    let root = data_root(&temp);
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("config.toml"),
        "[indexing]\nmode = \"auto\"\n\n[daemon]\nmode = \"source-refresh-only\"\n\n[upgrade]\nauto = \"apply\"\n",
    )
    .unwrap();

    let output = managed_release_env(
        ctx_from_binary(&temp, &binary).args(["doctor", "--format=json"]),
        &release,
        &binary,
    )
    .env("CTX_AUTOMATIC_UPGRADE_WORKER_RECEIPT_FOR_TESTS", &receipt)
    .output()
    .unwrap();

    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice::<Value>(&output.stdout).unwrap();
    assert!(output.stderr.is_empty(), "{output:?}");
    wait_for(
        "source-refresh-only automatic worker receipt",
        Duration::from_secs(10),
        || receipt.exists(),
    );
    assert_eq!(fs::read_to_string(receipt).unwrap(), "started\n");
    assert!(!scheduler_state_path(&binary).exists());
}

#[test]
fn live_switch_to_source_refresh_only_terminalizes_a_prepared_upgrade() {
    let temp = tempdir();
    let release = fake_release(&temp, FIXTURE_TARGET_VERSION);
    let binary = managed_hook_candidate(&temp, "ia_source_refresh_live_switch");
    configure_automatic_upgrades(&temp, "auto");
    let root = data_root(&temp);
    let block = root.join(".block-daemon-main-after-ready-for-test");
    let blocked = root.join(".daemon-main-blocked-after-ready-for-test");
    fs::write(&block, b"block\n").unwrap();

    let command = managed_daemon(&temp, &release, &binary);
    let child = spawn_persistent_daemon(&command);
    wait_for(
        "daemon Ready fence before automatic upgrade preparation",
        Duration::from_secs(30),
        || blocked.exists(),
    );

    fs::write(
        root.join("config.toml"),
        "[indexing]\nmode = \"auto\"\n\n[daemon]\nmode = \"source-refresh-only\"\n\n[upgrade]\nauto = \"apply\"\n",
    )
    .unwrap();
    fs::remove_file(&block).unwrap();

    wait_for(
        "prepared automatic upgrade cancellation",
        Duration::from_secs(30),
        || {
            fs::read(scheduler_state_path(&binary))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .is_some_and(|state| {
                    state["status"] == "error"
                        && state["error"].as_str().is_some_and(|error| {
                            error.contains("mode changed to source-refresh-only")
                        })
                })
        },
    );

    let output = stop_persistent_daemon(child);
    assert!(output.status.success(), "{output:?}");
    let state: Value =
        serde_json::from_slice(&fs::read(scheduler_state_path(&binary)).unwrap()).unwrap();
    assert_eq!(state["status"], "error");
    assert!(state["next_retry_unix_s"].as_u64().is_some(), "{state:#}");
}

#[test]
fn failed_eligible_command_does_not_launch_an_automatic_worker() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let binary = managed_hook_candidate(&temp, "ia_failed_invocation_no_launch");
    let receipt = temp.path().join("automatic-worker.receipt");
    configure_automatic_upgrades(&temp, "manual");

    let output = managed_release_env(
        ctx_from_binary(&temp, &binary).args(["show", "session", "01234567"]),
        &release,
        &binary,
    )
    .env("CTX_AUTOMATIC_UPGRADE_WORKER_RECEIPT_FOR_TESTS", &receipt)
    .output()
    .unwrap();

    assert!(!output.status.success(), "{output:?}");
    std::thread::sleep(Duration::from_millis(300));
    assert!(!receipt.exists());
    assert!(!scheduler_state_path(&binary).exists());
}

#[test]
fn manual_indexing_worker_applies_through_the_shared_scheduler() {
    let temp = tempdir();
    let mut release = fake_release(&temp, FIXTURE_TARGET_VERSION);
    let binary = managed_hook_candidate(&temp, "ia_manual_invocation_apply");
    patch_release_artifact_with_next_ctx(&mut release, &binary, FIXTURE_TARGET_VERSION);
    configure_automatic_upgrades(&temp, "manual");

    managed_release_env(
        ctx_from_binary(&temp, &binary).args(["upgrade", "--automatic-worker"]),
        &release,
        &binary,
    )
    .assert()
    .success()
    .stdout("")
    .stderr("");

    let marker: Value =
        serde_json::from_slice(&fs::read(install_marker_path(&binary)).unwrap()).unwrap();
    let state: Value =
        serde_json::from_slice(&fs::read(scheduler_state_path(&binary)).unwrap()).unwrap();
    assert_eq!(marker["version"], FIXTURE_TARGET_VERSION);
    assert_eq!(state["status"], "applied");
    assert_eq!(state["attempt_source"], "automatic");
}

#[test]
fn automatic_worker_terminalizes_and_releases_handoff_when_policy_reload_fails() {
    let temp = tempdir();
    let mut release = fake_release(&temp, FIXTURE_TARGET_VERSION);
    let binary = managed_hook_candidate(&temp, "ia_worker_reload_failure");
    patch_release_artifact_with_next_ctx(&mut release, &binary, FIXTURE_TARGET_VERSION);
    configure_automatic_upgrades(&temp, "manual");
    let pause = temp.path().join("automatic-quiescence.pause");

    let mut prepared = ctx_from_binary(&temp, &binary);
    prepared.args(["upgrade", "--automatic-worker"]);
    managed_release_env(&mut prepared, &release, &binary)
        .env("CTX_UPGRADE_PAUSE_AFTER_QUIESCENCE_FOR_TESTS", &pause);
    let mut command = std_command_from_assert(&prepared);
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    wait_for(
        "automatic worker quiescence pause",
        Duration::from_secs(15),
        || pause.exists(),
    );
    fs::write(data_root(&temp).join("config.toml"), "[upgrade\n").unwrap();
    fs::write(pause.with_extension("continue"), b"continue\n").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success(), "{output:?}");

    let state: Value =
        serde_json::from_slice(&fs::read(scheduler_state_path(&binary)).unwrap()).unwrap();
    assert_eq!(state["status"], "error");
    assert!(state["next_retry_unix_s"].as_u64().is_some(), "{state:#}");
    let handoff: Value = serde_json::from_slice(
        &fs::read(data_root(&temp).join("daemon").join("upgrade-handoff.json")).unwrap(),
    )
    .unwrap();
    assert!(
        matches!(handoff["phase"].as_str(), Some("completed" | "aborted")),
        "{handoff:#}"
    );
}

#[test]
fn automatic_worker_terminalizes_staged_state_when_handoff_begin_fails() {
    let temp = tempdir();
    let mut release = fake_release(&temp, FIXTURE_TARGET_VERSION);
    let binary = managed_hook_candidate(&temp, "ia_worker_handoff_failure");
    patch_release_artifact_with_next_ctx(&mut release, &binary, FIXTURE_TARGET_VERSION);
    configure_automatic_upgrades(&temp, "manual");
    let daemon_root = data_root(&temp).join("daemon");
    fs::create_dir_all(&daemon_root).unwrap();
    fs::set_permissions(&daemon_root, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(daemon_root.join("upgrade-handoff.json"), b"{corrupt").unwrap();

    managed_release_env(
        ctx_from_binary(&temp, &binary).args(["upgrade", "--automatic-worker"]),
        &release,
        &binary,
    )
    .assert()
    .failure();

    let state: Value =
        serde_json::from_slice(&fs::read(scheduler_state_path(&binary)).unwrap()).unwrap();
    assert_eq!(state["status"], "error");
    assert!(state["next_retry_unix_s"].as_u64().is_some(), "{state:#}");
}

#[test]
fn automatic_worker_releases_handoff_when_applied_state_write_fails() {
    let temp = tempdir();
    let mut release = fake_release(&temp, FIXTURE_TARGET_VERSION);
    let binary = managed_hook_candidate(&temp, "ia_worker_applied_state_failure");
    patch_release_artifact_with_next_ctx(&mut release, &binary, FIXTURE_TARGET_VERSION);
    configure_automatic_upgrades(&temp, "manual");

    managed_release_env(
        ctx_from_binary(&temp, &binary).args(["upgrade", "--automatic-worker"]),
        &release,
        &binary,
    )
    .env("CTX_UPGRADE_FAIL_APPLIED_STATE_WRITE_FOR_TESTS", "1")
    .assert()
    .failure();

    let marker: Value =
        serde_json::from_slice(&fs::read(install_marker_path(&binary)).unwrap()).unwrap();
    assert_eq!(marker["version"], FIXTURE_TARGET_VERSION);
    let state: Value =
        serde_json::from_slice(&fs::read(scheduler_state_path(&binary)).unwrap()).unwrap();
    assert_eq!(state["status"], "error");
    assert!(
        state["error"]
            .as_str()
            .is_some_and(|error| error.contains("injected applied-state write failure")),
        "{state:#}"
    );
    let handoff: Value = serde_json::from_slice(
        &fs::read(data_root(&temp).join("daemon").join("upgrade-handoff.json")).unwrap(),
    )
    .unwrap();
    assert!(
        matches!(handoff["phase"].as_str(), Some("completed" | "aborted")),
        "{handoff:#}"
    );
}
