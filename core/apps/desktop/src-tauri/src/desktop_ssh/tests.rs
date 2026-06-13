use super::update::{
    remote_backup_ctx_bin_cmd, remote_cleanup_backup_ctx_bin_cmd, remote_restore_ctx_bin_cmd,
    remote_stop_daemon_cmd, remote_update_backup_ctx_bin,
};
use super::*;

#[cfg(unix)]
fn pid_is_alive(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(unix)]
fn wait_for_pid_exit(pid: u32, timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if !pid_is_alive(pid) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    !pid_is_alive(pid)
}

#[cfg(unix)]
fn new_temp_test_dir(prefix: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("{prefix}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).expect("create temp dir");
    path
}

#[cfg(unix)]
fn spawn_tokio_sleep_child() -> Child {
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg("sleep 30 >/dev/null 2>&1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    command.spawn().expect("spawn tokio sleep child")
}

#[test]
fn remote_ctx_bin_values_validate() {
    let valid = validate_remote_ctx_bin("/opt/ctx/bin/ctx").expect("absolute path is valid");
    assert_eq!(valid, "/opt/ctx/bin/ctx");
    let valid_home = validate_remote_ctx_bin("~/.ctx/bin/ctx").expect("~/ path is valid");
    assert_eq!(valid_home, "~/.ctx/bin/ctx");
    let err_empty = validate_remote_ctx_bin(" ").expect_err("empty path must fail");
    assert!(err_empty.to_string().contains("remote_ctx_bin is required"));
    let err_rel = validate_remote_ctx_bin("ctx").expect_err("relative path must fail");
    assert!(err_rel
        .to_string()
        .contains("must be an absolute path or ~/ path"));
}

#[test]
fn remote_ctx_bin_parent_dir_handles_home_and_absolute_paths() {
    assert_eq!(
        remote_ctx_bin_parent_dir("~/.ctx/bin/ctx").expect("home-based path parent"),
        "~/.ctx/bin"
    );
    assert_eq!(
        remote_ctx_bin_parent_dir("/opt/ctx/bin/ctx").expect("absolute path parent"),
        "/opt/ctx/bin"
    );
}

#[test]
fn remote_daemon_exec_command_injects_system_sandbox_cli_env_only_when_ready() {
    let command = super::install::render_remote_daemon_exec_cmd(
        "~/.ctx/bin/ctx",
        44199,
        "~/.ctx",
        "~/.ctx/bundles",
        Some("canary"),
        Some("https://updates.example/functions/v1"),
    )
    .expect("render remote daemon exec command");
    assert!(command.contains("CTX_BUNDLE_DIR=\"$HOME/.ctx/bundles\""));
    assert!(command
        .contains("\"$HOME/.ctx/bin/ctx\" serve --bind 127.0.0.1:44199 --data-dir \"$HOME/.ctx\""));
    assert!(
        command.contains("CTX_HARNESS_SANDBOX_CLI_PATH"),
        "remote daemon start command should inject a system sandbox CLI path when the managed runtime is healthy: {command}"
    );
    assert!(
        command.contains("CTX_MANAGED_DAEMON_AUTO_UPDATE=1 CTX_DAEMON_UPDATE_CHANNEL='canary' CTX_DAEMON_UPDATE_BASE_URL='https://updates.example/functions/v1'"),
        "remote daemon start command should persist release source for daemon-owned update checks: {command}"
    );
    assert!(
        command.contains("'/usr/local/bin/ctx-rootful-nerdctl' info >/dev/null 2>&1"),
        "remote daemon start command should guard sandbox env behind a runtime health check: {command}"
    );
    assert!(
        !command.contains("CTX_SANDBOX_PREFETCH"),
        "remote daemon start command should not depend on legacy sandbox prefetch env: {command}"
    );
}

#[test]
fn remote_startup_prewarm_request_targets_daemon_launch_api() {
    let request = super::commands::build_remote_startup_prewarm_request();
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/api/execution/launch/start");
    assert_eq!(
        request.headers,
        vec![("Content-Type".to_string(), "application/json".to_string())]
    );
    assert_eq!(
        request.body.as_deref(),
        Some(r#"{"kind":"startup_prewarm","prewarm_scope":"all"}"#)
    );
}

#[test]
fn remote_linux_sandbox_stage_request_targets_stage_api() {
    let request = super::commands::build_remote_linux_sandbox_stage_request();
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, "/api/execution/linux_sandbox_runtime/stage");
    assert_eq!(
        request.headers,
        vec![("Content-Type".to_string(), "application/json".to_string())]
    );
    assert!(request.body.is_none());
}

#[test]
fn remote_bootstrap_planner_covers_primary_paths() {
    assert_eq!(
        plan_remote_bootstrap(RemoteBootstrapPlannerInput {
            start_remote: true,
            no_start_remote: false,
            existing_daemon_reachable: true,
            managed_binary_present: false,
        }),
        RemoteBootstrapPlan::ConnectToRunningDaemon
    );
    assert_eq!(
        plan_remote_bootstrap(RemoteBootstrapPlannerInput {
            start_remote: false,
            no_start_remote: false,
            existing_daemon_reachable: false,
            managed_binary_present: true,
        }),
        RemoteBootstrapPlan::RefuseBecauseStartRemoteDisabled
    );
    assert_eq!(
        plan_remote_bootstrap(RemoteBootstrapPlannerInput {
            start_remote: true,
            no_start_remote: false,
            existing_daemon_reachable: false,
            managed_binary_present: true,
        }),
        RemoteBootstrapPlan::StartManagedDaemon
    );
    assert_eq!(
        plan_remote_bootstrap(RemoteBootstrapPlannerInput {
            start_remote: true,
            no_start_remote: false,
            existing_daemon_reachable: false,
            managed_binary_present: false,
        }),
        RemoteBootstrapPlan::InstallManagedDaemonThenStart
    );
}

#[test]
fn ssh_connect_job_registry_tracks_phase_and_consumes_terminal_state() {
    let job_id = begin_connect_job().expect("job should start");
    record_connect_job_phase(&job_id, ConnectJobPhase::Planning);
    let pending = desktop_connect_ssh_poll(DesktopSshConnectPollReq {
        job_id: job_id.clone(),
        consume: false,
    })
    .expect("pending snapshot");
    assert_eq!(pending.status, "pending");
    assert_eq!(pending.phase.as_deref(), Some("planning"));
    complete_connect_job_failure(&job_id, "boom".to_string());
    let failed = desktop_connect_ssh_poll(DesktopSshConnectPollReq {
        job_id: job_id.clone(),
        consume: true,
    })
    .expect("failed snapshot");
    assert_eq!(failed.status, "failed");
    assert_eq!(failed.phase.as_deref(), Some("failed"));
    let missing = desktop_connect_ssh_poll(DesktopSshConnectPollReq {
        job_id,
        consume: false,
    });
    assert!(missing.is_err(), "terminal consume should remove the job");
}

#[test]
#[cfg(unix)]
fn bootstrap_failure_cleanup_kills_ephemeral_tunnel() {
    let child = Command::new("sh")
        .arg("-c")
        .arg("sleep 60")
        .spawn()
        .expect("spawn ssh tunnel fixture");
    let pid = child.id();
    assert!(
        pid_is_alive(pid),
        "ephemeral tunnel fixture should start alive"
    );

    let tunnel = TunnelHandle::from_child_for_test(45123, child);
    let err = super::connect::cleanup_ephemeral_tunnel_on_error::<()>(tunnel, anyhow!("boom"))
        .expect_err("cleanup should return the original failure");

    assert!(err.to_string().contains("boom"));
    assert!(
        wait_for_pid_exit(pid, std::time::Duration::from_secs(3)),
        "ephemeral tunnel pid {pid} should be terminated on bootstrap failure"
    );
}

#[test]
#[cfg(unix)]
fn ssh_tunnel_health_probe_with_auth_validates_protected_route() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind test listener");
    let addr = listener.local_addr().expect("listener addr");
    let observed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let observed_server = std::sync::Arc::clone(&observed);
    let server = std::thread::spawn(move || {
        let health_body =
            "{\"pid\":1,\"data_root\":\"/tmp/test\",\"compatibility\":{\"desktop_exact_version\":\"1.0.0\",\"desktop_build_id\":\"build-a\",\"desktop_dev_instance_id\":\"dev\"}}";
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().expect("accept request");
            let mut buf = [0_u8; 2048];
            let size = std::io::Read::read(&mut stream, &mut buf).expect("read request");
            let request = String::from_utf8_lossy(&buf[..size]).to_string();
            let request_lower = request.to_ascii_lowercase();
            observed_server
                .lock()
                .expect("lock observed requests")
                .push(request.clone());
            let (status_line, body) = if request.starts_with("GET /api/health ") {
                ("HTTP/1.1 200 OK", health_body)
            } else if request.starts_with("GET /api/workspaces ")
                && request_lower.contains("authorization: bearer remote-token")
            {
                ("HTTP/1.1 200 OK", "[]")
            } else {
                ("HTTP/1.1 401 Unauthorized", "{\"error\":\"unauthorized\"}")
            };
            let response = format!(
                "{status_line}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body,
            );
            std::io::Write::write_all(&mut stream, response.as_bytes()).expect("write response");
        }
    });

    let child = spawn_tokio_sleep_child();
    let mut tunnel = TunnelHandle::from_child_for_test(addr.port(), child);
    let base_url = format!("http://{}", addr);
    tunnel
        .probe_health_with_retry(&base_url, Some("remote-token"))
        .expect("auth-aware health probe succeeds");
    tunnel.kill().expect("kill test tunnel");

    server.join().expect("join test server");
    let requests = observed.lock().expect("lock observed requests");
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with("GET /api/health "));
    assert!(requests[1].starts_with("GET /api/workspaces "));
    assert!(requests[1]
        .to_ascii_lowercase()
        .contains("authorization: bearer remote-token"));
}

