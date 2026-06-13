use super::*;
use std::os::unix::process::ExitStatusExt;

#[test]
fn parse_os_release_value_trims_quotes() {
    let contents = "ID=\"ubuntu\"\nID_LIKE=debian ubuntu\n";
    assert_eq!(
        parse_os_release_value(contents, "ID=").as_deref(),
        Some("ubuntu")
    );
    assert_eq!(
        parse_os_release_value(contents, "ID_LIKE=").as_deref(),
        Some("debian ubuntu")
    );
}

#[test]
fn normalize_bootstrap_state_maps_manual_runtime_required() {
    assert_eq!(
        normalize_bootstrap_state("manual_runtime_required"),
        LinuxSandboxRuntimeState::Unsupported
    );
    assert_eq!(
        normalize_bootstrap_state("downloading"),
        LinuxSandboxRuntimeState::DownloadPending
    );
}

#[test]
fn sudo_needs_password_detects_wrong_password_attempts() {
    let output = std::process::Output {
        status: std::process::ExitStatus::from_raw(1 << 8),
        stdout: Vec::new(),
        stderr: b"sudo: 1 incorrect password attempt".to_vec(),
    };
    assert!(sudo_needs_password(&output));
}

#[test]
fn posix_safe_username_rejects_shell_metacharacters() {
    assert!(is_posix_safe_username("ctx-user_01"));
    assert!(!is_posix_safe_username("ctx user"));
    assert!(!is_posix_safe_username("ctx$(rm -rf /)"));
}

#[test]
fn activation_args_execute_bootstrap_payload_with_bash() {
    let args = activation_args(Path::new("/tmp/ctx-data"), "ctx-user");
    assert_eq!(args[0], "bash");
    assert_eq!(args[1], "-s");
    assert_eq!(args[2], "--");
    assert_eq!(args[3], "activate");
}

#[test]
fn bootstrap_wrapper_forces_container_processes_to_run_as_allowed_uid_gid() {
    assert!(BOOTSTRAP_SCRIPT.contains("allowed_gid="));
    assert!(BOOTSTRAP_SCRIPT.contains("local exec_user="));
    assert!(BOOTSTRAP_SCRIPT.contains("exec --user \"\\${exec_user}\""));
    assert!(BOOTSTRAP_SCRIPT.contains("local args=(-d --user"));
    assert!(BOOTSTRAP_SCRIPT.contains("is_allowed_user_value"));
    assert!(BOOTSTRAP_SCRIPT.contains("is_root_user_value"));
    assert!(BOOTSTRAP_SCRIPT.contains("CTX_CONTAINER_TERMINAL_USER"));
    assert!(BOOTSTRAP_SCRIPT.contains("iptables -P OUTPUT DROP"));
}

#[test]
fn bootstrap_wrapper_restricts_root_materialization_to_workspace_paths() {
    assert!(BOOTSTRAP_SCRIPT.contains("is_materialization_workspace_path()"));
    assert!(BOOTSTRAP_SCRIPT.contains("/ctx/ws|/ctx/ws/worktrees|/ctx/ws/worktrees/*"));
    assert!(BOOTSTRAP_SCRIPT
        .contains("[[ \"\\${2:-}\" == \"\\${allowed_uid}:\\${allowed_gid}\" ]] || return 1"));
    assert!(BOOTSTRAP_SCRIPT.contains("is_materialization_workspace_path \"\\${4:-}\" || return 1"));
    assert!(BOOTSTRAP_SCRIPT.contains("is_materialization_workspace_path \"\\${5:-}\" || return 1"));
}

#[test]
fn bootstrap_script_prefers_verified_staged_debs_before_network_refresh() {
    let install_idx = BOOTSTRAP_SCRIPT
        .find("if [[ ${#verified_debs[@]} -eq ${#required_packages[@]} ]]; then")
        .expect("verified deb branch should exist");
    let update_idx = BOOTSTRAP_SCRIPT
        .rfind("apt-get update")
        .expect("apt-get update should remain available for fallback installs");
    assert!(
        install_idx < update_idx,
        "activation should try verified staged debs before refreshing apt metadata"
    );
}

#[test]
fn bootstrap_script_preserves_existing_containerd_provider() {
    assert!(BOOTSTRAP_SCRIPT.contains("resolve_cni_plugin_source_dir()"));
    assert!(BOOTSTRAP_SCRIPT.contains("containerd_provider_available()"));
    assert!(BOOTSTRAP_SCRIPT.contains("systemctl cat containerd.service"));
    assert!(BOOTSTRAP_SCRIPT.contains("if ! containerd_provider_available; then"));
    assert!(BOOTSTRAP_SCRIPT.contains("if ! resolve_cni_plugin_source_dir >/dev/null 2>&1; then"));
    assert!(BOOTSTRAP_SCRIPT.contains("required_packages+=(containerd)"));
    assert!(BOOTSTRAP_SCRIPT.contains("required_packages+=(containernetworking-plugins)"));
    assert!(BOOTSTRAP_SCRIPT.contains("apt-get install -y \"${required_packages[@]}\""));
    assert!(!BOOTSTRAP_SCRIPT.contains("for package in containerd containernetworking-plugins; do"));
    assert!(!BOOTSTRAP_SCRIPT.contains("apt-get install -y containerd containernetworking-plugins"));
}

#[test]
fn bootstrap_script_promotes_runtime_archive_only_after_checksum() {
    assert!(BOOTSTRAP_SCRIPT.contains("acquire_nerdctl_download_lock"));
    assert!(BOOTSTRAP_SCRIPT.contains("local partial=\"${dest}.partial.$$\""));
    assert!(BOOTSTRAP_SCRIPT.contains("verify_nerdctl_checksum \"${arch}\" \"${partial}\""));
    assert!(BOOTSTRAP_SCRIPT.contains("mv -f \"${partial}\" \"${dest}\""));
    assert!(BOOTSTRAP_SCRIPT.contains("Staged Linux sandbox runtime download failed verification."));
}

#[test]
fn bootstrap_sudoers_rule_avoids_requiretty_defaults() {
    assert!(!BOOTSTRAP_SCRIPT.contains("requiretty"));
    assert!(BOOTSTRAP_SCRIPT.contains("${user_name} ALL=(root) NOPASSWD: ${wrapper_path}"));
}

#[test]
fn product_message_for_failed_on_ubuntu_is_generic() {
    let platform = LinuxSandboxPlatform::UbuntuDebian {
        distro: "Ubuntu".to_string(),
    };
    let msg = platform_default_message(&platform, &LinuxSandboxRuntimeState::Failed);
    assert!(msg.contains("Preparing the Linux sandbox runtime failed on Ubuntu."));
    for leak in ["apt", "containerd", "nerdctl"] {
        assert!(
            !msg.to_ascii_lowercase().contains(leak),
            "message leaked tool detail: {leak}",
        );
    }
}

#[test]
fn product_message_for_failed_on_otherlinux_is_best_effort() {
    let platform = LinuxSandboxPlatform::OtherLinux {
        distro: "Arch".to_string(),
    };
    let msg = platform_default_message(&platform, &LinuxSandboxRuntimeState::Failed);
    assert!(msg.contains("best-effort on Arch"));
    for leak in ["apt", "containerd", "nerdctl"] {
        assert!(
            !msg.to_ascii_lowercase().contains(leak),
            "message leaked tool detail: {leak}",
        );
    }
}
