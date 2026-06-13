use super::*;

#[test]
fn connection_info_tracks_local_auto_bootstrap_intent() {
    let manager = ConnectionManager::default();
    let info = manager.info();
    assert!(matches!(info.kind, DesktopConnectionKind::None));
    assert!(matches!(
        info.intent,
        DesktopConnectionIntent::AutoLocalBootstrap
    ));
    assert!(info.local_auto_bootstrap_allowed);

    manager.disconnect();
    let info = manager.info();
    assert!(matches!(info.kind, DesktopConnectionKind::None));
    assert!(matches!(
        info.intent,
        DesktopConnectionIntent::ExplicitDisconnected
    ));
    assert!(!info.local_auto_bootstrap_allowed);
}

#[test]
fn explicit_remote_intent_without_active_transport_blocks_local_auto_bootstrap() {
    let manager = ConnectionManager::default();
    manager.mark_explicit_remote_intent();

    let info = manager.info();
    assert!(matches!(info.kind, DesktopConnectionKind::None));
    assert!(matches!(
        info.intent,
        DesktopConnectionIntent::ExplicitRemote
    ));
    assert!(!info.local_auto_bootstrap_allowed);
}

#[test]
#[cfg(unix)]
fn stale_auto_bootstrap_spawned_local_does_not_replace_remote_intent() {
    let child = spawn_tokio_sleep_child();
    let child_pid = child.id();
    assert!(
        pid_is_alive(child_pid),
        "auto-bootstrap child should start alive"
    );

    let manager = ConnectionManager::default();
    manager.mark_explicit_remote_intent();
    let installed = manager.set_local_auto_bootstrap(
        "http://127.0.0.1:65520".to_string(),
        "token".to_string(),
        child,
        false,
    );

    assert!(!installed, "stale auto-bootstrap local must not install");
    assert!(
        wait_for_pid_exit(child_pid, Duration::from_secs(3)),
        "rejected auto-bootstrap child {child_pid} should be cleaned up"
    );
    let info = manager.info();
    assert!(matches!(info.kind, DesktopConnectionKind::None));
    assert!(matches!(
        info.intent,
        DesktopConnectionIntent::ExplicitRemote
    ));
    assert!(!info.local_auto_bootstrap_allowed);
}

#[test]
#[cfg(unix)]
fn stale_auto_bootstrap_attached_local_does_not_replace_active_ssh() {
    let tunnel = spawn_tokio_sleep_child();
    let tunnel_pid = tunnel.id();
    assert!(pid_is_alive(tunnel_pid), "ssh tunnel should start alive");

    let manager = ConnectionManager::default();
    manager.set_ssh(
        "http://127.0.0.1:65519".to_string(),
        Some("ssh-token".to_string()),
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

    let installed = manager.set_local_attached_auto_bootstrap(
        "http://127.0.0.1:65518".to_string(),
        "local-token".to_string(),
        None,
        LocalConnectionSource::ExistingCompatibleDaemon,
    );

    assert!(
        !installed,
        "stale auto-bootstrap local must not replace ssh"
    );
    let info = manager.info();
    assert!(matches!(info.kind, DesktopConnectionKind::Ssh));
    assert_eq!(info.base_url.as_deref(), Some("http://127.0.0.1:65519"));

    manager.disconnect();
    assert!(
        wait_for_pid_exit(tunnel_pid, Duration::from_secs(3)),
        "ssh tunnel should be cleaned up during test teardown"
    );
}

#[test]
#[cfg(unix)]
fn explicit_remote_connection_disables_local_auto_bootstrap() {
    let tunnel = spawn_tokio_sleep_child();
    let tunnel_pid = tunnel.id();

    let manager = ConnectionManager::default();
    manager.set_ssh(
        "http://127.0.0.1:65521".to_string(),
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

    let info = manager.info();
    assert!(matches!(info.kind, DesktopConnectionKind::Ssh));
    assert!(matches!(
        info.intent,
        DesktopConnectionIntent::ExplicitRemote
    ));
    assert!(!info.local_auto_bootstrap_allowed);

    manager.disconnect();

    assert!(
        wait_for_pid_exit(tunnel_pid, Duration::from_secs(3)),
        "ssh tunnel should be cleaned up during test teardown"
    );
}