#[test]
fn windows_detection_helpers_match_expected_tokens() {
    assert!(parse_remote_platform_probe_stdout(
        "welcome\n__CTX_PLATFORM_OS__Linux\n__CTX_PLATFORM_ARCH__x86_64\n"
    )
    .is_some());
}

#[test]
fn ssh_auth_failure_detection_matches_permission_denied_errors() {
    assert!(looks_like_ssh_auth_failure(
        "ssh failed to probe remote platform: Permission denied (publickey,password)."
    ));
    assert!(!looks_like_ssh_auth_failure(
        "ssh: connect to host devbox.example port 22: Operation timed out"
    ));
}

#[test]
fn ssh_bootstrap_authorized_keys_command_is_idempotent() {
    let cmd = ssh_authorized_keys_install_command();
    assert!(cmd.contains("grep -qxF \"$key\" \"$HOME/.ssh/authorized_keys\" ||"));
}

#[test]
fn reap_password_once_child_after_input_error_reports_remote_output() {
    #[cfg(windows)]
    let mut cmd = {
        let mut c = Command::new("cmd");
        c.arg("/C").arg("echo remote-ssh-failed 1>&2 & exit /b 19");
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = Command::new("sh");
        c.arg("-lc").arg("echo remote-ssh-failed >&2; exit 19");
        c
    };
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = cmd.spawn().expect("spawn failure fixture");
    let err = reap_password_once_child_after_input_error(child, "stdin write failed");
    let msg = err.to_string();
    assert!(msg.contains("stdin write failed"));
    assert!(msg.contains("password-once ssh exited with status"));
    assert!(msg.contains("remote-ssh-failed"));
}

