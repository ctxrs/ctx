use super::*;

fn connection_info_with_kind(kind: DesktopConnectionKind) -> DesktopConnectionInfo {
    DesktopConnectionInfo {
        base_url: None,
        intent: DesktopConnectionIntent::AutoLocalBootstrap,
        kind,
        local_auto_bootstrap_allowed: true,
        host: None,
        remote_data_dir: None,
        remote_port: None,
        remote_update_message: None,
        remote_update_state: None,
        browser_query_secret: None,
        token: None,
        user: None,
    }
}

#[test]
fn explicit_local_ensure_replaces_remote_but_not_existing_local() {
    assert!(ensure_mode_needs_connect(
        EnsureLocalConnectionMode::ExplicitLocal,
        &connection_info_with_kind(DesktopConnectionKind::None)
    ));
    assert!(ensure_mode_needs_connect(
        EnsureLocalConnectionMode::ExplicitLocal,
        &connection_info_with_kind(DesktopConnectionKind::Ssh)
    ));
    assert!(!ensure_mode_needs_connect(
        EnsureLocalConnectionMode::ExplicitLocal,
        &connection_info_with_kind(DesktopConnectionKind::Local)
    ));
}

#[test]
fn auto_bootstrap_ensure_only_runs_when_transport_is_missing() {
    assert!(ensure_mode_needs_connect(
        EnsureLocalConnectionMode::AutoBootstrap,
        &connection_info_with_kind(DesktopConnectionKind::None)
    ));
    assert!(!ensure_mode_needs_connect(
        EnsureLocalConnectionMode::AutoBootstrap,
        &connection_info_with_kind(DesktopConnectionKind::Local)
    ));
    assert!(!ensure_mode_needs_connect(
        EnsureLocalConnectionMode::AutoBootstrap,
        &connection_info_with_kind(DesktopConnectionKind::Ssh)
    ));
}

#[test]
fn local_connection_stale_when_health_probe_fails() {
    let info = DesktopConnectionInfo {
        kind: DesktopConnectionKind::Local,
        base_url: Some("http://127.0.0.1:43535".to_string()),
        intent: DesktopConnectionIntent::AutoLocalBootstrap,
        local_auto_bootstrap_allowed: true,
        browser_query_secret: None,
        token: Some("token".to_string()),
        host: None,
        user: None,
        remote_port: None,
        remote_data_dir: None,
        remote_update_message: None,
        remote_update_state: None,
    };
    assert!(current_local_connection_stale(&info, |_url, token| {
        assert_eq!(token, Some("token"));
        Err(anyhow!("connection refused"))
    }));
    assert!(!current_local_connection_stale(&info, |_url, token| {
        assert_eq!(token, Some("token"));
        Ok(())
    }));
}

#[test]
fn local_connection_without_base_url_is_treated_as_stale() {
    let info = DesktopConnectionInfo {
        kind: DesktopConnectionKind::Local,
        base_url: None,
        intent: DesktopConnectionIntent::AutoLocalBootstrap,
        local_auto_bootstrap_allowed: true,
        browser_query_secret: None,
        token: Some("token".to_string()),
        host: None,
        user: None,
        remote_port: None,
        remote_data_dir: None,
        remote_update_message: None,
        remote_update_state: None,
    };
    assert!(current_local_connection_stale(&info, |_url, _token| Ok(())));
}

