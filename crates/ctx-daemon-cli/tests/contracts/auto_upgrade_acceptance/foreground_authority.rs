use super::*;

pub(super) fn configure_automatic_upgrades(temp: &tempfile::TempDir, indexing_mode: &str) {
    let root = data_root(temp);
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("config.toml"),
        format!("[indexing]\nmode = \"{indexing_mode}\"\n\n[upgrade]\nauto = \"apply\"\n"),
    )
    .unwrap();
}

#[test]
fn foreground_commands_leave_upgrade_authority_untouched() {
    for (case, config, machine_output) in [
        (
            "automatic-full",
            "[indexing]\nmode = \"auto\"\n\n[upgrade]\nauto = \"apply\"\n",
            false,
        ),
        (
            "manual",
            "[indexing]\nmode = \"manual\"\n\n[upgrade]\nauto = \"apply\"\n",
            true,
        ),
        (
            "source-refresh-only",
            "[indexing]\nmode = \"auto\"\n\n[daemon]\nmode = \"source-refresh-only\"\n\n[upgrade]\nauto = \"apply\"\n",
            true,
        ),
    ] {
        let temp = tempdir();
        let release = fake_release(&temp, "9.9.9");
        let binary = managed_hook_candidate(&temp, &format!("ia_foreground_{case}"));
        let root = data_root(&temp);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("config.toml"), config).unwrap();
        let before = fs::read(&binary).unwrap();
        let transaction = installation_sibling(&binary, "upgrade-install-transaction.json");

        let mut command = ctx_from_binary(&temp, &binary);
        command.arg("sources");
        if machine_output {
            command.arg("--format=json");
        }
        managed_release_env(&mut command, &release, &binary)
            .assert()
            .success();

        assert_eq!(fs::read(&binary).unwrap(), before, "{case}");
        assert!(!scheduler_state_path(&binary).exists(), "{case}");
        assert!(!transaction.exists(), "{case}");
    }
}

#[test]
fn automatic_worker_process_protocol_is_not_available() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let binary = managed_hook_candidate(&temp, "ia_no_automatic_worker_protocol");
    let before = fs::read(&binary).unwrap();
    let transaction = installation_sibling(&binary, "upgrade-install-transaction.json");

    managed_release_env(
        ctx_from_binary(&temp, &binary).args(["upgrade", "--automatic-worker"]),
        &release,
        &binary,
    )
    .assert()
    .failure();

    assert_eq!(fs::read(&binary).unwrap(), before);
    assert!(!scheduler_state_path(&binary).exists());
    assert!(!transaction.exists());
}

#[test]
fn live_switch_to_source_refresh_only_terminalizes_a_prepared_upgrade() {
    let temp = tempdir();
    let mut release = fake_release(&temp, FIXTURE_TARGET_VERSION);
    let binary = managed_hook_candidate(&temp, "ia_source_refresh_live_switch");
    patch_release_artifact_with_next_ctx(&mut release, &binary, FIXTURE_TARGET_VERSION);
    configure_automatic_upgrades(&temp, "auto");
    let root = data_root(&temp);
    let pause = temp.path().join("source-refresh-before-apply");
    let binary_before = fs::read(&binary).unwrap();
    let marker = install_marker_path(&binary);
    let marker_before = fs::read(&marker).unwrap();
    let transaction = installation_sibling(&binary, "upgrade-install-transaction.json");

    let mut command = managed_daemon(&temp, &release, &binary);
    command
        .env("CTX_DAEMON_AUTOSTART_OFF", "1")
        .env("CTX_UPGRADE_PAUSE_AFTER_QUIESCENCE_FOR_TESTS", &pause);
    let child = spawn_persistent_daemon(&command);
    wait_for(
        "automatic upgrade quiescence before source-refresh-only switch",
        Duration::from_secs(30),
        || pause.exists(),
    );

    fs::write(
        root.join("config.toml"),
        "[indexing]\nmode = \"auto\"\n\n[daemon]\nmode = \"source-refresh-only\"\n\n[upgrade]\nauto = \"apply\"\n",
    )
    .unwrap();
    fs::write(pause.with_extension("continue"), b"continue\n").unwrap();

    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let state: Value =
        serde_json::from_slice(&fs::read(scheduler_state_path(&binary)).unwrap()).unwrap();
    assert_eq!(state["status"], "disabled", "{state:#}");
    assert!(state["next_check_unix_s"].as_u64().is_some(), "{state:#}");
    assert_eq!(fs::read(&binary).unwrap(), binary_before);
    assert_eq!(fs::read(&marker).unwrap(), marker_before);
    assert!(!transaction.exists());
}

