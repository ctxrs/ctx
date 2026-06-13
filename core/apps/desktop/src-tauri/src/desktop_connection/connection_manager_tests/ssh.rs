use super::*;

#[test]
#[cfg(unix)]
fn ssh_connection_info_exposes_and_clears_remote_update_state() {
    let tunnel = spawn_tokio_sleep_child();
    let tunnel_pid = tunnel.id();

    let manager = ConnectionManager::default();
    manager.set_ssh(
        "http://127.0.0.1:65522".to_string(),
        Some("token".to_string()),
        tunnel,
        "example.test".to_string(),
        Some("dev".to_string()),
        22,
        Some("/tmp/ctx".to_string()),
        SshRuntimeMetadata {
            managed_ctx_bin: "~/.ctx/bin/ctx".to_string(),
            active_ctx_bin: Some("~/.ctx/bin/ctx".to_string()),
            ssh_password_once: None,
            admin_password_once: None,
        },
    );

    manager
        .set_ssh_remote_update_state(
            ctx_desktop_ipc::DesktopRemoteDaemonUpdateState::Pending,
            Some("waiting for idle".to_string()),
        )
        .expect("set pending state");
    let info = manager.info();
    assert_eq!(
        info.remote_update_state,
        Some(ctx_desktop_ipc::DesktopRemoteDaemonUpdateState::Pending)
    );
    assert_eq!(
        info.remote_update_message.as_deref(),
        Some("waiting for idle")
    );

    manager
        .set_ssh_remote_update_state(
            ctx_desktop_ipc::DesktopRemoteDaemonUpdateState::Failed,
            Some("failed".to_string()),
        )
        .expect("set failed state");
    let info = manager.info();
    assert_eq!(
        info.remote_update_state,
        Some(ctx_desktop_ipc::DesktopRemoteDaemonUpdateState::Failed)
    );
    assert_eq!(info.remote_update_message.as_deref(), Some("failed"));

    manager
        .clear_ssh_remote_update_state()
        .expect("clear remote update state");
    let info = manager.info();
    assert_eq!(info.remote_update_state, None);
    assert_eq!(info.remote_update_message, None);

    manager.disconnect();
    assert!(
        wait_for_pid_exit(tunnel_pid, Duration::from_secs(3)),
        "ssh tunnel should be cleaned up during test teardown"
    );
}

#[test]
#[cfg(unix)]
fn replacing_ssh_connection_stops_previous_tunnel() {
    let previous = spawn_tokio_sleep_child();
    let previous_pid = previous.id();
    assert!(
        pid_is_alive(previous_pid),
        "previous ssh tunnel should start alive"
    );

    let next = spawn_tokio_sleep_child();
    let next_pid = next.id();
    assert!(pid_is_alive(next_pid), "next ssh tunnel should start alive");

    let manager = ConnectionManager::default();
    manager.set_ssh(
        "http://127.0.0.1:65523".to_string(),
        Some("token".to_string()),
        previous,
        "example.test".to_string(),
        Some("dev".to_string()),
        22,
        Some("/tmp/ctx".to_string()),
        SshRuntimeMetadata {
            managed_ctx_bin: "~/.ctx/bin/ctx".to_string(),
            active_ctx_bin: Some("~/.ctx/bin/ctx".to_string()),
            ssh_password_once: None,
            admin_password_once: None,
        },
    );
    manager.set_ssh(
        "http://127.0.0.1:65522".to_string(),
        Some("token".to_string()),
        next,
        "example.test".to_string(),
        Some("dev".to_string()),
        22,
        Some("/tmp/ctx".to_string()),
        SshRuntimeMetadata {
            managed_ctx_bin: "~/.ctx/bin/ctx".to_string(),
            active_ctx_bin: Some("~/.ctx/bin/ctx".to_string()),
            ssh_password_once: None,
            admin_password_once: None,
        },
    );

    assert!(
        wait_for_pid_exit(previous_pid, Duration::from_secs(3)),
        "replaced ssh tunnel {previous_pid} should be terminated"
    );
    assert!(
        pid_is_alive(next_pid),
        "replacement ssh tunnel {next_pid} should remain alive until disconnect"
    );

    manager.disconnect();
    assert!(
        wait_for_pid_exit(next_pid, Duration::from_secs(3)),
        "active replacement ssh tunnel {next_pid} should be terminated on disconnect"
    );
}

#[test]
#[cfg(unix)]
fn replace_with_ssh_defers_previous_tunnel_cleanup_to_caller() {
    let previous = spawn_tokio_sleep_child();
    let previous_pid = previous.id();
    assert!(
        pid_is_alive(previous_pid),
        "previous ssh tunnel should start alive"
    );

    let next = spawn_tokio_sleep_child();
    let next_pid = next.id();
    assert!(pid_is_alive(next_pid), "next ssh tunnel should start alive");

    let manager = ConnectionManager::default();
    manager.set_ssh(
        "http://127.0.0.1:65523".to_string(),
        Some("token".to_string()),
        previous,
        "example.test".to_string(),
        Some("dev".to_string()),
        22,
        Some("/tmp/ctx".to_string()),
        SshRuntimeMetadata {
            managed_ctx_bin: "~/.ctx/bin/ctx".to_string(),
            active_ctx_bin: Some("~/.ctx/bin/ctx".to_string()),
            ssh_password_once: None,
            admin_password_once: None,
        },
    );

    let displaced = manager.replace_with_ssh(
        "http://127.0.0.1:65522".to_string(),
        Some("token".to_string()),
        next,
        "example.test".to_string(),
        Some("dev".to_string()),
        22,
        Some("/tmp/ctx".to_string()),
        SshRuntimeMetadata {
            managed_ctx_bin: "~/.ctx/bin/ctx".to_string(),
            active_ctx_bin: Some("~/.ctx/bin/ctx".to_string()),
            ssh_password_once: None,
            admin_password_once: None,
        },
    );

    assert!(displaced.is_some(), "previous tunnel should be returned");
    assert!(
        pid_is_alive(previous_pid),
        "replace_with_ssh should not eagerly terminate the displaced tunnel"
    );
    assert!(
        pid_is_alive(next_pid),
        "replacement tunnel should remain alive after the swap"
    );

    cleanup_active_connection(displaced.expect("previous connection should exist"));
    assert!(
        wait_for_pid_exit(previous_pid, Duration::from_secs(3)),
        "caller cleanup should terminate displaced tunnel {previous_pid}"
    );

    manager.disconnect();
    assert!(
        wait_for_pid_exit(next_pid, Duration::from_secs(3)),
        "active replacement ssh tunnel {next_pid} should be terminated on disconnect"
    );
}