#[test]
fn ssh_config_override_normalization() {
    assert_eq!(
        normalized_ssh_config_override(Some(" /tmp/ctx-fixture-ssh-config ")),
        Some("/tmp/ctx-fixture-ssh-config".to_string())
    );
    assert_eq!(normalized_ssh_config_override(Some("   ")), None);
}

#[test]
fn ssh_identity_file_parser_extracts_and_normalizes_paths() {
    let expanded = "host fixture\nidentityfile ~/.ssh/fixture_key\nidentityfile /tmp/ctx\\ fixture/id_ed25519\n";
    let identities = parse_ssh_identity_files_from_expanded_config(expanded);
    assert_eq!(identities.len(), 2);
    assert_eq!(
        identities[0],
        expand_tilde("~/.ssh/fixture_key").expect("home expansion should succeed")
    );
    assert_eq!(identities[1], PathBuf::from("/tmp/ctx fixture/id_ed25519"));
}

#[test]
fn ssh_identity_path_helpers_strip_and_append_pub_suffix() {
    let public_identity = PathBuf::from("/tmp/ctx-fixture/id_ed25519.pub");
    let private_identity = private_key_path_for_identity(&public_identity);
    assert_eq!(
        private_identity,
        PathBuf::from("/tmp/ctx-fixture/id_ed25519")
    );
    assert_eq!(
        public_key_path_for_private_key(&private_identity),
        public_identity
    );
}

