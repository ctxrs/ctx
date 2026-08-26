use super::*;

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
