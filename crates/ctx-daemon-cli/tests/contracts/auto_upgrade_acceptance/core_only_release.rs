#[test]
fn persistent_daemon_upgrades_without_legacy_runtime_metadata() {
    let temp = tempdir();
    let mut release = fake_legacy_release(&temp, FIXTURE_TARGET_VERSION);
    let binary = managed_hook_candidate(&temp, "ia_daemon_core_only_release");
    patch_release_artifact_with_next_ctx(&mut release, &binary, FIXTURE_TARGET_VERSION);
    seed_authoritative_codex_source(temp.path());

    managed_daemon(&temp, &release, &binary)
        .env("CTX_DAEMON_AUTOSTART_OFF", "1")
        .assert()
        .success();

    let marker: Value =
        serde_json::from_slice(&fs::read(install_marker_path(&binary)).unwrap()).unwrap();
    let state: Value =
        serde_json::from_slice(&fs::read(scheduler_state_path(&binary)).unwrap()).unwrap();
    assert_eq!(marker["version"], FIXTURE_TARGET_VERSION);
    assert_eq!(state["status"], "applied");
    assert_eq!(state["attempt_source"], "automatic");
    assert!(!data_root(&temp).join("runtime").exists());
}
