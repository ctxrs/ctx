use super::*;

#[test]
fn remote_daemon_env_prefix_checks_ready_marker() {
    let prefix = remote_linux_sandbox_daemon_env_prefix("~/.ctx");
    assert!(prefix.contains("CTX_HARNESS_SANDBOX_CLI_PATH"));
    assert!(prefix.contains("info >/dev/null 2>&1"));
    assert!(prefix.contains(ROOTFUL_WRAPPER_PATH));
}

#[test]
fn parse_status_json_reads_expected_shape() {
    let status = parse_status_json(
        r#"{"state":"downloaded_not_activated","supported":true,"message":"","distro":"ubuntu"}"#,
    )
    .expect("status should parse");
    assert_eq!(status.state, "downloaded_not_activated");
    assert!(status.message.is_empty());
}

#[test]
fn wait_for_local_stage_completion_returns_failed_status_immediately() {
    let temp = std::env::temp_dir().join(format!("ctx-linux-sandbox-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp).expect("create temp dir");
    write_local_status_override(
        &temp,
        "failed",
        "local linux sandbox bootstrap stage failed: checksum mismatch",
    )
    .expect("write failed status");

    let status = wait_for_local_stage_completion(&temp).expect("failed status should return");
    assert_eq!(status.state, "failed");
    assert!(status.message.contains("checksum mismatch"));
    std::fs::remove_dir_all(&temp).expect("remove temp dir");
}

#[test]
fn select_remote_admin_password_prefers_fresh_admin_entry() {
    let selected = select_remote_admin_password(
        Some("fresh-admin"),
        Some("cached-admin"),
        Some("ssh-password"),
    );
    assert_eq!(selected, Some("fresh-admin"));
}

#[test]
fn select_remote_admin_password_falls_back_to_ssh_password() {
    let selected = select_remote_admin_password(None, None, Some("ssh-password"));
    assert_eq!(selected, Some("ssh-password"));
}

#[test]
fn missing_remote_linux_sandbox_endpoint_detected_from_404() {
    assert!(remote_daemon_missing_linux_sandbox_endpoint(
        &DesktopHttpResponse {
            status: 404,
            body: String::new(),
            content_type: None,
        }
    ));
    assert!(!remote_daemon_missing_linux_sandbox_endpoint(
        &DesktopHttpResponse {
            status: 200,
            body: String::new(),
            content_type: None,
        }
    ));
}

#[test]
fn missing_remote_linux_sandbox_endpoint_requires_explicit_update() {
    let err = remote_stage_status_response(DesktopHttpResponse {
        status: 404,
        body: String::new(),
        content_type: None,
    })
    .expect_err("404 stage response should require explicit remote update");
    assert!(err.to_string().contains(
            "remote daemon does not support sandbox preparation yet. Update or install the remote daemon explicitly, then reconnect"
        ));
}

#[test]
fn posix_safe_username_rejects_shell_metacharacters() {
    assert!(is_posix_safe_username("ctx-user_01"));
    assert!(!is_posix_safe_username("ctx user"));
    assert!(!is_posix_safe_username("ctx$(rm -rf /)"));
}

#[test]
fn local_activation_executes_bootstrap_payload_with_bash() {
    let args = activation_args(Path::new("/tmp/ctx-data"), "ctx-user");
    assert_eq!(args[0], "bash");
    assert_eq!(args[1], "-s");
    assert_eq!(args[2], "--");
    assert_eq!(args[3], "activate");
}

#[test]
fn persisting_remote_admin_password_keeps_latest_runtime_paths() {
    let updated = runtime_with_persisted_remote_admin_password(
        SshRuntimeMetadata {
            managed_ctx_bin: "~/.ctx/bin/ctx-managed".to_string(),
            active_ctx_bin: Some("~/.ctx/bin/ctx-active".to_string()),
            ssh_password_once: None,
            admin_password_once: None,
        },
        "admin-secret",
    );
    assert_eq!(updated.managed_ctx_bin, "~/.ctx/bin/ctx-managed");
    assert_eq!(
        updated.active_ctx_bin.as_deref(),
        Some("~/.ctx/bin/ctx-active")
    );
    assert_eq!(updated.admin_password_once.as_deref(), Some("admin-secret"));
}

#[test]
fn bootstrap_script_embeds_expected_runtime_markers() {
    assert!(BOOTSTRAP_SCRIPT.contains("allowed_gid="));
    assert!(BOOTSTRAP_SCRIPT.contains("local exec_user="));
    assert!(BOOTSTRAP_SCRIPT.contains("exec --user \"\\${exec_user}\""));
    assert!(BOOTSTRAP_SCRIPT.contains("CTX_CONTAINER_TERMINAL_USER"));
    assert!(BOOTSTRAP_SCRIPT.contains("iptables -P OUTPUT DROP"));
}