#[test]
fn remote_path_helpers_round_trip() {
    assert_eq!(split_remote_path(""), ("~".to_string(), String::new()));
    assert_eq!(join_remote_path("~", "repo"), "~/repo".to_string());
    assert_eq!(join_remote_path("/", "repo"), "/repo".to_string());
}

#[test]
fn update_channel_validation() {
    assert_eq!(
        super::model::normalize_update_channel_with_env(None, None).expect("default"),
        "stable"
    );
    assert_eq!(
        super::model::normalize_update_channel_with_sources(None, None, Some("canary"))
            .expect("preference"),
        "canary"
    );
    assert_eq!(
        super::model::normalize_update_channel_with_sources(None, None, None).expect("default"),
        "stable"
    );
    assert_eq!(
        super::model::normalize_update_channel_with_sources(Some("e2e"), None, Some("canary"))
            .expect("explicit"),
        "e2e"
    );
    assert_eq!(
        super::model::normalize_update_channel_with_sources(None, Some("canary"), Some("e2e"))
            .expect("env"),
        "canary"
    );
    assert_eq!(
        super::model::normalize_update_channel_with_sources(Some("beta"), Some("canary"), None)
            .expect("explicit"),
        "beta"
    );
    assert!(
        super::model::normalize_update_channel_with_sources(Some("bad channel"), None, None)
            .is_err()
    );
    assert!(super::model::normalize_update_channel_with_sources(Some("."), None, None).is_err());
    assert!(super::model::normalize_update_channel_with_sources(Some(".."), None, None).is_err());
    assert!(super::model::normalize_update_channel_with_sources(
        Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        None,
        None
    )
    .is_err());
}

#[test]
fn remote_update_reuses_existing_managed_binary_when_recorded_active_is_missing() {
    let decision = super::update::resolve_remote_update_target_ctx_bin(
        Some("/tmp/custom/ctx".to_string()),
        MANAGED_REMOTE_CTX_BIN,
        false,
        true,
    );
    assert_eq!(decision.ctx_bin, MANAGED_REMOTE_CTX_BIN);
    assert!(!decision.install_managed);
}

#[test]
fn remote_update_backup_path_is_stable_and_adjacent_to_binary() {
    let backup = remote_update_backup_ctx_bin("/opt/ctx/bin/ctx").expect("backup path");
    assert_eq!(backup, "/opt/ctx/bin/ctx.pre-update-backup");
}

#[test]
fn remote_update_backup_and_restore_commands_use_copy_not_rename() {
    let backup_cmd =
        remote_backup_ctx_bin_cmd("/opt/ctx/bin/ctx", "/opt/ctx/bin/ctx.pre-update-backup");
    assert!(backup_cmd.contains("cp '/opt/ctx/bin/ctx' '/opt/ctx/bin/ctx.pre-update-backup'"));
    assert!(backup_cmd.contains("chmod 755"));

    let restore_cmd =
        remote_restore_ctx_bin_cmd("/opt/ctx/bin/ctx", "/opt/ctx/bin/ctx.pre-update-backup");
    assert!(restore_cmd.contains("cp '/opt/ctx/bin/ctx.pre-update-backup' '/opt/ctx/bin/ctx'"));
    assert!(restore_cmd.contains("backup missing"));
}

