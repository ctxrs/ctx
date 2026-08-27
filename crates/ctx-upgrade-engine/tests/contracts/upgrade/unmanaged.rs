#[cfg(any(unix, windows))]
use super::*;

#[cfg(any(unix, windows))]
pub(super) fn install_managed_contract_marker(binary: &Path) {
    #[cfg(unix)]
    ensure_managed_test_binary_is_bounded(binary);
    let binary = fs::canonicalize(binary).unwrap();
    #[cfg(unix)]
    let platform = test_platform_key().replace('_', "-");
    #[cfg(windows)]
    let platform = "windows-x64";
    let marker = json!({
        "schema_version": 1,
        "manager": "ctx-hosted-installer",
        "install_attempt_id": "ia_upgrade_config_contract",
        "install_path": binary.display().to_string(),
        "platform": platform,
        "channel": "stable",
        "version": env!("CARGO_PKG_VERSION"),
        "sha256": sha256_hex(&fs::read(&binary).unwrap()),
    });
    fs::write(
        hosted_install_marker_path(&binary),
        serde_json::to_vec_pretty(&marker).unwrap(),
    )
    .unwrap();
}

#[cfg(unix)]
#[test]
fn upgrade_rejects_unmanaged_install_before_network() {
    let temp = tempdir();
    let binary = managed_candidate(&temp, "ia_removed_unmanaged_marker");
    fs::remove_file(install_marker_path(&binary)).unwrap();
    let stderr = failure_stderr(
        ctx_from_binary(&temp, &binary)
            .args(["upgrade", "--dry-run"])
            .env(
                "CTX_RELEASE_METADATA_URL",
                "file:///definitely/not/a/real/ctx-release-metadata.env",
            )
            .env(
                "CTX_RELEASE_METADATA_SIGNATURE_URL",
                "file:///definitely/not/a/real/ctx-release-metadata.env.sig",
            ),
    );
    assert!(
        stderr.contains("ctx is not installed by the hosted installer"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("download release metadata"),
        "unmanaged installs should fail before metadata fetch: {stderr}"
    );
}

#[cfg(unix)]
#[test]
fn unmanaged_upgrade_enable_fails_before_config_write() {
    let temp = tempdir();
    let binary = managed_candidate(&temp, "ia_removed_unmanaged_enable_marker");
    fs::remove_file(install_marker_path(&binary)).unwrap();

    let stderr = failure_stderr(ctx_from_binary(&temp, &binary).args(["upgrade", "enable"]));

    assert!(
        stderr.contains("ctx is not installed by the hosted installer"),
        "{stderr}"
    );
    assert_safe_platform_install_action(&stderr);
    assert!(
        !data_root(&temp).join("config.toml").exists(),
        "rejected enable must not persist a fake automatic-upgrade state"
    );
}

/// The metadata environment `fake_release_env` would set, without its
/// `CTX_UPGRADE_TEST_TARGET` redirect: the running executable itself stays
/// the upgrade subject.
#[cfg(unix)]
fn unmanaged_release_env<'a>(command: &'a mut Command, release: &FakeRelease) -> &'a mut Command {
    command
        .env("CTX_RELEASE_METADATA_URL", file_url(&release.metadata))
        .env(
            "CTX_RELEASE_METADATA_SIGNATURE_URL",
            file_url(&release.signature),
        )
        .env(
            "CTX_RELEASE_METADATA_PUBLIC_KEY_PEM",
            TEST_RELEASE_PUBLIC_KEY_PEM,
        )
}

#[cfg(unix)]
fn assert_unmanaged_check_outcome(output: &Value) {
    assert_eq!(output["status"], json!("available"), "{output:#}");
    assert_eq!(output["update_available"], json!(true), "{output:#}");
    assert_eq!(output["managed"], json!(false), "{output:#}");
    assert!(
        output["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning
                .as_str()
                .unwrap_or_default()
                .contains("ctx is not installed by the hosted installer")),
        "missing unmanaged warning: {output:#}"
    );
}

#[cfg(unix)]
#[test]
fn unmanaged_read_only_installation_check_is_lock_free_and_stateless() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let _runtime = add_fake_release_runtime(&temp, &release);
    let binary = managed_candidate(&temp, "ia_readonly_unmanaged_check");
    fs::remove_file(install_marker_path(&binary)).unwrap();
    let bin_dir = binary.parent().unwrap();
    fs::set_permissions(bin_dir, fs::Permissions::from_mode(0o555)).unwrap();
    struct Restore<'a>(&'a Path);
    impl Drop for Restore<'_> {
        fn drop(&mut self) {
            let _ = fs::set_permissions(self.0, fs::Permissions::from_mode(0o755));
        }
    }
    let _restore = Restore(bin_dir);

    let output = json_output(unmanaged_release_env(
        ctx_from_binary(&temp, &binary).args(["upgrade", "check", "--format=json"]),
        &release,
    ));

    assert_unmanaged_check_outcome(&output);
    assert!(
        !installation_lock_path(&binary).exists(),
        "unmanaged check must not lock beside the executable"
    );
    assert!(
        !scheduler_state_path(&binary).exists(),
        "unmanaged check must not write scheduler state beside the executable"
    );
}

#[cfg(unix)]
#[test]
fn unmanaged_writable_installation_check_leaves_no_installation_files() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let _runtime = add_fake_release_runtime(&temp, &release);
    let binary = managed_candidate(&temp, "ia_writable_unmanaged_check");
    fs::remove_file(install_marker_path(&binary)).unwrap();

    let output = json_output(unmanaged_release_env(
        ctx_from_binary(&temp, &binary).args(["upgrade", "check", "--format=json"]),
        &release,
    ));

    assert_unmanaged_check_outcome(&output);
    assert!(
        !installation_lock_path(&binary).exists(),
        "unmanaged check must not lock beside the executable"
    );
    assert!(
        !scheduler_state_path(&binary).exists(),
        "unmanaged check must not write scheduler state beside the executable"
    );
}

#[cfg(unix)]
#[test]
fn unmanaged_read_only_installation_apply_reports_unmanaged_guidance() {
    let temp = tempdir();
    let release = fake_release(&temp, "9.9.9");
    let binary = managed_candidate(&temp, "ia_readonly_unmanaged_apply");
    fs::remove_file(install_marker_path(&binary)).unwrap();
    let bin_dir = binary.parent().unwrap();
    fs::set_permissions(bin_dir, fs::Permissions::from_mode(0o555)).unwrap();
    struct Restore<'a>(&'a Path);
    impl Drop for Restore<'_> {
        fn drop(&mut self) {
            let _ = fs::set_permissions(self.0, fs::Permissions::from_mode(0o755));
        }
    }
    let _restore = Restore(bin_dir);

    let stderr = failure_stderr(unmanaged_release_env(
        ctx_from_binary(&temp, &binary).args(["upgrade", "--dry-run"]),
        &release,
    ));

    assert!(
        stderr.contains("ctx is not installed by the hosted installer"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("download release metadata"),
        "unmanaged apply must fail before metadata fetch: {stderr}"
    );
    assert!(
        !stderr.contains("owner-safe"),
        "unmanaged apply must not fail on executable-directory safety: {stderr}"
    );
    assert!(
        !installation_lock_path(&binary).exists(),
        "unmanaged apply must not lock beside the executable"
    );
}