#[test]
fn live_daemon_disablement_terminalizes_a_prepared_upgrade() {
    let temp = tempdir();
    let mut release = fake_release(&temp, FIXTURE_TARGET_VERSION);
    let binary = managed_hook_candidate(&temp, "ia_daemon_disable_live_switch");
    patch_release_artifact_with_next_ctx(&mut release, &binary, FIXTURE_TARGET_VERSION);
    configure_automatic_upgrades(&temp, "auto");
    let root = data_root(&temp);
    let pause = temp.path().join("daemon-disable-before-apply");
    let binary_before = fs::read(&binary).unwrap();
    let marker = install_marker_path(&binary);
    let marker_before = fs::read(&marker).unwrap();
    let transaction = installation_sibling(&binary, "upgrade-install-transaction.json");

    let mut command = managed_daemon(&temp, &release, &binary);
    command
        .env("CTX_DAEMON_AUTOSTART_OFF", "1")
        .env("CTX_UPGRADE_PAUSE_AFTER_QUIESCENCE_FOR_TESTS", &pause);
    let child = spawn_persistent_daemon(&command);
    wait_for(
        "automatic upgrade quiescence before live daemon disablement",
        Duration::from_secs(30),
        || pause.exists(),
    );

    fs::write(
        root.join("config.toml"),
        "[indexing]\nmode = \"manual\"\n\n[upgrade]\nauto = \"apply\"\n",
    )
    .unwrap();
    fs::write(pause.with_extension("continue"), b"continue\n").unwrap();

    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let state: Value =
        serde_json::from_slice(&fs::read(scheduler_state_path(&binary)).unwrap()).unwrap();
    assert_eq!(state["status"], "disabled", "{state:#}");
    assert!(state["next_check_unix_s"].as_u64().is_some(), "{state:#}");
    assert_eq!(fs::read(&binary).unwrap(), binary_before);
    assert_eq!(fs::read(&marker).unwrap(), marker_before);
    assert!(!transaction.exists());
}

#[test]
fn live_channel_change_before_apply_prevents_old_channel_publication() {
    let temp = tempdir();
    let mut release = fake_release(&temp, FIXTURE_TARGET_VERSION);
    let binary = managed_hook_candidate(&temp, "ia_channel_change_before_apply");
    patch_release_artifact_with_next_ctx(&mut release, &binary, FIXTURE_TARGET_VERSION);
    configure_automatic_upgrades(&temp, "auto");
    let root = data_root(&temp);
    let pause = temp.path().join("channel-change-before-apply");
    let binary_before = fs::read(&binary).unwrap();
    let marker = install_marker_path(&binary);
    let marker_before = fs::read(&marker).unwrap();
    let transaction = installation_sibling(&binary, "upgrade-install-transaction.json");

    let mut command = managed_daemon(&temp, &release, &binary);
    command
        .env("CTX_DAEMON_AUTOSTART_OFF", "1")
        .env("CTX_UPGRADE_PAUSE_AFTER_QUIESCENCE_FOR_TESTS", &pause);
    let child = spawn_persistent_daemon(&command);
    wait_for(
        "stable-channel automatic upgrade staging",
        Duration::from_secs(30),
        || pause.exists(),
    );

    let staged: Value =
        serde_json::from_slice(&fs::read(scheduler_state_path(&binary)).unwrap()).unwrap();
    assert_eq!(staged["status"], "staged", "{staged:#}");
    assert_eq!(staged["channel"], "stable", "{staged:#}");
    fs::write(
        root.join("config.toml"),
        "[indexing]\nmode = \"auto\"\n\n[upgrade]\nauto = \"apply\"\nchannel = \"beta\"\n",
    )
    .unwrap();
    fs::write(pause.with_extension("continue"), b"continue\n").unwrap();

    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let state: Value =
        serde_json::from_slice(&fs::read(scheduler_state_path(&binary)).unwrap()).unwrap();
    assert!(
        matches!(state["status"].as_str(), Some("disabled" | "error")),
        "{state:#}"
    );
    assert_eq!(state["channel"], "stable", "{state:#}");
    assert_eq!(fs::read(&binary).unwrap(), binary_before);
    assert_eq!(fs::read(&marker).unwrap(), marker_before);
    assert!(!transaction.exists());
}