#[test]
fn remote_update_backup_cleanup_command_removes_backup_file() {
    let cmd = remote_cleanup_backup_ctx_bin_cmd("/opt/ctx/bin/ctx.pre-update-backup");
    assert_eq!(cmd, "rm -f '/opt/ctx/bin/ctx.pre-update-backup'");
}

#[test]
fn remote_stop_command_targets_expected_listener_without_fallback_patterns() {
    let cmd = remote_stop_daemon_cmd(44199, Some("/tmp/ctx-remote"), "/opt/ctx/bin/ctx");
    assert!(cmd.contains("lsof -tiTCP:44199 -sTCP:LISTEN"));
    assert!(
        cmd.contains("expected_cmd=\"$ctx_bin serve --bind 127.0.0.1:44199 --data-dir $data_dir\"")
    );
    assert!(cmd.contains("ps -p \"$pid\" -o args="));
    assert!(!cmd.contains("[["));
    assert!(!cmd.contains("pkill"));
}

#[test]
#[cfg(unix)]
fn remote_stop_command_runs_under_sh_and_kills_only_matching_listener() {
    let temp = new_temp_test_dir("ctx-remote-stop-match");
    let fakebin = temp.join("fakebin");
    std::fs::create_dir_all(&fakebin).expect("create fakebin");
    let home_dir = temp.join("home");
    std::fs::create_dir_all(&home_dir).expect("create home dir");
    let mut child = Command::new("/bin/sleep")
        .arg("30")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn sleep child");
    let child_pid = child.id();

    let lsof_path = fakebin.join("lsof");
    std::fs::write(
        &lsof_path,
        format!("#!/bin/sh\nprintf '{}\\n'\n", child_pid),
    )
    .expect("write fake lsof");
    let ps_path = fakebin.join("ps");
    std::fs::write(
        &ps_path,
        format!(
            "#!/bin/sh\nif [ \"$1\" = '-p' ] && [ \"$2\" = '{}' ]; then\n  printf '%s\\n' '{}'\n  exit 0\nfi\nexit 1\n",
            child_pid,
            format!(
                "{}/.ctx/bin/ctx serve --bind 127.0.0.1:44199 --data-dir {}/daemon",
                home_dir.display(),
                home_dir.display()
            )
        ),
    )
    .expect("write fake ps");
    for path in [&lsof_path, &ps_path] {
        let mut perms = std::fs::metadata(path).expect("metadata").permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).expect("chmod");
    }

    let cmd = remote_stop_daemon_cmd(44199, Some("~/daemon"), "~/.ctx/bin/ctx");
    let output = Command::new("/bin/sh")
        .arg("-c")
        .arg(&cmd)
        .env("PATH", format!("{}:/usr/bin:/bin", fakebin.display()))
        .env("HOME", home_dir.display().to_string())
        .output()
        .expect("run generated stop command");

    assert!(
        output.status.success(),
        "expected stop command to succeed under /bin/sh: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !child.wait().expect("wait sleep child").success(),
        "expected remote stop command to terminate pid {child_pid}"
    );
    std::fs::remove_dir_all(&temp).expect("remove temp dir");
}

