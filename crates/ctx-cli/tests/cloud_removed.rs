mod support;

use support::*;

#[test]
fn cloud_subcommand_is_not_reachable() {
    let temp = tempdir();
    ctx(&temp)
        .arg("cloud")
        .assert()
        .failure()
        .stderr(predicate::str::contains("unrecognized subcommand 'cloud'"));
}

#[test]
fn removed_cloud_config_is_rejected_without_initializing_storage() {
    let temp = tempdir();
    let data_root = data_root(&temp);
    fs::create_dir_all(&data_root).unwrap();
    let config_path = data_root.join("config.toml");
    let stale_config = "[cloud]\nmode = \"local_and_cloud\"\n";
    fs::write(&config_path, stale_config).unwrap();

    ctx(&temp)
        .args(["status", "--format=json"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cloud.mode"));

    assert_eq!(fs::read_to_string(config_path).unwrap(), stale_config);
    assert!(!data_root.join("search").exists());
    assert!(!data_root.join("relational.sqlite").exists());
    assert!(!data_root.join("spool").exists());
}

#[test]
fn workspace_has_no_cloud_history_targets_or_features() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for removed in [
        "crates/ctx-cloud-client/Cargo.toml",
        "crates/ctx-captured-batch/Cargo.toml",
        "protocol/captured-batch/v2/manifest.json",
    ] {
        assert!(
            !repo_root.join(removed).exists(),
            "{removed} must stay deleted"
        );
    }

    let workspace_manifest = fs::read_to_string(repo_root.join("Cargo.toml")).unwrap();
    let cli_manifest = fs::read_to_string(repo_root.join("crates/ctx-cli/Cargo.toml")).unwrap();
    for removed in ["ctx-cloud-client", "ctx-captured-batch", "cloud-v2"] {
        assert!(
            !workspace_manifest.contains(removed) && !cli_manifest.contains(removed),
            "removed build surface is still reachable: {removed}"
        );
    }
}
