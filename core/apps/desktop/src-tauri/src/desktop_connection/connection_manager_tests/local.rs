use super::*;

#[test]
#[cfg(unix)]
fn disconnect_does_not_stop_attached_compatible_local_daemon_pid() {
    let pid = spawn_detached_sleep_pid();
    assert!(
        pid_is_alive(pid),
        "sleep process should be alive before disconnect"
    );

    let manager = ConnectionManager::default();
    manager.set_local_attached(
        "http://127.0.0.1:65531".to_string(),
        "token".to_string(),
        Some(pid),
        LocalConnectionSource::ExistingCompatibleDaemon,
    );
    manager.disconnect();

    assert!(
        pid_is_alive(pid),
        "attached compatible local daemon pid {pid} must not be terminated on disconnect"
    );
    let _ = Command::new("kill")
        .arg("-KILL")
        .arg(pid.to_string())
        .output();
}

#[test]
#[cfg(unix)]
fn disconnect_does_not_signal_attached_compatible_local_daemon() {
    let term_marker =
        std::env::temp_dir().join(format!("ctx-daemon-term-marker-{}", uuid::Uuid::new_v4()));
    let mut child = spawn_term_trap_child(&term_marker);
    let pid = child.id();
    assert!(
        pid_is_alive(pid),
        "term trap child should be alive before disconnect"
    );

    let manager = ConnectionManager::default();
    manager.set_local_attached(
        "http://127.0.0.1:65532".to_string(),
        "token".to_string(),
        Some(pid),
        LocalConnectionSource::ExistingCompatibleDaemon,
    );
    manager.disconnect();

    assert!(
        pid_is_alive(pid),
        "attached compatible local daemon pid {pid} must remain alive after disconnect"
    );
    assert!(
        !wait_for_file(&term_marker, Duration::from_millis(250)),
        "attached compatible local daemon should not receive TERM on disconnect"
    );
    let _ = Command::new("kill")
        .arg("-KILL")
        .arg(pid.to_string())
        .output();
    let _ = child.wait();
    std::fs::remove_file(&term_marker).ok();
}

#[test]
#[cfg(unix)]
fn disconnect_does_not_stop_env_override_local_daemon_pid() {
    let pid = spawn_detached_sleep_pid();
    assert!(
        pid_is_alive(pid),
        "sleep process should be alive before disconnect"
    );

    let manager = ConnectionManager::default();
    manager.set_local_attached(
        "http://127.0.0.1:65530".to_string(),
        "token".to_string(),
        Some(pid),
        LocalConnectionSource::EnvOverride,
    );
    manager.disconnect();

    assert!(
        pid_is_alive(pid),
        "env override local daemon pid {pid} must not be terminated by disconnect"
    );
    let _ = Command::new("kill")
        .arg("-KILL")
        .arg(pid.to_string())
        .output();
}

#[test]
#[cfg(unix)]
fn replacing_env_override_local_connection_leaves_previous_pid_running() {
    let previous_pid = spawn_detached_sleep_pid();
    assert!(
        pid_is_alive(previous_pid),
        "previous pid should start alive"
    );

    let manager = ConnectionManager::default();
    manager.set_local_attached(
        "http://127.0.0.1:65527".to_string(),
        "token".to_string(),
        Some(previous_pid),
        LocalConnectionSource::EnvOverride,
    );
    manager.set_local_attached(
        "http://127.0.0.1:65526".to_string(),
        "token".to_string(),
        None,
        LocalConnectionSource::EnvOverride,
    );

    assert!(
        pid_is_alive(previous_pid),
        "replacing an env override pid {previous_pid} must not terminate it"
    );
    let _ = Command::new("kill")
        .arg("-KILL")
        .arg(previous_pid.to_string())
        .output();
}

#[test]
#[cfg(unix)]
fn replacing_local_connection_stops_previous_child() {
    let previous = spawn_tokio_sleep_child();
    let previous_pid = previous.id();
    assert!(
        pid_is_alive(previous_pid),
        "previous local child should start alive"
    );

    let next = spawn_tokio_sleep_child();
    let next_pid = next.id();
    assert!(
        pid_is_alive(next_pid),
        "next local child should start alive"
    );

    let manager = ConnectionManager::default();
    manager.set_local(
        "http://127.0.0.1:65525".to_string(),
        "token".to_string(),
        previous,
        false,
    );
    manager.set_local(
        "http://127.0.0.1:65524".to_string(),
        "token".to_string(),
        next,
        false,
    );

    assert!(
        wait_for_pid_exit(previous_pid, Duration::from_secs(3)),
        "replaced local child {previous_pid} should be terminated"
    );
    assert!(
        pid_is_alive(next_pid),
        "replacement local child {next_pid} should remain alive until disconnect"
    );

    manager.disconnect();
    assert!(
        wait_for_pid_exit(next_pid, Duration::from_secs(3)),
        "active replacement local child {next_pid} should be terminated on disconnect"
    );
}

#[test]
#[cfg(unix)]
fn disconnect_owned_child_local_daemon_prefers_graceful_shutdown() {
    let term_marker = std::env::temp_dir().join(format!(
        "ctx-owned-child-term-marker-{}",
        uuid::Uuid::new_v4()
    ));
    let child = spawn_term_trap_child(&term_marker);
    let pid = child.id();
    assert!(
        pid_is_alive(pid),
        "owned child term trap should be alive before disconnect"
    );

    let manager = ConnectionManager::default();
    manager.set_local(
        "http://127.0.0.1:65523".to_string(),
        "token".to_string(),
        child,
        false,
    );
    manager.disconnect();

    assert!(
        wait_for_pid_exit(pid, Duration::from_secs(3)),
        "owned child local daemon pid {pid} should exit after disconnect"
    );
    let marker = std::fs::read_to_string(&term_marker)
        .expect("owned child term marker should be written by graceful TERM handler");
    assert_eq!(marker, "term");
    std::fs::remove_file(&term_marker).ok();
}

#[test]
#[cfg(unix)]
fn reattaching_to_same_owned_local_daemon_preserves_process() {
    let child = spawn_tokio_sleep_child();
    let pid = child.id();
    assert!(pid_is_alive(pid), "local child should start alive");

    let manager = ConnectionManager::default();
    manager.set_local(
        "http://127.0.0.1:65524".to_string(),
        "token".to_string(),
        child,
        false,
    );
    manager.set_local_attached(
        "http://127.0.0.1:65524".to_string(),
        "token".to_string(),
        Some(pid),
        LocalConnectionSource::ExistingCompatibleDaemon,
    );

    assert!(
        pid_is_alive(pid),
        "same-daemon handoff must not kill the process being reattached"
    );

    manager.disconnect();
    assert!(
        wait_for_pid_exit(pid, Duration::from_secs(3)),
        "reattached owned local daemon pid {pid} should still be terminated on disconnect"
    );
}