#[cfg(unix)]
fn spawn_detached_sleep_pid() -> u32 {
    let output = Command::new("sh")
        .arg("-c")
        .arg("sleep 30 >/dev/null 2>&1 & echo $!")
        .output()
        .expect("spawn detached sleep");
    assert!(
        output.status.success(),
        "detached sleep spawn failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u32>()
        .expect("parse detached sleep pid")
}

#[cfg(unix)]
fn pid_is_alive(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[cfg(unix)]
fn wait_for_pid_exit(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !pid_is_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(80));
    }
    !pid_is_alive(pid)
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
#[cfg(unix)]
fn apply_validated_local_connection_preserves_spawned_shutdown_token() {
    let state = ConnectionManager::default();
    let child = spawn_tokio_sleep_child();
    let child_pid = child.id();

    let info = apply_validated_local_connection(
        &state,
        Ok(SpawnedLocalDaemonReady {
            url: "http://127.0.0.1:4313".to_string(),
            token: "daemon-token".to_string(),
            local_shutdown_token: "shutdown-token".to_string(),
            child,
            systemd_scope: false,
        }),
        None,
    )
    .expect("validated spawned daemon should install local connection");

    assert!(matches!(info.kind, DesktopConnectionKind::Local));
    assert_eq!(
        state.local_shutdown_token_for_scope(DEFAULT_CONNECTION_SCOPE),
        Some("shutdown-token".to_string())
    );

    state.disconnect();
    assert!(
        wait_for_pid_exit(child_pid, Duration::from_secs(3)),
        "spawned local daemon child {child_pid} should terminate during test teardown"
    );
}

#[test]
#[cfg(unix)]
fn desktop_restart_local_daemon_spawn_failure_preserves_env_override_local_connection() {
    let state = ConnectionManager::default();
    let daemon_pid = spawn_detached_sleep_pid();
    assert!(
        pid_is_alive(daemon_pid),
        "attached daemon should start alive before restart attempt"
    );
    state.set_local_attached(
        "http://127.0.0.1:4315".to_string(),
        "attached-token".to_string(),
        Some(daemon_pid),
        LocalConnectionSource::EnvOverride,
    );

    let err = restart_local_with_spawn(&state, || Err(anyhow!("daemon lockfile is held")))
        .expect_err("spawn failure should not disconnect an env override local daemon");
    assert!(format!("{err:#}").contains("daemon lockfile is held"));

    let info = state.info();
    assert!(matches!(info.kind, DesktopConnectionKind::Local));
    assert_eq!(info.base_url.as_deref(), Some("http://127.0.0.1:4315"));
    assert_eq!(info.token.as_deref(), Some("attached-token"));
    assert!(
        pid_is_alive(daemon_pid),
        "env override daemon pid {daemon_pid} must remain alive after restart failure"
    );

    let _ = Command::new("kill")
        .arg("-KILL")
        .arg(daemon_pid.to_string())
        .output();
}

#[test]
fn desktop_restart_local_daemon_uses_local_connect_gate() {
    let first_guard = lock_local_connect_gate().expect("lock first gate holder");
    let ready = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let spawn_entered = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let ready_for_thread = std::sync::Arc::clone(&ready);
    let spawn_entered_for_thread = std::sync::Arc::clone(&spawn_entered);
    let handle = std::thread::spawn(move || {
        let state = ConnectionManager::default();
        ready_for_thread.store(true, std::sync::atomic::Ordering::SeqCst);
        let err = restart_local_with_spawn(&state, || {
            spawn_entered_for_thread.store(true, std::sync::atomic::Ordering::SeqCst);
            Err(anyhow!("restart reached spawn"))
        })
        .expect_err("spawn failure should surface after the restart acquires the gate");
        assert!(format!("{err:#}").contains("restart reached spawn"));
    });

    while !ready.load(std::sync::atomic::Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(80));
    assert!(
        !spawn_entered.load(std::sync::atomic::Ordering::SeqCst),
        "restart spawn should not start while the local-connect gate is held"
    );

    drop(first_guard);
    handle.join().expect("join restart waiter");
    assert!(
        spawn_entered.load(std::sync::atomic::Ordering::SeqCst),
        "restart spawn should proceed once the local-connect gate is released"
    );
}

#[test]
#[cfg(unix)]
fn desktop_restart_local_daemon_does_not_stop_attached_compatible_daemon() {
    let state = ConnectionManager::default();
    let previous_pid = spawn_detached_sleep_pid();
    assert!(
        pid_is_alive(previous_pid),
        "reattached compatible daemon should start alive before restart"
    );
    state.set_local_attached(
        "http://127.0.0.1:4316".to_string(),
        "compatible-token".to_string(),
        Some(previous_pid),
        LocalConnectionSource::ExistingCompatibleDaemon,
    );

    let replacement = spawn_tokio_sleep_child();
    let replacement_pid = replacement.id();
    assert!(
        pid_is_alive(replacement_pid),
        "replacement child should start alive before installation"
    );

    let info = restart_local_with_spawn(&state, || {
        assert!(
            pid_is_alive(previous_pid),
            "attached compatible daemon pid {previous_pid} must remain alive when replacement spawn begins"
        );
        Ok(SpawnedLocalDaemonReady {
            url: "http://127.0.0.1:4317".to_string(),
            token: "replacement-token".to_string(),
            local_shutdown_token: "replacement-shutdown-token".to_string(),
            child: replacement,
            systemd_scope: false,
        })
    })
    .expect("restart should replace a reattached compatible local daemon");

    assert!(matches!(info.kind, DesktopConnectionKind::Local));
    assert_eq!(info.base_url.as_deref(), Some("http://127.0.0.1:4317"));
    assert_eq!(info.token.as_deref(), Some("replacement-token"));
    assert_eq!(
        state.local_shutdown_token_for_scope(DEFAULT_CONNECTION_SCOPE),
        Some("replacement-shutdown-token".to_string())
    );
    assert!(
        pid_is_alive(replacement_pid),
        "replacement child {replacement_pid} should remain active after restart"
    );
    assert!(
        pid_is_alive(previous_pid),
        "attached compatible daemon pid {previous_pid} must remain external after restart"
    );

    state.disconnect();
    assert!(
        wait_for_pid_exit(replacement_pid, Duration::from_secs(3)),
        "replacement child {replacement_pid} should terminate on disconnect"
    );
    assert!(
        pid_is_alive(previous_pid),
        "external attached compatible daemon pid {previous_pid} must not terminate on disconnect"
    );
    let _ = Command::new("kill")
        .arg("-KILL")
        .arg(previous_pid.to_string())
        .output();
}

#[test]
#[cfg(unix)]
fn desktop_restart_local_daemon_stops_owned_child_from_another_scope() {
    let state = ConnectionManager::default();
    let previous = spawn_tokio_sleep_child();
    let previous_pid = previous.id();
    assert!(
        pid_is_alive(previous_pid),
        "default-scope daemon should start alive before restart"
    );
    state.set_local(
        "http://127.0.0.1:4318".to_string(),
        "shared-token".to_string(),
        previous,
        false,
    );
    state.set_local_attached_for_scope(
        "main",
        "http://127.0.0.1:4318".to_string(),
        "shared-token".to_string(),
        Some(previous_pid),
        LocalConnectionSource::ExistingCompatibleDaemon,
    );

    let replacement = spawn_tokio_sleep_child();
    let replacement_pid = replacement.id();
    let info = restart_local_with_spawn_for_scope("main", &state, || {
        assert!(
            wait_for_pid_exit(previous_pid, Duration::from_secs(3)),
            "default-scope daemon child {previous_pid} should be stopped before replacement spawn"
        );
        Ok(SpawnedLocalDaemonReady {
            url: "http://127.0.0.1:4319".to_string(),
            token: "replacement-token".to_string(),
            local_shutdown_token: "replacement-shutdown-token".to_string(),
            child: replacement,
            systemd_scope: false,
        })
    })
    .expect("restart should stop cross-scope owned daemon before spawning replacement");

    assert!(matches!(info.kind, DesktopConnectionKind::Local));
    assert_eq!(info.base_url.as_deref(), Some("http://127.0.0.1:4319"));
    assert_eq!(info.token.as_deref(), Some("replacement-token"));
    assert_eq!(
        state.local_shutdown_token_for_scope("main"),
        Some("replacement-shutdown-token".to_string())
    );
    assert!(
        matches!(
            state.info_for_scope(DEFAULT_CONNECTION_SCOPE).kind,
            DesktopConnectionKind::None
        ),
        "default scope should not keep a stale local connection after global restart"
    );
    assert!(
        pid_is_alive(replacement_pid),
        "replacement child {replacement_pid} should remain active after restart"
    );

    state.disconnect_for_scope("main");
    assert!(
        wait_for_pid_exit(replacement_pid, Duration::from_secs(3)),
        "replacement child {replacement_pid} should terminate on disconnect"
    );
}