#[test]
#[cfg(unix)]
fn remote_stop_command_refuses_non_matching_listener() {
    let temp = new_temp_test_dir("ctx-remote-stop-mismatch");
    let fakebin = temp.join("fakebin");
    std::fs::create_dir_all(&fakebin).expect("create fakebin");
    let home_dir = temp.join("home");
    std::fs::create_dir_all(&home_dir).expect("create home dir");
    let mut child = Command::new("/bin/sleep")
        .arg("30")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn sleep child");
    let child_pid = child.id();

    let lsof_path = fakebin.join("lsof");
    std::fs::write(
        &lsof_path,
        format!("#!/bin/sh\nprintf '{}\\n'\n", child_pid),
    )
    .expect("write fake lsof");
    let ps_path = fakebin.join("ps");
    std::fs::write(
        &ps_path,
        format!(
            "#!/bin/sh\nif [ \"$1\" = '-p' ] && [ \"$2\" = '{}' ]; then\n  printf '%s\\n' '/usr/bin/ctx serve --bind 127.0.0.1:44199 --data-dir /tmp/other'\n  exit 0\nfi\nexit 1\n",
            child_pid
        ),
    )
    .expect("write fake ps");
    for path in [&lsof_path, &ps_path] {
        let mut perms = std::fs::metadata(path).expect("metadata").permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).expect("chmod");
    }

    let cmd = remote_stop_daemon_cmd(44199, Some("~/daemon"), "~/.ctx/bin/ctx");
    let output = Command::new("/bin/sh")
        .arg("-c")
        .arg(&cmd)
        .env("PATH", format!("{}:/usr/bin:/bin", fakebin.display()))
        .env("HOME", home_dir.display().to_string())
        .output()
        .expect("run generated stop command");

    assert!(!output.status.success(), "mismatched listener should fail");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("remote daemon stop refused"),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        pid_is_alive(child_pid),
        "mismatched listener should not terminate pid {child_pid}"
    );
    child.kill().expect("kill sleep child");
    let _ = child.wait().expect("wait sleep child");
    std::fs::remove_dir_all(&temp).expect("remove temp dir");
}

#[test]
#[cfg(unix)]
fn ssh_handoff_replaces_local_connection_without_pre_disconnect() {
    let local_child = spawn_tokio_sleep_child();
    let local_pid = local_child.id();
    assert!(
        pid_is_alive(local_pid),
        "owned local child should start alive"
    );

    let manager = std::sync::Arc::new(ConnectionManager::default());
    manager.set_local(
        "http://127.0.0.1:65521".to_string(),
        "token".to_string(),
        local_child,
        false,
    );
    assert!(matches!(manager.info().kind, DesktopConnectionKind::Local));

    let ssh_tunnel = spawn_tokio_sleep_child();
    let ssh_pid = ssh_tunnel.id();
    assert!(pid_is_alive(ssh_pid), "ssh tunnel child should start alive");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("build current-thread runtime");
    let result = runtime.block_on(manager.set_ssh_with_blocking_cleanup(
        "http://127.0.0.1:65522".to_string(),
        Some("remote-token".to_string()),
        ssh_tunnel,
        "fixture.example".to_string(),
        Some("ctxfixture".to_string()),
        44199,
        Some("/tmp/ctx-remote-sandbox".to_string()),
        SshRuntimeMetadata {
            managed_ctx_bin: "~/.ctx/bin/ctx".to_string(),
            active_ctx_bin: Some("~/.ctx/bin/ctx".to_string()),
            ssh_password_once: None,
            admin_password_once: None,
        },
    ));

    assert!(
        result.is_ok(),
        "ssh handoff should succeed without tearing down the existing connection first: {result:?}"
    );
    assert!(matches!(manager.info().kind, DesktopConnectionKind::Ssh));
    assert_eq!(
        manager.info().remote_data_dir.as_deref(),
        Some("/tmp/ctx-remote-sandbox")
    );
    assert!(
        wait_for_pid_exit(local_pid, std::time::Duration::from_secs(3)),
        "previous local child {local_pid} should be reclaimed after ssh handoff"
    );
    assert!(
        pid_is_alive(ssh_pid),
        "ssh tunnel child should remain active after handoff"
    );

    manager.disconnect();
    assert!(
        wait_for_pid_exit(ssh_pid, std::time::Duration::from_secs(3)),
        "ssh tunnel child {ssh_pid} should be reclaimed on disconnect"
    );
}
